use xxhash_rust::xxh3::Xxh3;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

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

// Thread-local buffer для bulk hashing (уникає allocations)
thread_local! {
    static HASH_BUFFER: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::with_capacity(256 * 256 * 4));
}

// Adaptive hash function з runtime SIMD detection
pub fn hash_tile(rgba: &[u8], x: u32, y: u32, width: u32, height: u32, frame_width: u32) -> u64 {
    // Optimization #4: Bulk hashing - один update замість багатьох
    // Якщо tile повної ширини - можемо хешувати напряму
    if width == frame_width {
        let offset = (y * frame_width * 4) as usize;
        let len = (width * height * 4) as usize;
        return hash_contiguous(&rgba[offset..offset + len]);
    }

    // Інакше копіюємо в contiguous buffer
    HASH_BUFFER.with(|cell| {
        let mut buf = cell.borrow_mut();
        let tile_size = (width * height * 4) as usize;

        if buf.len() < tile_size {
            buf.resize(tile_size, 0);
        }

        // Копіюємо tile рядок за рядком
        for row in 0..height {
            let src_offset = (((y + row) * frame_width + x) * 4) as usize;
            let dst_offset = (row * width * 4) as usize;
            let len = (width * 4) as usize;
            buf[dst_offset..dst_offset + len].copy_from_slice(&rgba[src_offset..src_offset + len]);
        }

        hash_contiguous(&buf[..tile_size])
    })
}

pub fn hash_tile_half(rgba: &[u8], x: u32, y: u32, width: u32, height: u32, frame_width: u32) -> u64 {
    // Optimization #4: Bulk hashing для half hash
    HASH_BUFFER.with(|cell| {
        let mut buf = cell.borrow_mut();
        let rows = (height + 1) / 2;
        let tile_size = (width * rows * 4) as usize;

        if buf.len() < tile_size {
            buf.resize(tile_size, 0);
        }

        // Копіюємо кожен 2-й рядок
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

// Optimization #1: AVX2 хешування для contiguous data
#[inline]
fn hash_contiguous(data: &[u8]) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 GUARANTEES SSE2, so only check for AVX2
        if is_x86_feature_detected!("avx2") {
            return unsafe { hash_avx2(data) };
        }
        // SSE2 fallback (always available on x86_64)
        return unsafe { hash_sse2(data) };
    }

    // Non-x86_64 architectures (ARM, RISC-V, etc.)
    #[cfg(not(target_arch = "x86_64"))]
    hash_scalar(data)
}

// AVX2 implementation (256-bit, процесує 32 байти за раз)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(data: &[u8]) -> u64 {
    let mut acc = _mm256_setzero_si256();
    let mut seed = _mm256_set1_epi64x(0x9e3779b97f4a7c15u64 as i64);

    let chunks = data.chunks_exact(32);
    let remainder = chunks.remainder();

    for chunk in chunks {
        let v = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);
        acc = _mm256_xor_si256(acc, v);
        seed = _mm256_add_epi64(seed, _mm256_set1_epi64x(0x9e3779b97f4a7c15u64 as i64));
        acc = _mm256_xor_si256(acc, seed);
    }

    // Process remainder
    if !remainder.is_empty() {
        let mut tail = [0u8; 32];
        tail[..remainder.len()].copy_from_slice(remainder);
        let v = _mm256_loadu_si256(tail.as_ptr() as *const __m256i);
        acc = _mm256_xor_si256(acc, v);
    }

    // Horizontal XOR reduction: 256-bit → 64-bit
    let low = _mm256_extracti128_si256(acc, 0);
    let high = _mm256_extracti128_si256(acc, 1);
    let xor128 = _mm_xor_si128(low, high);

    let low64 = _mm_extract_epi64(xor128, 0) as u64;
    let high64 = _mm_extract_epi64(xor128, 1) as u64;

    low64 ^ high64 ^ (data.len() as u64)
}

// SSE2 implementation (128-bit, процесує 16 байтів за раз)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn hash_sse2(data: &[u8]) -> u64 {
    let mut acc = _mm_setzero_si128();
    let mut seed = _mm_set1_epi64x(0x9e3779b97f4a7c15u64 as i64);

    let chunks = data.chunks_exact(16);
    let remainder = chunks.remainder();

    for chunk in chunks {
        let v = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
        acc = _mm_xor_si128(acc, v);
        seed = _mm_add_epi64(seed, _mm_set1_epi64x(0x9e3779b97f4a7c15u64 as i64));
        acc = _mm_xor_si128(acc, seed);
    }

    // Process remainder
    if !remainder.is_empty() {
        let mut tail = [0u8; 16];
        tail[..remainder.len()].copy_from_slice(remainder);
        let v = _mm_loadu_si128(tail.as_ptr() as *const __m128i);
        acc = _mm_xor_si128(acc, v);
    }

    // Reduction: 128-bit → 64-bit
    let low64 = _mm_extract_epi64(acc, 0) as u64;
    let high64 = _mm_extract_epi64(acc, 1) as u64;

    low64 ^ high64 ^ (data.len() as u64)
}

// Scalar fallback (для систем без SIMD - non-x86_64 architectures)
#[cfg(not(target_arch = "x86_64"))]
fn hash_scalar(data: &[u8]) -> u64 {
    let mut hasher = Xxh3::new();
    hasher.update(data);
    hasher.digest()
}

/// SIMD Batch Operations для diff detection
#[cfg(target_arch = "x86_64")]
pub mod simd_batch {
    use core::arch::x86_64::*;

    /// Compare two arrays of u64 hashes and return indices where they differ
    /// Uses AVX2 to compare 4 hashes at once
    #[target_feature(enable = "avx2")]
    pub unsafe fn find_changed_tiles_avx2(
        prev: &[u64],
        new: &[u64],
    ) -> Vec<usize> {
        assert_eq!(prev.len(), new.len());
        let mut changed = Vec::new();

        let chunks = prev.len() / 4;
        let remainder = prev.len() % 4;

        // Process 4 hashes at once with AVX2
        for chunk_idx in 0..chunks {
            let offset = chunk_idx * 4;

            let prev_vec = _mm256_loadu_si256(prev[offset..].as_ptr() as *const __m256i);
            let new_vec = _mm256_loadu_si256(new[offset..].as_ptr() as *const __m256i);

            // Compare for equality
            let cmp = _mm256_cmpeq_epi64(prev_vec, new_vec);

            // Extract comparison mask
            let mask = _mm256_movemask_pd(_mm256_castsi256_pd(cmp));

            // Check each of 4 comparison results
            for bit in 0..4 {
                if (mask & (1 << bit)) == 0 {
                    // Not equal - hash changed
                    changed.push(offset + bit);
                }
            }
        }

        // Handle remainder (scalar fallback)
        for i in (chunks * 4)..(chunks * 4 + remainder) {
            if prev[i] != new[i] {
                changed.push(i);
            }
        }

        changed
    }

    /// Scalar fallback for find_changed_tiles
    pub fn find_changed_tiles_scalar(prev: &[u64], new: &[u64]) -> Vec<usize> {
        prev.iter()
            .zip(new.iter())
            .enumerate()
            .filter_map(|(i, (p, n))| if p != n { Some(i) } else { None })
            .collect()
    }

    /// Increment array of u32 counters by 1 using AVX2
    /// Processes 8 counters at once
    #[target_feature(enable = "avx2")]
    pub unsafe fn increment_counters_avx2(counters: &mut [u32]) {
        let one = _mm256_set1_epi32(1);

        let chunks = counters.len() / 8;
        let remainder = counters.len() % 8;

        // Process 8 counters at once
        for chunk_idx in 0..chunks {
            let offset = chunk_idx * 8;
            let ptr = counters[offset..].as_ptr() as *const __m256i;

            let vals = _mm256_loadu_si256(ptr);
            let incremented = _mm256_add_epi32(vals, one);

            let out_ptr = counters[offset..].as_mut_ptr() as *mut __m256i;
            _mm256_storeu_si256(out_ptr, incremented);
        }

        // Handle remainder (scalar)
        for i in (chunks * 8)..(chunks * 8 + remainder) {
            counters[i] += 1;
        }
    }
}

/// Public API for SIMD batch operations (with runtime detection)
pub fn find_changed_tiles(prev: &[u64], new: &[u64]) -> Vec<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { simd_batch::find_changed_tiles_avx2(prev, new) };
        }
    }

    // Scalar fallback
    #[cfg(target_arch = "x86_64")]
    return simd_batch::find_changed_tiles_scalar(prev, new);

    #[cfg(not(target_arch = "x86_64"))]
    prev.iter()
        .zip(new.iter())
        .enumerate()
        .filter_map(|(i, (p, n))| if p != n { Some(i) } else { None })
        .collect()
}

pub fn increment_unchanged_counters(counters: &mut [u32]) {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { simd_batch::increment_counters_avx2(counters) };
        }
    }

    // Scalar fallback
    for counter in counters.iter_mut() {
        *counter += 1;
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
        Self { x, y, width, height, quality }
    }

    pub fn distance_from_center(&self, center_x: u32, center_y: u32) -> i32 {
        let tile_cx = self.x + self.width / 2;
        let tile_cy = self.y + self.height / 2;
        let dx = (tile_cx as i32 - center_x as i32).abs();
        let dy = (tile_cy as i32 - center_y as i32).abs();
        dx * dx + dy * dy
    }
}

// Circular buffer для change history (замість VecDeque для кращої performance)
#[derive(Clone, Debug)]
pub struct CircularBuffer {
    data: u64,  // Bitfield для 64 frames історії (true/false = 1/0 bit)
    size: u8,   // Поточна кількість елементів
    capacity: u8, // Максимальна місткість (обмежена 64)
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
        Self::new(32)  // Default history window
    }
}

#[derive(Clone)]
pub struct TileMetadata {
    pub unchanged_frames: u32,
    pub last_sent_frame: u64,
    pub is_dynamic: bool,
    pub last_sent_as_dynamic: bool,
    pub change_history: CircularBuffer,  // Замінили VecDeque на CircularBuffer
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

impl Default for TileMetadata {
    fn default() -> Self {
        Self {
            unchanged_frames: 0,
            last_sent_frame: 0,
            is_dynamic: false,
            last_sent_as_dynamic: false,
            change_history: CircularBuffer::default(),
            last_hash_diff: 0,
            prev_half_hash: 0,
            cached_encoded: None,
            cached_hash: 0,
        }
    }
}
