use crate::config::Config;
use crate::tile::{hash_tile, hash_tile_half, Tile, TileMetadata};
use crate::frame::Frame;
use rayon::prelude::*;
use std::collections::HashSet;

pub struct DiffDetector {
    prev_hashes: Vec<u64>,
    prev_prev_hashes: Vec<u64>,
    tile_metadata: Vec<TileMetadata>,
    damaged_tiles: HashSet<usize>,
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
            damaged_tiles: HashSet::new(),
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

        let tile_width = width / self.config.tiles_x;
        let tile_height = tile_width * height / width;
        let tiles_y = (height + tile_height - 1) / tile_height;
        let total_tiles = (tiles_y * self.config.tiles_x) as usize;

        let is_first_frame = self.prev_hashes.is_empty();

        if is_first_frame {
            self.prev_hashes.resize(total_tiles, 0);
            self.prev_prev_hashes.resize(total_tiles, 0);
            self.tile_metadata.resize(total_tiles, TileMetadata::default());
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

        // Створюємо набір тайлів що перетинаються з damage regions
        self.damaged_tiles.clear();
        if has_damage {
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
                        self.damaged_tiles.insert((ty * self.config.tiles_x + tx) as usize);
                    }
                }
            }
        }

        // Snapshot для безпечного паралельного доступу
        let prev_half_hashes: Vec<u64> = self.tile_metadata
            .iter()
            .map(|m| m.prev_half_hash)
            .collect();

        // Single-pass parallel loop: hash ALL tiles + detect changes + build metadata
        let (new_hashes, changed_tiles, tile_indices, tile_hashes_vec, stats) = (0..total_tiles)
            .into_par_iter()
            .fold(
                || (Vec::new(), Vec::new(), Vec::new(), Vec::new(), (0u64, 0u64, 0u64, 0u64, 0u64)),
                |(mut hashes, mut tiles, mut indices, mut half_hashes, mut stats), i| {
                    // Якщо є damage tracking і тайл не в damaged_tiles - skip hashing
                    if has_damage && !self.damaged_tiles.contains(&i) {
                        hashes.push((i, self.prev_hashes[i]));
                        stats.1 += 1; // damage_skipped
                        return (hashes, tiles, indices, half_hashes, stats);
                    }

                    let ty = i as u32 / self.config.tiles_x;
                    let tx = i as u32 % self.config.tiles_x;
                    let x = tx * tile_width;
                    let y = ty * tile_height;
                    let tw = if tx == self.config.tiles_x - 1 { width - x } else { tile_width };
                    let th = if ty == tiles_y - 1 { height - y } else { tile_height };

                    // Compute half_hash ONCE
                    let half_hash = hash_tile_half(frame_data, x, y, tw, th, width);

                    let full_hash = if !is_first_frame && half_hash == prev_half_hashes[i] {
                        // Half хеш не змінився → Zero-copy!
                        stats.0 += 1; // skipped_hashes
                        self.prev_hashes[i]
                    } else {
                        // Half хеш змінився → повний хеш
                        hash_tile(frame_data, x, y, tw, th, width)
                    };

                    hashes.push((i, full_hash));

                    // Change detection
                    let is_changed = is_first_frame || full_hash != self.prev_hashes[i];

                    if !is_changed {
                        return (hashes, tiles, indices, half_hashes, stats);
                    }

                    // Tile changed - check if we should send it
                    let is_dynamic = !is_first_frame
                        && self.tile_metadata[i].last_sent_frame > 0
                        && self.prev_prev_hashes[i] != self.prev_hashes[i]
                        && self.prev_hashes[i] != full_hash;

                    let was_sent_as_dynamic = self.tile_metadata[i].last_sent_as_dynamic;
                    let frames_since_last = self.frame_count - self.tile_metadata[i].last_sent_frame;

                    // Розраховуємо інтервал відправки
                    let interval = if is_dynamic {
                        self.config.target_fps.get() / self.config.dynamic_tile_fps.get()
                    } else {
                        self.config.target_fps.get() / self.config.static_tile_fps.get()
                    };

                    // Перевіряємо чи треба відправляти
                    let should_send = is_first_frame
                        || (!was_sent_as_dynamic && is_dynamic)
                        || frames_since_last >= interval;

                    if should_send {
                        let quality = if is_dynamic {
                            self.config.webp_quality_low
                        } else {
                            self.config.webp_quality_high
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
                        stats.2 += 1; // skipped_by_fps
                    }

                    (hashes, tiles, indices, half_hashes, stats)
                },
            )
            .reduce(
                || (Vec::new(), Vec::new(), Vec::new(), Vec::new(), (0u64, 0u64, 0u64, 0u64, 0u64)),
                |(mut h1, mut t1, mut i1, mut hh1, s1), (h2, t2, i2, hh2, s2)| {
                    h1.extend(h2);
                    t1.extend(t2);
                    i1.extend(i2);
                    hh1.extend(hh2);
                    (
                        h1,
                        t1,
                        i1,
                        hh1,
                        (s1.0 + s2.0, s1.1 + s2.1, s1.2 + s2.2, s1.3 + s2.3, s1.4 + s2.4),
                    )
                },
            );

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

        // Sequential metadata update (necessary for VecDeque which is not thread-safe)
        // Reuse computed hashes from parallel phase
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

            let changes = meta.change_history.count_ones();
            meta.update_frequency = changes as f32 / meta.change_history.len().max(1) as f32;

            self.prev_prev_hashes[i] = self.prev_hashes[i];
            self.prev_hashes[i] = new_hashes_array[i];
        }

        // SIMD Batch Operation: Increment unchanged_frames for all unchanged tiles
        let unchanged_indices: Vec<usize> = (0..total_tiles)
            .filter(|i| !tile_indices.contains(i))
            .collect();

        if !unchanged_indices.is_empty() {
            // Collect counters into contiguous array for SIMD
            let mut unchanged_counters: Vec<u32> = unchanged_indices
                .iter()
                .map(|&i| self.tile_metadata[i].unchanged_frames)
                .collect();

            // SIMD batch increment
            crate::tile::increment_unchanged_counters(&mut unchanged_counters);

            // Write back
            for (idx, &i) in unchanged_indices.iter().enumerate() {
                self.tile_metadata[i].unchanged_frames = unchanged_counters[idx];

                // Update change history
                let meta = &mut self.tile_metadata[i];
                meta.change_history.push(false);

                let changes = meta.change_history.count_ones();
                meta.update_frequency = changes as f32 / meta.change_history.len().max(1) as f32;
            }
        }

        // Логуємо статистику адаптивного FPS (silent in benchmark mode)
        if self.frame_count % 100 == 0 && std::env::var("BENCHMARK_MODE").is_err() {
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
        self.prev_hashes.clear();
        self.prev_prev_hashes.clear();
        self.tile_metadata.clear();
        self.damaged_tiles.clear();
        self.frame_count = 0;
        self.skipped_hashes = 0;
        self.total_hashes = 0;
    }
}
