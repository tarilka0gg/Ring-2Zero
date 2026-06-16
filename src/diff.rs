use crate::config::Config;
use crate::tile::{hash_tile, hash_tile_half, Tile, TileMetadata};
use crate::frame::Frame;
use rayon::prelude::*;

pub struct DiffDetector {
    prev_hashes: Vec<u64>,
    prev_prev_hashes: Vec<u64>,
    tile_metadata: Vec<TileMetadata>,
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
        let mut damaged_tiles = std::collections::HashSet::new();
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
                        damaged_tiles.insert((ty * self.config.tiles_x + tx) as usize);
                    }
                }
            }
        }

        // Snapshot для безпечного паралельного доступу
        let prev_half_hashes: Vec<u64> = self.tile_metadata
            .iter()
            .map(|m| m.prev_half_hash)
            .collect();

        let mut new_hashes = vec![0u64; total_tiles];
        let skipped_count = std::sync::atomic::AtomicU64::new(0);
        let damage_skipped = std::sync::atomic::AtomicU64::new(0);

        new_hashes
            .par_iter_mut()
            .enumerate()
            .for_each(|(i, slot)| {
                // Якщо є damage tracking і тайл не в damaged_tiles - пропускаємо хешування
                if has_damage && !damaged_tiles.contains(&i) {
                    *slot = self.prev_hashes[i];
                    damage_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                let ty = i as u32 / self.config.tiles_x;
                let tx = i as u32 % self.config.tiles_x;
                let x = tx * tile_width;
                let y = ty * tile_height;
                let tw = if tx == self.config.tiles_x - 1 { width - x } else { tile_width };
                let th = if ty == tiles_y - 1 { height - y } else { tile_height };

                // Етап 1: Half хеш (кожен 2-й рядок, 50% даних)
                let half_hash = hash_tile_half(frame_data, x, y, tw, th, width);

                if !is_first_frame && half_hash == prev_half_hashes[i] {
                    // Half хеш не змінився → Zero-copy!
                    *slot = self.prev_hashes[i];
                    skipped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                } else {
                    // Half хеш змінився → повний хеш
                    *slot = hash_tile(frame_data, x, y, tw, th, width);
                }
            });

        let skipped = skipped_count.load(std::sync::atomic::Ordering::Relaxed);
        let damage_skip = damage_skipped.load(std::sync::atomic::Ordering::Relaxed);
        self.skipped_hashes += skipped;
        self.total_hashes += total_tiles as u64;

        // Логуємо статистику кожні 100 кадрів
        if self.frame_count % 100 == 0 {
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

        // SIMD Batch Operation: Find changed tiles
        let changed_indices = if is_first_frame {
            // First frame - all tiles changed
            (0..total_tiles).collect::<Vec<usize>>()
        } else {
            crate::tile::find_changed_tiles(&self.prev_hashes, &new_hashes)
        };

        // Parallel processing of changed tiles - store hash results
        use std::sync::Mutex;

        let changed_tiles_mutex = Mutex::new(Vec::<Tile>::new());
        let tile_indices_mutex = Mutex::new(Vec::<usize>::new());
        let tile_hashes_mutex = Mutex::new(Vec::<(usize, u64)>::new()); // Store (index, hash)
        let stats_mutex = Mutex::new((0u64, 0u64, 0u64)); // (skipped, dynamic, static)

        changed_indices.par_iter().for_each(|&i| {
            let ty = i as u32 / self.config.tiles_x;
            let tx = i as u32 % self.config.tiles_x;
            let x = tx * tile_width;
            let y = ty * tile_height;
            let tw = if tx == self.config.tiles_x - 1 { width - x } else { tile_width };
            let th = if ty == tiles_y - 1 { height - y } else { tile_height };

            // Compute half_hash once
            let half_hash = hash_tile_half(frame_data, x, y, tw, th, width);

            // Read metadata (safe - different indices)
            let is_dynamic = !is_first_frame
                && self.tile_metadata[i].last_sent_frame > 0
                && self.prev_prev_hashes[i] != self.prev_hashes[i]
                && self.prev_hashes[i] != new_hashes[i];

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

                // Lock and push to results
                changed_tiles_mutex.lock().unwrap().push(Tile::new(x, y, tw, th, quality));
                tile_indices_mutex.lock().unwrap().push(i);
                tile_hashes_mutex.lock().unwrap().push((i, half_hash)); // Store hash for reuse

                // Update stats
                let mut stats = stats_mutex.lock().unwrap();
                if is_dynamic {
                    stats.1 += 1; // dynamic_sent
                } else {
                    stats.2 += 1; // static_sent
                }
            } else {
                stats_mutex.lock().unwrap().0 += 1; // skipped_by_fps
            }
        });

        // Extract results from Mutex
        let changed_tiles = changed_tiles_mutex.into_inner().unwrap();
        let tile_indices = tile_indices_mutex.into_inner().unwrap();
        let tile_hashes_map = tile_hashes_mutex.into_inner().unwrap();
        let (skipped_by_fps, dynamic_sent, static_sent) = stats_mutex.into_inner().unwrap();

        // Sequential metadata update (necessary for VecDeque which is not thread-safe)
        // Reuse computed hashes instead of recomputing
        for (i, half_hash) in tile_hashes_map {

            let is_dynamic = !is_first_frame
                && self.tile_metadata[i].last_sent_frame > 0
                && self.prev_prev_hashes[i] != self.prev_hashes[i]
                && self.prev_hashes[i] != new_hashes[i];

            let meta = &mut self.tile_metadata[i];
            meta.prev_half_hash = half_hash;
            meta.last_sent_frame = self.frame_count;
            meta.last_sent_as_dynamic = is_dynamic;
            meta.is_dynamic = is_dynamic;
            meta.unchanged_frames = 0;

            meta.change_history.push(true);

            meta.last_hash_diff = self.prev_hashes[i] ^ new_hashes[i];

            let changes = meta.change_history.count_ones();
            meta.update_frequency = changes as f32 / meta.change_history.len().max(1) as f32;

            self.prev_prev_hashes[i] = self.prev_hashes[i];
            self.prev_hashes[i] = new_hashes[i];
        }

        // SIMD Batch Operation: Increment unchanged_frames for all unchanged tiles
        let unchanged_indices: Vec<usize> = (0..total_tiles)
            .filter(|i| !changed_indices.contains(i))
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

        // Логуємо статистику адаптивного FPS
        if self.frame_count % 100 == 0 {
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
        self.frame_count = 0;
        self.skipped_hashes = 0;
        self.total_hashes = 0;
    }
}
