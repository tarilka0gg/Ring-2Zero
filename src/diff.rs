use crate::config::Config;
use crate::tile::{hash_tile, hash_tile_half, Tile, TileMetadata};
use crate::frame::Frame;
use rayon::prelude::*;

pub struct DiffDetector {
    prev_hashes: Vec<u64>,
    prev_prev_hashes: Vec<u64>,
    tile_metadata: Vec<TileMetadata>,
    damaged_tiles: Vec<bool>,
    // Tiles that invalidate_tiles/invalidate_cache forced a hash reset on.
    // Consulted (and cleared) once by the next detect_changes call so the
    // damage-region skip below can't bypass hash comparison for them — see
    // force_redetect_tile's doc comment for why this exists.
    force_redetect: Vec<bool>,
    // Persistent scratch buffer for the "did this tile change or get sent
    // this frame" mask, reused across frames instead of being reallocated.
    changed_mask: Vec<bool>,
    config: Config,
    frame_count: u64,
    skipped_hashes: u64,
    total_hashes: u64,
}

impl DiffDetector {
    pub fn new(config: Config) -> Self {
        Self {
            prev_hashes: Vec::new(),
            prev_prev_hashes: Vec::new(),
            tile_metadata: Vec::new(),
            damaged_tiles: Vec::new(),
            force_redetect: Vec::new(),
            changed_mask: Vec::new(),
            config,
            frame_count: 0,
            skipped_hashes: 0,
            total_hashes: 0,
        }
    }

    pub fn detect_changes(&mut self, frame: &Frame) -> (Vec<Tile>, Vec<usize>) {
        self.frame_count += 1;

        let frame_data = &frame.rgba;
        let width = frame.width;
        let height = frame.height;

        let (tile_width, tile_height, tiles_y) = self.config.calculate_tile_dimensions(width, height);
        let total_tiles = (tiles_y * self.config.tiles_x) as usize;

        let is_first_frame = self.prev_hashes.is_empty();

        if is_first_frame {
            self.prev_hashes = vec![0; total_tiles];
            self.prev_prev_hashes = vec![0; total_tiles];
            self.tile_metadata.resize(total_tiles, TileMetadata::default());
            self.damaged_tiles = vec![false; total_tiles];
            self.force_redetect = vec![false; total_tiles];
            self.changed_mask = vec![false; total_tiles];
        }

        // Перевіряємо чи є damage regions від Wayland
        let has_damage = !frame.damage_regions.is_empty();

        if self.config.debug_mode && self.frame_count % 100 == 0 {
            if has_damage {
                println!("[Damage tracking] Received {} damage regions", frame.damage_regions.len());
            } else {
                println!("[Damage tracking] No damage regions from Wayland compositor");
            }
        }

        // Створюємо набір тайлів що перетинаються з damage regions.
        // Only touched (and only needs touching) on frames that actually
        // carry damage info — damaged_tiles is never read when !has_damage,
        // so resetting it then would just be a wasted O(total_tiles) pass.
        if has_damage {
            self.damaged_tiles.iter_mut().for_each(|d| *d = false);
            for damage in &frame.damage_regions {
                // Знаходимо всі тайли що перетинаються з цим damage region
                let tile_x_start = (damage.x / tile_width).min(self.config.tiles_x - 1);
                let tile_y_start = (damage.y / tile_height).min(tiles_y - 1);

                // Use saturating arithmetic to prevent overflow
                let tile_x_end = damage.x
                    .saturating_add(damage.width)
                    .saturating_add(tile_width)
                    .saturating_sub(1)
                    .saturating_div(tile_width)
                    .min(self.config.tiles_x);

                let tile_y_end = damage.y
                    .saturating_add(damage.height)
                    .saturating_add(tile_height)
                    .saturating_sub(1)
                    .saturating_div(tile_height)
                    .min(tiles_y);

                for ty in tile_y_start..tile_y_end {
                    for tx in tile_x_start..tile_x_end {
                        self.damaged_tiles[(ty * self.config.tiles_x + tx) as usize] = true;
                    }
                }
            }
        }

        // Read-only borrows handed to the rayon closure below. Nothing in the
        // parallel section mutates self, so these are plain shared
        // references — no per-frame Vec/HashSet clones needed (the previous
        // version cloned prev_hashes/prev_prev_hashes/metadata/damaged_tiles
        // in full every frame just to satisfy the borrow checker).
        let prev_hashes_ref = &self.prev_hashes;
        let prev_prev_hashes_ref = &self.prev_prev_hashes;
        let tile_metadata_ref = &self.tile_metadata;
        let damaged_tiles_ref = &self.damaged_tiles;
        let force_redetect_ref = &self.force_redetect;
        let frame_count = self.frame_count;
        let config = &self.config;

        // Single-pass parallel loop: hash ALL tiles + detect changes + build metadata.
        // Besides the tiles actually queued for sending, this also tracks
        // tiles that changed but were held back by FPS throttling
        // (`changed_unsent`) — their hash baseline still needs to advance and
        // their change_history still needs to reflect that they changed, even
        // though nothing was sent for them this frame.
        let (new_hashes, changed_tiles, tile_indices, tile_hashes_vec, changed_unsent, stats) = (0..total_tiles)
            .into_par_iter()
            .fold(
                || (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), (0u64, 0u64, 0u64, 0u64, 0u64)),
                |(mut hashes, mut tiles, mut indices, mut half_hashes, mut changed_unsent, mut stats), i| {
                    // Damage-based skip only applies once we have a real previous
                    // frame to compare against — on the very first frame there is
                    // no prior content for the client at all, so every tile must
                    // be considered regardless of what the compositor reports as
                    // damaged. It also must not apply to a tile invalidate_tiles/
                    // invalidate_cache just force-reset: those calls zero
                    // prev_hashes[i] specifically so the next comparison sees a
                    // change, but this early return never reaches that
                    // comparison — it would reuse the freshly-zeroed hash as both
                    // "previous" and "current" and silently swallow the forced
                    // re-detection (this was the actual root cause of tiles never
                    // recovering after an ACK-loss invalidation while damage
                    // tracking was active).
                    if has_damage && !is_first_frame && !damaged_tiles_ref[i] && !force_redetect_ref[i] {
                        hashes.push((i, prev_hashes_ref[i]));
                        stats.1 += 1; // damage_skipped
                        return (hashes, tiles, indices, half_hashes, changed_unsent, stats);
                    }

                    let ty = i as u32 / config.tiles_x;
                    let tx = i as u32 % config.tiles_x;
                    let x = tx * tile_width;
                    let y = ty * tile_height;
                    let tw = if tx == config.tiles_x - 1 { width - x } else { tile_width };
                    let th = if ty == tiles_y - 1 { height - y } else { tile_height };

                    // Compute half_hash ONCE
                    let half_hash = hash_tile_half(frame_data, x, y, tw, th, width);

                    let full_hash = if !is_first_frame && half_hash == tile_metadata_ref[i].prev_half_hash {
                        // Half хеш не змінився → Zero-copy!
                        stats.0 += 1; // skipped_hashes
                        prev_hashes_ref[i]
                    } else {
                        // Half хеш змінився → повний хеш
                        hash_tile(frame_data, x, y, tw, th, width)
                    };

                    hashes.push((i, full_hash));

                    // Change detection
                    let is_changed = is_first_frame || full_hash != prev_hashes_ref[i];

                    if !is_changed {
                        return (hashes, tiles, indices, half_hashes, changed_unsent, stats);
                    }

                    // Tile changed - check if we should send it
                    let is_dynamic = !is_first_frame
                        && tile_metadata_ref[i].last_sent_frame > 0
                        && prev_prev_hashes_ref[i] != prev_hashes_ref[i]
                        && prev_hashes_ref[i] != full_hash;

                    let was_sent_as_dynamic = tile_metadata_ref[i].last_sent_as_dynamic;
                    let frames_since_last = frame_count - tile_metadata_ref[i].last_sent_frame;

                    // Розраховуємо інтервал відправки
                    let interval = if is_dynamic {
                        config.target_fps.get() / config.dynamic_tile_fps.get()
                    } else {
                        config.target_fps.get() / config.static_tile_fps.get()
                    };

                    // Перевіряємо чи треба відправляти
                    let should_send = is_first_frame
                        || (!was_sent_as_dynamic && is_dynamic)
                        || frames_since_last >= interval;

                    if should_send {
                        let quality = if is_dynamic {
                            config.webp_quality_low
                        } else {
                            config.webp_quality_high
                        };

                        // Lock-free push to thread-local vectors
                        tiles.push(Tile::new(x, y, tw, th, quality));
                        indices.push(i);
                        half_hashes.push((i, half_hash));

                        // Update thread-local stats
                        if is_dynamic {
                            stats.3 += 1; // dynamic_sent
                        } else {
                            stats.4 += 1; // static_sent
                        }
                    } else {
                        // Changed but throttled by the FPS interval: still
                        // record it so its baseline/change_history get
                        // updated below instead of being misclassified as
                        // "unchanged".
                        changed_unsent.push((i, half_hash));
                        stats.2 += 1; // skipped_by_fps
                    }

                    (hashes, tiles, indices, half_hashes, changed_unsent, stats)
                },
            )
            .reduce(
                || (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), (0u64, 0u64, 0u64, 0u64, 0u64)),
                |(mut h1, mut t1, mut i1, mut hh1, mut cu1, s1), (h2, t2, i2, hh2, cu2, s2)| {
                    h1.extend(h2);
                    t1.extend(t2);
                    i1.extend(i2);
                    hh1.extend(hh2);
                    cu1.extend(cu2);
                    (
                        h1,
                        t1,
                        i1,
                        hh1,
                        cu1,
                        (s1.0 + s2.0, s1.1 + s2.1, s1.2 + s2.2, s1.3 + s2.3, s1.4 + s2.4),
                    )
                },
            );

        // force_redetect_ref has now been read by every tile this frame
        // (the borrow ends with the .reduce() call above) — clear it so it
        // only ever bypasses the damage-skip for exactly the one frame
        // following an invalidation, not indefinitely.
        self.force_redetect.iter_mut().for_each(|f| *f = false);

        let (skipped, damage_skip, skipped_by_fps, dynamic_sent, static_sent) = stats;

        // Convert Vec<(index, hash)> to indexed array
        let mut new_hashes_array = vec![0u64; total_tiles];
        for (i, hash) in new_hashes {
            new_hashes_array[i] = hash;
        }

        self.skipped_hashes += skipped;
        self.total_hashes += total_tiles as u64;

        // Логуємо статистику кожні 100 кадрів (silent in benchmark mode)
        if self.frame_count % 100 == 0 && self.config.debug_mode {
            let skip_percent = (self.skipped_hashes as f64 / self.total_hashes as f64) * 100.0;
            let cpu_savings = skip_percent * 0.5;
            println!(
                "[Zero-copy stats] Skipped: {}/{} tiles ({:.1}%) | Est. CPU savings: {:.1}%",
                self.skipped_hashes, self.total_hashes, skip_percent, cpu_savings
            );
            if has_damage {
                println!(
                    "[Damage tracking] Skipped {} tiles outside damage regions",
                    damage_skip
                );
            }
            self.skipped_hashes = 0;
            self.total_hashes = 0;
        }

        // Sequential metadata update for tiles that were actually sent
        // (necessary for CircularBuffer which is not thread-safe)
        for (i, half_hash) in tile_hashes_vec {
            let is_dynamic = !is_first_frame
                && self.tile_metadata[i].last_sent_frame > 0
                && self.prev_prev_hashes[i] != self.prev_hashes[i]
                && self.prev_hashes[i] != new_hashes_array[i];

            let meta = &mut self.tile_metadata[i];
            meta.prev_half_hash = half_hash;
            meta.last_sent_frame = self.frame_count;
            meta.last_sent_as_dynamic = is_dynamic;
            meta.is_dynamic = is_dynamic;
            meta.unchanged_frames = 0;

            meta.change_history.push(true);

            meta.last_hash_diff = self.prev_hashes[i] ^ new_hashes_array[i];

            self.prev_prev_hashes[i] = self.prev_hashes[i];
            self.prev_hashes[i] = new_hashes_array[i];
        }

        // Metadata update for tiles that changed but were held back by FPS
        // throttling: the hash baseline still has to advance (otherwise the
        // next frame's diff is computed against a stale, multi-frame-old
        // value) and change_history has to record that they DID change.
        // last_sent_frame/last_sent_as_dynamic stay untouched since nothing
        // was actually transmitted for these tiles.
        for &(i, half_hash) in &changed_unsent {
            let meta = &mut self.tile_metadata[i];
            meta.prev_half_hash = half_hash;
            meta.unchanged_frames = 0;
            meta.change_history.push(true);
            meta.last_hash_diff = self.prev_hashes[i] ^ new_hashes_array[i];

            self.prev_prev_hashes[i] = self.prev_hashes[i];
            self.prev_hashes[i] = new_hashes_array[i];
        }

        // Increment unchanged_frames for tiles that genuinely did not change
        // this frame (i.e. excluding both sent tiles and throttled-but-changed
        // tiles). A dense Vec<bool> mask avoids the per-frame HashSet
        // allocation/hashing that a HashSet<usize> would need for a known,
        // contiguous index range — reused as a persistent scratch buffer
        // (like damaged_tiles) instead of reallocated every frame.
        self.changed_mask.iter_mut().for_each(|c| *c = false);
        for &i in &tile_indices {
            self.changed_mask[i] = true;
        }
        for &(i, _) in &changed_unsent {
            self.changed_mask[i] = true;
        }

        let changed_mask_ref = &self.changed_mask;
        let unchanged_data: Vec<(usize, u32)> = (0..total_tiles)
            .filter(|&i| !changed_mask_ref[i])
            .map(|i| (i, self.tile_metadata[i].unchanged_frames))
            .collect();

        if !unchanged_data.is_empty() {
            // Extract counters for SIMD increment
            let mut counters: Vec<u32> = unchanged_data.iter().map(|(_, c)| *c).collect();

            // SIMD batch increment
            crate::tile::increment_unchanged_counters(&mut counters);

            // Write back incremented counters
            for (idx, (tile_idx, _)) in unchanged_data.iter().enumerate() {
                self.tile_metadata[*tile_idx].unchanged_frames = counters[idx];

                // Update change history
                let meta = &mut self.tile_metadata[*tile_idx];
                meta.change_history.push(false);
            }
        }

        // Логуємо статистику адаптивного FPS (тільки в debug режимі)
        if self.frame_count % 100 == 0 && self.config.debug_mode {
            println!("[Frame {}] Changed tiles: {}", self.frame_count, changed_tiles.len());
            if skipped_by_fps > 0 {
                println!("[Adaptive FPS] Skipped {} tiles due to FPS throttling", skipped_by_fps);
            }
            if dynamic_sent > 0 || static_sent > 0 {
                println!(
                    "[Adaptive FPS] Sent: {} dynamic (32 FPS), {} static (8 FPS)",
                    dynamic_sent, static_sent
                );
            }
        }

        (changed_tiles, tile_indices)
    }

    pub fn get_metadata(&self, index: usize) -> &TileMetadata {
        &self.tile_metadata[index]
    }

    // Optimization #3: Mutable access для update кешу
    pub fn get_metadata_mut(&mut self, index: usize) -> &mut TileMetadata {
        &mut self.tile_metadata[index]
    }

    pub fn get_all_metadata(&self) -> &[TileMetadata] {
        &self.tile_metadata
    }

    pub fn get_all_metadata_mut(&mut self) -> &mut [TileMetadata] {
        &mut self.tile_metadata
    }

    pub fn get_current_hashes(&self) -> &[u64] {
        &self.prev_hashes
    }

    pub fn reset(&mut self) {
        self.prev_hashes = Vec::new();
        self.prev_prev_hashes = Vec::new();
        self.tile_metadata.clear();
        self.damaged_tiles.clear();
        self.force_redetect.clear();
        self.changed_mask.clear();
        self.frame_count = 0;
        self.skipped_hashes = 0;
        self.total_hashes = 0;
    }

    /// Resets every piece of state that must move together whenever a tile
    /// is force-marked for re-detection. All four fields below have to
    /// change in lockstep: leaving any one of them stale reproduces one of
    /// the v0.299.1 invalidation bugs (prev_half_hash alone let the
    /// zero-copy shortcut swallow the reset; force_redetect alone couldn't
    /// bypass the damage-skip; last_sent_frame alone left the FPS-throttle
    /// interval blocking the immediate resend this call exists to trigger).
    fn force_redetect_tile(&mut self, i: usize) {
        self.tile_metadata[i].prev_half_hash = 0;
        self.tile_metadata[i].last_sent_frame = 0;
        self.prev_hashes[i] = 0;
        self.prev_prev_hashes[i] = 0;
        if i < self.force_redetect.len() {
            self.force_redetect[i] = true;
        }
    }

    /// Force re-detection of specific tiles (called when ACK timeout means they were lost in transit).
    pub fn invalidate_tiles(&mut self, indices: &[usize]) {
        for &i in indices {
            if i < self.tile_metadata.len() {
                self.tile_metadata[i].cached_hash = 0;
                self.tile_metadata[i].cached_encoded = None;
                self.force_redetect_tile(i);
            }
        }
    }

    /// Force re-encoding of tiles that were sent at low quality (dynamic mode).
    /// Resets both prev_hashes entries so detect_changes classifies them as static
    /// (is_dynamic=false) → they get re-encoded at webp_quality_high on the next frame.
    pub fn invalidate_cache(&mut self) {
        for i in 0..self.tile_metadata.len() {
            self.tile_metadata[i].cached_hash = 0;
            self.tile_metadata[i].cached_encoded = None;
            if self.tile_metadata[i].last_sent_as_dynamic {
                // prev_prev == prev → is_dynamic = false → high-quality re-encode.
                self.force_redetect_tile(i);
            }
        }
    }
}
