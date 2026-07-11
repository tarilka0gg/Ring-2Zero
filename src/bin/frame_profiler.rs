/// Frame processing profiler - показує що саме займає час
/// Детальний breakdown кожної операції в pipeline

use screen_streamer::config::Config;
use screen_streamer::diff::DiffDetector;
use screen_streamer::encoder::TileMerger;
use screen_streamer::frame::Frame;
use std::time::Instant;
use std::sync::Arc;

fn generate_test_frame(width: u32, height: u32, frame_num: usize, change_pct: f32) -> Vec<u8> {
    let mut rgba = vec![100u8; (width * height * 4) as usize];

    // Simulate changes in center area
    let change_pixels = (width * height) as f32 * change_pct;
    let change_count = change_pixels as usize;

    for i in 0..change_count {
        let idx = (i * 4 + frame_num * 7) % rgba.len();
        if idx + 3 < rgba.len() {
            rgba[idx] = ((frame_num + i) % 256) as u8;
            rgba[idx + 1] = ((frame_num * 2 + i) % 256) as u8;
            rgba[idx + 2] = ((frame_num * 3 + i) % 256) as u8;
            rgba[idx + 3] = 255;
        }
    }

    rgba
}

struct FrameProfiler {
    diff_detector: DiffDetector,
    tile_merger: TileMerger,
    config: Config,
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles_y: u32,
}

#[derive(Default)]
struct TimingBreakdown {
    diff_detection_us: f64,
    tile_merging_us: f64,
    priority_calc_us: f64,
    sorting_us: f64,
    hash_collection_us: f64,
    cache_check_us: f64,
    tile_extraction_us: f64,
    webp_encoding_us: f64,
    cache_update_us: f64,
    total_us: f64,

    tiles_detected: usize,
    tiles_merged: usize,
    cache_hits: usize,
    tiles_encoded: usize,
}

impl FrameProfiler {
    fn new(config: Config, width: u32, height: u32) -> Self {
        let (tile_width, tile_height, tiles_y) = config.calculate_tile_dimensions(width, height);

        Self {
            diff_detector: DiffDetector::new(config.clone()),
            tile_merger: TileMerger::new(config.merge_gap),
            config,
            width,
            height,
            tile_width,
            tile_height,
            tiles_y,
        }
    }

    fn process_frame(&mut self, rgba: Vec<u8>) -> TimingBreakdown {
        let mut timing = TimingBreakdown::default();
        let overall_start = Instant::now();

        let frame = Frame::new(rgba, self.width, self.height, vec![]);

        // 1. Diff Detection
        let t0 = Instant::now();
        let (changed_tiles, _) = self.diff_detector.detect_changes(&frame);
        timing.diff_detection_us = t0.elapsed().as_secs_f64() * 1_000_000.0;
        timing.tiles_detected = changed_tiles.len();

        if changed_tiles.is_empty() {
            timing.total_us = overall_start.elapsed().as_secs_f64() * 1_000_000.0;
            return timing;
        }

        // 2. Tile Merging
        let t1 = Instant::now();
        let merged_tiles = self.tile_merger.merge(
            &changed_tiles,
            self.config.tiles_x,
            self.tiles_y,
            self.tile_width,
            self.tile_height,
            self.width,
            self.height,
        );
        timing.tile_merging_us = t1.elapsed().as_secs_f64() * 1_000_000.0;
        timing.tiles_merged = merged_tiles.len();

        // 3. Priority Calculation
        let t2 = Instant::now();
        let mut tiles_with_data: Vec<(_, usize, f32)> = merged_tiles
            .iter()
            .map(|tile| {
                // tile_indices is aligned to the PRE-merge changed_tiles list;
                // merge() can collapse several original tiles into fewer
                // merged ones, so indexing it by position in the (smaller)
                // merged_tiles list picks an unrelated tile whenever merging
                // actually reduced the count. Recompute the representative
                // original-grid index from the merged tile's own geometry
                // instead (same approach stream.rs uses for the real path).
                let tile_idx = (tile.y / self.tile_height * self.config.tiles_x + tile.x / self.tile_width) as usize;
                let metadata = self.diff_detector.get_metadata(tile_idx);
                let priority = calculate_priority(
                    tile,
                    metadata,
                    self.width,
                    self.height,
                    &self.config
                );
                (*tile, tile_idx, priority)
            })
            .collect();
        timing.priority_calc_us = t2.elapsed().as_secs_f64() * 1_000_000.0;

        // 4. Sorting by Priority
        let t3 = Instant::now();
        tiles_with_data.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        timing.sorting_us = t3.elapsed().as_secs_f64() * 1_000_000.0;

        let sorted_tiles: Vec<_> = tiles_with_data.iter().map(|(t, _, _)| *t).collect();
        let sorted_tile_indices: Vec<usize> = tiles_with_data.iter().map(|(_, idx, _)| *idx).collect();

        // 5. Hash Collection - КРИТИЧНО: хешуємо MERGED tiles, не оригінальні
        let t4 = Instant::now();
        let tile_hashes: Vec<u64> = sorted_tiles.iter()
            .map(|tile| {
                // Хешуємо merged tile з поточного фрейму
                screen_streamer::tile::hash_tile(
                    &frame.rgba,
                    tile.x,
                    tile.y,
                    tile.width,
                    tile.height,
                    self.width
                )
            })
            .collect();
        timing.hash_collection_us = t4.elapsed().as_secs_f64() * 1_000_000.0;

        // 6. Cache Check - порівнюємо хеш merged tile з cached_hash
        let t5 = Instant::now();
        let cached_data: Vec<Option<Vec<u8>>> = tile_hashes.iter()
            .enumerate()
            .map(|(i, &merged_hash)| {
                // Перевіряємо всі тайли у metadata чи є хтось з таким хешем
                for metadata in self.diff_detector.get_all_metadata() {
                    if metadata.cached_hash == merged_hash && metadata.cached_encoded.is_some() {
                        timing.cache_hits += 1;
                        // Cache hit - clone Vec
                        return metadata.cached_encoded.clone();
                    }
                }
                None
            })
            .collect();
        timing.cache_check_us = t5.elapsed().as_secs_f64() * 1_000_000.0;

        // 7. Tile Extraction
        let t6 = Instant::now();
        let mut tile_buffers = Vec::new();
        for (i, tile) in sorted_tiles.iter().enumerate() {
            if cached_data[i].is_some() {
                continue; // Skip cache hits
            }

            let tile_size = (tile.width * tile.height * 4) as usize;
            let mut tile_buffer = vec![0u8; tile_size];

            // Use SIMD-optimized tile extraction
            screen_streamer::tile_extract::extract_tile(
                &frame.rgba,
                &mut tile_buffer,
                tile.x,
                tile.y,
                tile.width,
                tile.height,
                self.width,
            );

            tile_buffers.push((i, tile, tile_buffer));
        }
        timing.tile_extraction_us = t6.elapsed().as_secs_f64() * 1_000_000.0;

        // 8. WebP Encoding
        let t7 = Instant::now();
        let mut encoded = vec![Vec::new(); sorted_tiles.len()];

        // Fill cache hits
        for (i, cached) in cached_data.iter().enumerate() {
            if let Some(data) = cached {
                encoded[i] = data.clone();
            }
        }

        // Encode non-cached tiles
        for (i, tile, tile_buffer) in tile_buffers {
            let webp_data = fast_webp::encode_rgba(
                &tile_buffer,
                tile.width,
                tile.height,
                fast_webp::WebpOptions {
                    quality: tile.quality,
                    ..Default::default()
                },
            ).unwrap_or_else(|e| {
                eprintln!("WebP encoding error: {:?}", e);
                Vec::new()
            });
            encoded[i] = webp_data;
            timing.tiles_encoded += 1;
        }
        timing.webp_encoding_us = t7.elapsed().as_secs_f64() * 1_000_000.0;

        // 9. Cache Update - зберігаємо хеш та encoded data для merged tiles
        let t8 = Instant::now();
        for (i, &merged_hash) in tile_hashes.iter().enumerate() {
            if !encoded[i].is_empty() {
                // Знаходимо tile_idx для збереження (використовуємо перший tile з merged group)
                if let Some(&tile_idx) = sorted_tile_indices.get(i) {
                    let tile_metadata = self.diff_detector.get_all_metadata_mut();
                    if tile_idx < tile_metadata.len() {
                        // Store encoded tile in cache
                        tile_metadata[tile_idx].cached_encoded = Some(encoded[i].clone());
                        tile_metadata[tile_idx].cached_hash = merged_hash;
                    }
                }
            }
        }
        timing.cache_update_us = t8.elapsed().as_secs_f64() * 1_000_000.0;

        timing.total_us = overall_start.elapsed().as_secs_f64() * 1_000_000.0;
        timing
    }
}

fn calculate_priority(
    tile: &screen_streamer::tile::Tile,
    metadata: &screen_streamer::tile::TileMetadata,
    width: u32,
    height: u32,
    config: &Config,
) -> f32 {
    let frequency_score = metadata.update_frequency();
    let change_speed = (metadata.last_hash_diff.count_ones() as f32) / 64.0;
    let center_x = width / 2;
    let center_y = height / 2;
    let distance = tile.distance_from_center(center_x, center_y) as f32;
    let max_distance = ((width * width + height * height) / 4) as f32;
    let center_score = 1.0 - (distance / max_distance).sqrt();

    frequency_score * config.priority_frequency_weight
        + change_speed * config.priority_speed_weight
        + center_score * config.priority_center_weight
}

impl TimingBreakdown {
    fn print(&self, frame_num: usize) {
        let total_ms = self.total_us / 1000.0;

        println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Frame #{} - Timing Breakdown ({:.2} ms total)", frame_num, total_ms);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        self.print_line("1. Diff Detection", self.diff_detection_us);
        self.print_line("2. Tile Merging", self.tile_merging_us);
        self.print_line("3. Priority Calc", self.priority_calc_us);
        self.print_line("4. Sorting", self.sorting_us);
        self.print_line("5. Hash Collection", self.hash_collection_us);
        self.print_line("6. Cache Check", self.cache_check_us);
        self.print_line("7. Tile Extraction", self.tile_extraction_us);
        self.print_line("8. WebP Encoding", self.webp_encoding_us);
        self.print_line("9. Cache Update", self.cache_update_us);

        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Stats:");
        println!("  Tiles detected:  {} → {} after merge", self.tiles_detected, self.tiles_merged);
        println!("  Cache hits:      {} ({:.1}%)",
            self.cache_hits,
            (self.cache_hits as f64 / self.tiles_merged.max(1) as f64) * 100.0);
        println!("  Tiles encoded:   {}", self.tiles_encoded);
    }

    fn print_line(&self, name: &str, us: f64) {
        let ms = us / 1000.0;
        let pct = (us / self.total_us) * 100.0;
        let bar_len = (pct / 2.0) as usize;
        let bar = "█".repeat(bar_len);
        println!("  {:<18} {:>8.2} ms  {:>5.1}%  {}", name, ms, pct, bar);
    }
}

fn main() {
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║         FRAME PROCESSING PROFILER                       ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    let config = Config::default();
    let width = 1920u32;
    let height = 1080u32;

    println!("Configuration:");
    println!("  Resolution: {}x{}", width, height);
    println!("  Tiles: {}x?", config.tiles_x);
    println!("  Merge gap: {}", config.merge_gap);
    println!("  WebP quality: {:.1} - {:.1}", config.webp_quality_low, config.webp_quality_high);

    let mut profiler = FrameProfiler::new(config, width, height);

    // Baseline frame
    let baseline = generate_test_frame(width, height, 0, 0.0);
    let _ = profiler.process_frame(baseline);

    // Test different scenarios
    let scenarios = vec![
        ("Light changes (5%)", 0.05, 10),
        ("Medium changes (20%)", 0.20, 10),
        ("Heavy changes (50%)", 0.50, 5),
    ];

    for (desc, change_pct, frames) in scenarios {
        println!("\n\n╔══════════════════════════════════════════════════════════╗");
        println!("║  Scenario: {:<46} ║", desc);
        println!("╚══════════════════════════════════════════════════════════╝");

        let mut avg = TimingBreakdown::default();

        for i in 1..=frames {
            let rgba = generate_test_frame(width, height, i, change_pct);
            let timing = profiler.process_frame(rgba);

            if i <= 3 {
                timing.print(i);
            }

            // Accumulate for average
            avg.diff_detection_us += timing.diff_detection_us;
            avg.tile_merging_us += timing.tile_merging_us;
            avg.priority_calc_us += timing.priority_calc_us;
            avg.sorting_us += timing.sorting_us;
            avg.hash_collection_us += timing.hash_collection_us;
            avg.cache_check_us += timing.cache_check_us;
            avg.tile_extraction_us += timing.tile_extraction_us;
            avg.webp_encoding_us += timing.webp_encoding_us;
            avg.cache_update_us += timing.cache_update_us;
            avg.total_us += timing.total_us;
            avg.tiles_detected += timing.tiles_detected;
            avg.tiles_merged += timing.tiles_merged;
            avg.cache_hits += timing.cache_hits;
            avg.tiles_encoded += timing.tiles_encoded;
        }

        // Print average
        avg.diff_detection_us /= frames as f64;
        avg.tile_merging_us /= frames as f64;
        avg.priority_calc_us /= frames as f64;
        avg.sorting_us /= frames as f64;
        avg.hash_collection_us /= frames as f64;
        avg.cache_check_us /= frames as f64;
        avg.tile_extraction_us /= frames as f64;
        avg.webp_encoding_us /= frames as f64;
        avg.cache_update_us /= frames as f64;
        avg.total_us /= frames as f64;
        avg.tiles_detected /= frames;
        avg.tiles_merged /= frames;
        avg.cache_hits /= frames;
        avg.tiles_encoded /= frames;

        println!("\n═════════════ AVERAGE ═════════════");
        avg.print(0);
    }

    println!("\n\n💡 Interpretation:");
    println!("   - WebP Encoding зазвичай займає 80-90% часу");
    println!("   - Diff Detection та Tile Extraction - другі за важливістю");
    println!("   - Інші операції (merging, sorting) - мізерні (<1% кожна)\n");
}
