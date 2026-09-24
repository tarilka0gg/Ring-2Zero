#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
#[cfg(not(target_arch = "x86_64"))]
use xxhash_rust::xxh3::Xxh3;

// SIMD detection at runtime
#[derive(Clone, Copy, Debug)]
pub enum SimdLevel {
    Avx2,
    Sse2,
    Scalar,
}

impl SimdLevel {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return SimdLevel::Avx2;
            }
            if is_x86_feature_detected!("sse2") {
                return SimdLevel::Sse2;
            }
        }
        SimdLevel::Scalar
    }
}

// Thread-local buffer for bulk hashing (avoids allocations)
thread_local! {
    static HASH_BUFFER: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::with_capacity(256 * 256 * 4));
}

// Adaptive hash function with runtime SIMD detection
pub fn hash_tile(rgba: &[u8], x: u32, y: u32, width: u32, height: u32, frame_width: u32) -> u64 {
    // Optimization #4: Bulk hashing - one update instead of many
    // If tile is full width, we can hash directly
    if width == frame_width {
        let offset = (y * frame_width * 4) as usize;
        let len = (width * height * 4) as usize;
        return hash_contiguous(&rgba[offset..offset + len]);
    }

    // Otherwise copy into contiguous buffer
    HASH_BUFFER.with(|cell| {
        let mut buf = cell.borrow_mut();
        let tile_size = (width * height * 4) as usize;

        if buf.len() < tile_size {
            buf.resize(tile_size, 0);
        }

        // Copy tile row by row
        for row in 0..height {
            let src_offset = (((y + row) * frame_width + x) * 4) as usize;
            let dst_offset = (row * width * 4) as usize;
            let len = (width * 4) as usize;
            buf[dst_offset..dst_offset + len].copy_from_slice(&rgba[src_offset..src_offset + len]);
        }

        hash_contiguous(&buf[..tile_size])
    })
}

pub fn hash_tile_half(
    rgba: &[u8],
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    frame_width: u32,
) -> u64 {
    // Optimization #4: Bulk hashing for half hash
    HASH_BUFFER.with(|cell| {
        let mut buf = cell.borrow_mut();
        let rows = height.div_ceil(2);
        let tile_size = (width * rows * 4) as usize;

        if buf.len() < tile_size {
            buf.resize(tile_size, 0);
        }

        // Copy every second row
        let mut dst_row = 0;
        let mut src_row = y;
        while src_row < y + height {
            let src_offset = ((src_row * frame_width + x) * 4) as usize;
            let dst_offset = (dst_row * width * 4) as usize;
            let len = (width * 4) as usize;
            buf[dst_offset..dst_offset + len].copy_from_slice(&rgba[src_offset..src_offset + len]);
            src_row += 2;
            dst_row += 1;
        }

        hash_contiguous(&buf[..tile_size])
    })
}

// Optimization #1: AVX2 hashing for contiguous data
#[inline]
fn hash_contiguous(data: &[u8]) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 GUARANTEES SSE2, so only check for AVX2
        if is_x86_feature_detected!("avx2") {
            return unsafe { hash_avx2(data) };
        }
        // SSE2 fallback (always available on x86_64)
        unsafe { hash_sse2(data) }
    }

    // Non-x86_64 architectures (ARM, RISC-V, etc.)
    #[cfg(not(target_arch = "x86_64"))]
    hash_scalar(data)
}

// Four distinct, well-established odd mixing constants (splitmix64 /
// MurmurHash3 finalizer constants) — one per SIMD lane, instead of the same
// constant broadcast to every lane (the original implementation). Distinct
// seeds alone turned out not to be enough (see fold_lanes' doc comment for
// why) but they're kept anyway as defense in depth against other
// pathological inputs beyond the byte-uniform case fold_lanes fixes.
const SEED_LANE_0: u64 = 0x9e3779b97f4a7c15; // splitmix64
const SEED_LANE_1: u64 = 0xff51afd7ed558ccd; // MurmurHash3 finalizer c1
const SEED_LANE_2: u64 = 0xc4ceb9fe1a85ec53; // MurmurHash3 finalizer c2
const SEED_LANE_3: u64 = 0xbf58476d1ce4e5b9; // splitmix64 mix step

/// Combines two or more 64-bit accumulator lanes into one hash, and mixes
/// the result through a proper avalanche finalizer (MurmurHash3's fmix64).
///
/// A plain `lane_a ^ lane_b ^ len` (the original implementation) is
/// structurally broken: whenever the lanes end up equal — which happens for
/// any byte-uniform input (e.g. a solid-color tile), since every lane then
/// accumulates the exact same {data, seed} sequence regardless of what
/// seed constants are used — XOR cancels them to 0, leaving the hash to
/// depend only on `len`. Two different solid colors of the same tile size
/// then hash identically. Rotating each lane by a different amount before
/// XORing them together means even fully-equal lanes don't cancel (`x ^
/// rotate_left(x, 16)` is zero only for very specific values of `x`, not
/// generically), and the finalizer's multiply/shift avalanche destroys
/// whatever linear structure the rotation-XOR combination still has.
#[inline]
fn fold_lanes(lanes: &[u64], len: u64) -> u64 {
    let mut h = len;
    for (i, &lane) in lanes.iter().enumerate() {
        h ^= lane.rotate_left((i as u32) * 16 + 1);
    }
    // MurmurHash3 fmix64.
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    h
}

// AVX2 implementation (256-bit, processes 32 bytes at a time)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(data: &[u8]) -> u64 {
    let mut acc = _mm256_setzero_si256();
    let mut seed = _mm256_set_epi64x(
        SEED_LANE_3 as i64,
        SEED_LANE_2 as i64,
        SEED_LANE_1 as i64,
        SEED_LANE_0 as i64,
    );
    let seed_inc = _mm256_set_epi64x(
        SEED_LANE_3 as i64,
        SEED_LANE_2 as i64,
        SEED_LANE_1 as i64,
        SEED_LANE_0 as i64,
    );

    let (chunks, remainder) = data.as_chunks::<32>();

    for chunk in chunks {
        let v = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);
        seed = _mm256_add_epi64(seed, seed_inc);
        // Add (not XOR) v and seed together before folding into acc: XORing
        // them as two independent terms let a *constant* v (any uniform
        // buffer loads the same v every iteration) cancel itself out after
        // an even number of chunks regardless of seed, collapsing acc to a
        // value that no longer depends on the data at all — see fold_lanes'
        // doc comment, this is the actual root cause it doesn't cover on
        // its own. Adding ties each iteration's contribution to the
        // (monotonically changing) seed, so a repeated v can't cancel.
        let mixed = _mm256_add_epi64(v, seed);
        acc = _mm256_xor_si256(acc, mixed);
    }

    // Process remainder
    if !remainder.is_empty() {
        let mut tail = [0u8; 32];
        tail[..remainder.len()].copy_from_slice(remainder);
        let v = _mm256_loadu_si256(tail.as_ptr() as *const __m256i);
        let mixed = _mm256_add_epi64(v, seed);
        acc = _mm256_xor_si256(acc, mixed);
    }

    // Extract all 4 lanes individually (not pre-XORed pairwise) — fold_lanes
    // needs each one separately to rotate them apart before combining.
    let low = _mm256_extracti128_si256(acc, 0);
    let high = _mm256_extracti128_si256(acc, 1);
    let lanes = [
        _mm_extract_epi64(low, 0) as u64,
        _mm_extract_epi64(low, 1) as u64,
        _mm_extract_epi64(high, 0) as u64,
        _mm_extract_epi64(high, 1) as u64,
    ];

    fold_lanes(&lanes, data.len() as u64)
}

// SSE2 implementation (128-bit, processes 16 bytes at a time)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn hash_sse2(data: &[u8]) -> u64 {
    let mut acc = _mm_setzero_si128();
    let mut seed = _mm_set_epi64x(SEED_LANE_1 as i64, SEED_LANE_0 as i64);
    let seed_inc = _mm_set_epi64x(SEED_LANE_1 as i64, SEED_LANE_0 as i64);

    let (chunks, remainder) = data.as_chunks::<16>();

    for chunk in chunks {
        let v = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
        seed = _mm_add_epi64(seed, seed_inc);
        // See hash_avx2's comment: add (not XOR) v and seed before folding
        // into acc, so a constant v (any uniform buffer) can't cancel
        // itself out over an even number of chunks.
        let mixed = _mm_add_epi64(v, seed);
        acc = _mm_xor_si128(acc, mixed);
    }

    // Process remainder
    if !remainder.is_empty() {
        let mut tail = [0u8; 16];
        tail[..remainder.len()].copy_from_slice(remainder);
        let v = _mm_loadu_si128(tail.as_ptr() as *const __m128i);
        let mixed = _mm_add_epi64(v, seed);
        acc = _mm_xor_si128(acc, mixed);
    }

    // Reduction: 128-bit → 64-bit
    let lanes = [
        _mm_extract_epi64(acc, 0) as u64,
        _mm_extract_epi64(acc, 1) as u64,
    ];

    fold_lanes(&lanes, data.len() as u64)
}

// Scalar fallback (for systems without SIMD - non-x86_64 architectures)
#[cfg(not(target_arch = "x86_64"))]
fn hash_scalar(data: &[u8]) -> u64 {
    let mut hasher = Xxh3::new();
    hasher.update(data);
    hasher.digest()
}

/// The tile grid laid over one frame: `tiles_x` columns of `tile_width`
/// pixels and `tiles_y` rows of `tile_height`. Tiles keep the frame's aspect
/// ratio, so the frame rarely divides evenly — the last column and row
/// absorb the remainder (see [`Grid::x_span`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub tiles_x: u32,
    pub tiles_y: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub frame_width: u32,
    pub frame_height: u32,
}

impl Grid {
    /// `tiles_x` columns over a `frame_width`×`frame_height` frame. Degenerate
    /// sizes (narrower than `tiles_x`, extreme aspect ratios) are clamped to
    /// at least one pixel per tile instead of dividing by zero.
    pub fn new(tiles_x: u32, frame_width: u32, frame_height: u32) -> Self {
        let frame_width = frame_width.max(1);
        let frame_height = frame_height.max(1);
        let tiles_x = tiles_x.clamp(1, frame_width);
        let tile_width = frame_width / tiles_x;
        let tile_height = (tile_width * frame_height / frame_width).max(1);
        Self {
            tiles_x,
            tiles_y: frame_height.div_ceil(tile_height),
            tile_width,
            tile_height,
            frame_width,
            frame_height,
        }
    }

    pub fn cell_count(&self) -> usize {
        (self.tiles_x * self.tiles_y) as usize
    }

    /// Pixel range `[start, end)` covered by columns `tx_start..=tx_end`. The
    /// last column extends to the frame's right edge: with 1366 px and 20
    /// columns of 68 px, it's 74 px wide — otherwise the rightmost 6 px would
    /// belong to no tile and never update.
    pub fn x_span(&self, tx_start: u32, tx_end: u32) -> (u32, u32) {
        let end = if tx_end + 1 >= self.tiles_x {
            self.frame_width
        } else {
            (tx_end + 1) * self.tile_width
        };
        (tx_start * self.tile_width, end)
    }

    /// Row counterpart of [`x_span`](Self::x_span).
    pub fn y_span(&self, ty_start: u32, ty_end: u32) -> (u32, u32) {
        let end = if ty_end + 1 >= self.tiles_y {
            self.frame_height
        } else {
            ((ty_end + 1) * self.tile_height).min(self.frame_height)
        };
        (ty_start * self.tile_height, end)
    }

    /// Pixel rect `(x, y, width, height)` of grid cell `index` (row-major).
    pub fn cell_rect(&self, index: usize) -> (u32, u32, u32, u32) {
        let (tx, ty) = (index as u32 % self.tiles_x, index as u32 / self.tiles_x);
        let (x0, x1) = self.x_span(tx, tx);
        let (y0, y1) = self.y_span(ty, ty);
        (x0, y0, x1 - x0, y1 - y0)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub quality: f32,
}

impl Tile {
    pub fn new(x: u32, y: u32, width: u32, height: u32, quality: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            quality,
        }
    }

    pub fn distance_from_center(&self, center_x: u32, center_y: u32) -> i32 {
        let tile_cx = self.x + self.width / 2;
        let tile_cy = self.y + self.height / 2;
        let dx = (tile_cx as i32 - center_x as i32).abs();
        let dy = (tile_cy as i32 - center_y as i32).abs();
        dx * dx + dy * dy
    }

    /// Grid-cell bounds (start_tx, start_ty, end_tx, end_ty) this tile covers.
    /// For a single-cell tile start == end on both axes; a merged tile spans
    /// every cell from start to end inclusive. Single source of truth for
    /// this math — it used to be hand-copied independently in stream.rs and
    /// frame_profiler.rs, which is exactly how the post-merge indexing bugs
    /// fixed in v0.299.1 happened.
    pub fn grid_bounds(&self, grid: &Grid) -> (u32, u32, u32, u32) {
        let start_tx = self.x / grid.tile_width;
        let start_ty = self.y / grid.tile_height;
        let end_tx = ((self.x + self.width - 1) / grid.tile_width).min(grid.tiles_x - 1);
        let end_ty = ((self.y + self.height - 1) / grid.tile_height).min(grid.tiles_y - 1);
        (start_tx, start_ty, end_tx, end_ty)
    }

    /// Index of this tile's representative grid cell (its top-left corner).
    /// For a merged multi-cell tile this identifies only ONE of the cells it
    /// covers — see `grid_bounds`/`covered_indices` when every covered cell
    /// is needed (e.g. ACK-loss recovery), and `is_single_cell` before
    /// treating this index as uniquely identifying the tile's whole area
    /// (e.g. per-tile encode caching).
    pub fn representative_index(&self, grid: &Grid) -> usize {
        (self.y / grid.tile_height * grid.tiles_x + self.x / grid.tile_width) as usize
    }

    /// Whether this tile covers exactly one grid cell (i.e. wasn't merged
    /// with neighbors). Only single-cell tiles can be safely cached/recovered
    /// by their representative index alone.
    pub fn is_single_cell(&self, grid: &Grid) -> bool {
        let (stx, sty, etx, ety) = self.grid_bounds(grid);
        stx == etx && sty == ety
    }

    /// Every grid-cell index this tile covers (one entry for a single-cell
    /// tile, up to `MAX_MERGE_TILES_X * MAX_MERGE_TILES_Y` for a merged one).
    pub fn covered_indices(&self, grid: &Grid) -> Vec<usize> {
        let (stx, sty, etx, ety) = self.grid_bounds(grid);
        let mut indices = Vec::with_capacity(((etx - stx + 1) * (ety - sty + 1)) as usize);
        for ty in sty..=ety {
            for tx in stx..=etx {
                indices.push((ty * grid.tiles_x + tx) as usize);
            }
        }
        indices
    }
}

// Circular buffer for change history (instead of VecDeque for better performance)
#[derive(Clone, Debug)]
pub struct CircularBuffer {
    data: u64,    // Bitfield for 64 frames history (true/false = 1/0 bit)
    size: u8,     // Current number of elements
    capacity: u8, // Maximum capacity (limited to 64)
}

impl CircularBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: 0,
            size: 0,
            capacity: capacity.min(64) as u8,
        }
    }

    #[inline]
    pub fn push(&mut self, value: bool) {
        if self.size < self.capacity {
            self.size += 1;
        }
        // Shift left and add new bit
        self.data = (self.data << 1) | (value as u64);
        // Mask to keep only capacity bits
        if self.capacity < 64 {
            self.data &= (1u64 << self.capacity) - 1;
        }
    }

    #[inline]
    pub fn count_ones(&self) -> u32 {
        if self.size == 0 {
            return 0;
        }
        // Count bits in the used portion
        let mask = if self.size == 64 {
            u64::MAX
        } else {
            (1u64 << self.size) - 1
        };
        (self.data & mask).count_ones()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.size as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}

impl Default for CircularBuffer {
    fn default() -> Self {
        Self::new(32) // Default history window
    }
}

#[derive(Clone, Default)]
pub struct TileMetadata {
    pub unchanged_frames: u32,
    pub last_sent_frame: u64,
    pub is_dynamic: bool,
    pub last_sent_as_dynamic: bool,
    pub change_history: CircularBuffer, // Replaced VecDeque with CircularBuffer
    pub last_hash_diff: u64,
    pub prev_half_hash: u64,

    // Optimization #3: Cache encoded tile data
    pub cached_encoded: Option<Vec<u8>>,
    pub cached_hash: u64,
}

impl TileMetadata {
    /// Lazy computation of update frequency (only when needed, e.g., debug logs)
    pub fn update_frequency(&self) -> f32 {
        let changes = self.change_history.count_ones();
        changes as f32 / self.change_history.len().max(1) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grid with the given cell size and cell counts (frame = exact multiple).
    fn g(tile_width: u32, tile_height: u32, tiles_x: u32, tiles_y: u32) -> Grid {
        Grid {
            tiles_x,
            tiles_y,
            tile_width,
            tile_height,
            frame_width: tile_width * tiles_x,
            frame_height: tile_height * tiles_y,
        }
    }

    #[test]
    fn grid_matches_the_old_tile_dimensions_on_1080p() {
        let grid = Grid::new(20, 1920, 1080);
        assert_eq!(
            (grid.tile_width, grid.tile_height, grid.tiles_y),
            (96, 54, 20)
        );
        assert_eq!(grid.cell_count(), 400);
    }

    #[test]
    fn last_column_and_row_reach_the_frame_edge() {
        let grid = Grid::new(20, 1366, 768); // 68 px columns, 1360 px would leave 6 px uncovered
        assert_eq!(grid.x_span(19, 19), (1292, 1366));
        assert_eq!(grid.x_span(16, 19), (1088, 1366));
        assert_eq!(grid.x_span(0, 3), (0, 272));
        let last = grid.cell_count() - 1;
        let (x, y, w, h) = grid.cell_rect(last);
        assert_eq!((x + w, y + h), (1366, 768));
        let covered: u64 = (0..grid.cell_count())
            .map(|i| {
                let (_, _, w, h) = grid.cell_rect(i);
                u64::from(w) * u64::from(h)
            })
            .sum();
        assert_eq!(covered, 1366 * 768, "cells tile the frame exactly");
    }

    #[test]
    fn degenerate_frames_do_not_divide_by_zero() {
        for (w, h) in [(5, 5), (1, 1), (0, 0), (4000, 1), (1, 4000)] {
            let grid = Grid::new(20, w, h);
            assert!(grid.tile_width >= 1 && grid.tile_height >= 1, "{w}x{h}");
            let (x, y, cw, ch) = grid.cell_rect(grid.cell_count() - 1);
            assert_eq!((x + cw, y + ch), (w.max(1), h.max(1)), "{w}x{h}");
        }
    }

    #[test]
    fn grid_bounds_single_cell() {
        let tile = Tile::new(40, 20, 20, 10, 5.0);
        assert_eq!(tile.grid_bounds(&g(20, 10, 10, 10)), (2, 2, 2, 2));
    }

    #[test]
    fn grid_bounds_merged_region() {
        let tile = Tile::new(20, 10, 40, 20, 5.0); // 2x2 cells starting at (1,1)
        assert_eq!(tile.grid_bounds(&g(20, 10, 10, 10)), (1, 1, 2, 2));
    }

    #[test]
    fn grid_bounds_clamps_to_grid_edge() {
        let tile = Tile::new(0, 0, 1000, 1000, 5.0);
        assert_eq!(tile.grid_bounds(&g(20, 10, 5, 5)), (0, 0, 4, 4));
    }

    #[test]
    fn representative_index_is_top_left_cell() {
        let tile = Tile::new(40, 20, 20, 10, 5.0);
        assert_eq!(tile.representative_index(&g(20, 10, 10, 100)), 22); // ty=2, tx=2 -> 2*10+2
    }

    #[test]
    fn is_single_cell_true_for_unmerged_tile() {
        let tile = Tile::new(20, 10, 20, 10, 5.0);
        assert!(tile.is_single_cell(&g(20, 10, 10, 10)));
    }

    #[test]
    fn is_single_cell_false_for_merged_tile() {
        let tile = Tile::new(20, 10, 40, 20, 5.0);
        assert!(!tile.is_single_cell(&g(20, 10, 10, 10)));
    }

    #[test]
    fn covered_indices_single_cell_has_one_entry() {
        let tile = Tile::new(20, 10, 20, 10, 5.0);
        assert_eq!(tile.covered_indices(&g(20, 10, 10, 10)), vec![11]); // ty=1,tx=1 -> 1*10+1
    }

    #[test]
    fn covered_indices_matches_every_cell_in_a_merged_region() {
        let tile = Tile::new(20, 10, 40, 20, 5.0); // covers (1,1),(2,1),(1,2),(2,2)
        let mut indices = tile.covered_indices(&g(20, 10, 10, 10));
        indices.sort_unstable();
        assert_eq!(indices, vec![11, 12, 21, 22]);
    }

    #[test]
    fn covered_indices_matches_max_merge_region_size() {
        // 4x4 grid cells (the largest a single TileMerger output can span)
        let tile = Tile::new(0, 0, 80, 40, 5.0);
        assert_eq!(tile.covered_indices(&g(20, 10, 10, 10)).len(), 16);
    }

    #[test]
    fn distance_from_center_is_zero_at_center() {
        let tile = Tile::new(40, 40, 20, 20, 5.0); // tile center is (50,50)
        assert_eq!(tile.distance_from_center(50, 50), 0);
    }

    #[test]
    fn circular_buffer_tracks_count_ones_within_capacity() {
        let mut buf = CircularBuffer::new(4);
        buf.push(true);
        buf.push(false);
        buf.push(true);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.count_ones(), 2);
    }

    #[test]
    fn circular_buffer_evicts_oldest_beyond_capacity() {
        let mut buf = CircularBuffer::new(2);
        buf.push(true);
        buf.push(true);
        buf.push(false); // evicts the first `true`
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.count_ones(), 1);
    }

    #[test]
    fn circular_buffer_caps_capacity_at_64() {
        let mut buf = CircularBuffer::new(1000);
        for _ in 0..100 {
            buf.push(true);
        }
        assert_eq!(buf.len(), 64);
        assert_eq!(buf.count_ones(), 64);
    }

    #[test]
    fn tile_metadata_update_frequency_is_ratio_of_changes() {
        let mut meta = TileMetadata::default();
        meta.change_history.push(true);
        meta.change_history.push(true);
        meta.change_history.push(false);
        meta.change_history.push(false);
        assert_eq!(meta.update_frequency(), 0.5);
    }

    #[test]
    fn hash_tile_is_deterministic() {
        let (width, height) = (8u32, 4u32);
        let rgba = vec![42u8; (width * height * 4) as usize];
        assert_eq!(
            hash_tile(&rgba, 0, 0, width, height, width),
            hash_tile(&rgba, 0, 0, width, height, width)
        );
    }

    #[test]
    fn hash_tile_no_longer_collides_between_different_uniform_colors() {
        // Regression test for a real bug found while writing these tests:
        // XORing the same loaded chunk `v` into the accumulator on every
        // iteration means, for a byte-uniform buffer (every chunk loads the
        // identical value), that v's entire contribution cancels to 0 after
        // an EVEN number of chunks — regardless of what v actually was.
        // Once that happens, the accumulator no longer depends on the pixel
        // data at all, only on the (data-independent) seed sequence and
        // length — so any two solid colors of the same tile size hash
        // identically. Neither seeding SIMD lanes differently nor mixing
        // the final reduction differently fixes this on its own (both were
        // tried and both still collided) — the loop itself has to stop
        // ever accumulating a position-independent, content-only term.
        // Fixed by adding (not XOR-ing as an independent term) v and the
        // per-iteration seed together before folding into the accumulator,
        // so a repeated v can't cancel out on its own — see hash_avx2's
        // "mixed" comment.
        let (width, height) = (10u32, 10u32); // 400 bytes: 12 AVX2 chunks (even) + a 16B remainder — the exact case that used to collide
        let a = vec![0u8; (width * height * 4) as usize];
        let b = vec![255u8; (width * height * 4) as usize];
        assert_ne!(
            hash_tile(&a, 0, 0, width, height, width),
            hash_tile(&b, 0, 0, width, height, width),
        );

        // Also check a buffer size that's an exact multiple of 32 bytes —
        // no remainder to (accidentally) rescue it, so this is the more
        // dangerous version of the same bug: it would have collided on
        // every single call, not just sometimes.
        let (width2, height2) = (8u32, 8u32); // 256 bytes = 8 AVX2 chunks exactly
        let a2 = vec![0u8; (width2 * height2 * 4) as usize];
        let b2 = vec![255u8; (width2 * height2 * 4) as usize];
        assert_ne!(
            hash_tile(&a2, 0, 0, width2, height2, width2),
            hash_tile(&b2, 0, 0, width2, height2, width2),
        );
    }

    #[test]
    fn hash_tile_distinguishes_uniform_colors_across_many_sizes() {
        // Broader sweep across chunk-count parity/remainder combinations
        // (odd/even AVX2 chunks, odd/even SSE2 chunks, zero/nonzero
        // remainder) and several color pairs, on top of the two specific
        // sizes pinned above — a targeted fix for one size shouldn't be
        // trusted without checking it generalizes.
        let colors: &[(u8, u8)] = &[(0, 255), (0, 128), (1, 254), (17, 200), (127, 128)];
        for side in 1u32..=20 {
            let (width, height) = (side, side);
            for &(ca, cb) in colors {
                let a = vec![ca; (width * height * 4) as usize];
                let b = vec![cb; (width * height * 4) as usize];
                assert_ne!(
                    hash_tile(&a, 0, 0, width, height, width),
                    hash_tile(&b, 0, 0, width, height, width),
                    "collision at {width}x{height} between fill {ca} and fill {cb}"
                );
            }
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn hash_sse2_directly_no_longer_collides_between_uniform_colors() {
        // Same regression as above, but forcing the SSE2 path directly
        // (bypassing runtime AVX2 detection) since x86_64 always has SSE2
        // and this dev machine's own hashes above would otherwise only ever
        // exercise the AVX2 path.
        if !is_x86_feature_detected!("sse2") {
            return; // x86_64 guarantees this, but don't assume in CI images
        }
        let (width, height) = (10u32, 10u32); // 400 bytes: 25 SSE2 chunks exactly, no remainder
        let a = vec![0u8; (width * height * 4) as usize];
        let b = vec![255u8; (width * height * 4) as usize];
        let ha = unsafe { hash_sse2(&a) };
        let hb = unsafe { hash_sse2(&b) };
        assert_ne!(ha, hb);
    }

    #[test]
    fn hash_tile_differs_for_different_pixel_content() {
        let (width, height) = (8u32, 4u32);
        let a = vec![0u8; (width * height * 4) as usize];
        let mut b = a.clone();
        b[0] = 255;
        assert_ne!(
            hash_tile(&a, 0, 0, width, height, width),
            hash_tile(&b, 0, 0, width, height, width)
        );
    }

    #[test]
    fn hash_tile_half_is_blind_to_an_unsampled_row() {
        // hash_tile_half samples rows y, y+2, y+4, ... — a change confined
        // to row y+1 (not sampled) must not move the half-hash.
        let (width, height) = (4u32, 4u32);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let before = hash_tile_half(&rgba, 0, 0, width, height, width);
        let row1_offset = (width * 4) as usize; // start of row 1
        rgba[row1_offset] = 255;
        let after = hash_tile_half(&rgba, 0, 0, width, height, width);
        assert_eq!(before, after);
    }

    #[test]
    fn hash_tile_half_changes_when_a_sampled_row_changes() {
        let (width, height) = (4u32, 4u32);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let before = hash_tile_half(&rgba, 0, 0, width, height, width);
        rgba[0] = 255; // row 0 is always sampled
        let after = hash_tile_half(&rgba, 0, 0, width, height, width);
        assert_ne!(before, after);
    }
}
