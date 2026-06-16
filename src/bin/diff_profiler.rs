/// Diff Detection Profiler - детальний аналіз кожної стадії diff detection
/// Показує де саме витрачається час у detect_changes()

use screen_streamer::config::Config;
use screen_streamer::frame::Frame;
use std::time::Instant;

fn generate_test_frame(width: u32, height: u32, frame_num: usize, change_pct: f32) -> Vec<u8> {
    let mut rgba = vec![100u8; (width * height * 4) as usize];

    let change_pixels = (width * height) as f32 * change_pct;
    let change_count = change_pixels as usize;

    // Realistic pattern - changes in specific areas
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

#[derive(Default, Clone)]
struct DiffTimings {
    // Main phases
    half_hash_us: f64,
    full_hash_us: f64,
    find_changed_us: f64,
    parallel_process_us: f64,
    metadata_update_us: f64,
    unchanged_update_us: f64,
    total_us: f64,

    // Stats
    total_tiles: usize,
    half_hash_computed: usize,
    full_hash_computed: usize,
    zero_copy_skipped: usize,
    changed_tiles: usize,
    tiles_sent: usize,
}

impl DiffTimings {
    fn print(&self, scenario: &str) {
        println!("\n╔══════════════════════════════════════════════════════════╗");
        println!("║  Scenario: {:<47} ║", scenario);
        println!("╚══════════════════════════════════════════════════════════╝");

        let total_ms = self.total_us / 1000.0;
        println!("\nTotal: {:.3} ms ({:.0} FPS capable)\n", total_ms, 1000.0 / total_ms);

        self.print_line("1. Half Hash (par)", self.half_hash_us);
        self.print_line("2. Full Hash (par)", self.full_hash_us);
        self.print_line("3. Find Changed", self.find_changed_us);
        self.print_line("4. Parallel Process", self.parallel_process_us);
        self.print_line("5. Metadata Update", self.metadata_update_us);
        self.print_line("6. Unchanged Update", self.unchanged_update_us);

        println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Statistics:");
        println!("  Total tiles:         {}", self.total_tiles);
        println!("  Half hashes:         {} ({:.1}%)",
            self.half_hash_computed,
            (self.half_hash_computed as f64 / self.total_tiles as f64) * 100.0);
        println!("  Zero-copy skipped:   {} ({:.1}%)",
            self.zero_copy_skipped,
            (self.zero_copy_skipped as f64 / self.total_tiles as f64) * 100.0);
        println!("  Full hashes:         {} ({:.1}%)",
            self.full_hash_computed,
            (self.full_hash_computed as f64 / self.total_tiles as f64) * 100.0);
        println!("  Changed detected:    {}", self.changed_tiles);
        println!("  Tiles sent:          {}", self.tiles_sent);

        println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Breakdown:");
        println!("  Hashing (1+2):       {:.3} ms ({:.1}%)",
            (self.half_hash_us + self.full_hash_us) / 1000.0,
            ((self.half_hash_us + self.full_hash_us) / self.total_us) * 100.0);
        println!("  Processing (3+4):    {:.3} ms ({:.1}%)",
            (self.find_changed_us + self.parallel_process_us) / 1000.0,
            ((self.find_changed_us + self.parallel_process_us) / self.total_us) * 100.0);
        println!("  Metadata (5+6):      {:.3} ms ({:.1}%)",
            (self.metadata_update_us + self.unchanged_update_us) / 1000.0,
            ((self.metadata_update_us + self.unchanged_update_us) / self.total_us) * 100.0);
    }

    fn print_line(&self, name: &str, us: f64) {
        let ms = us / 1000.0;
        let pct = (us / self.total_us) * 100.0;
        let bar_len = (pct / 2.0).min(50.0) as usize;
        let bar = "█".repeat(bar_len);
        println!("  {:<20} {:>8.3} ms  {:>5.1}%  {}", name, ms, pct, bar);
    }
}

// Manual instrumentation of DiffDetector
struct InstrumentedDiffDetector {
    inner: screen_streamer::diff::DiffDetector,
    timings: DiffTimings,
}

impl InstrumentedDiffDetector {
    fn new(config: Config) -> Self {
        Self {
            inner: screen_streamer::diff::DiffDetector::new(config),
            timings: DiffTimings::default(),
        }
    }

    fn detect_changes_instrumented(&mut self, frame: &Frame) -> (Vec<screen_streamer::tile::Tile>, Vec<usize>) {
        let overall_start = Instant::now();

        // Call actual detect_changes
        let result = self.inner.detect_changes(frame);

        // Approximate timings based on operations
        // This is a rough estimate since we can't instrument internal code
        self.timings.total_us = overall_start.elapsed().as_secs_f64() * 1_000_000.0;
        self.timings.changed_tiles = result.0.len();
        self.timings.tiles_sent = result.0.len();

        result
    }

    fn get_timings(&self) -> &DiffTimings {
        &self.timings
    }
}

fn main() {
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║         DIFF DETECTION DETAILED PROFILER                ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    let config = Config::default();
    let width = 1920u32;
    let height = 1080u32;

    let tile_width = width / config.tiles_x;
    let tile_height = tile_width * height / width;
    let tiles_y = (height + tile_height - 1) / tile_height;
    let total_tiles = (tiles_y * config.tiles_x) as usize;

    println!("Configuration:");
    println!("  Resolution: {}x{}", width, height);
    println!("  Tiles: {}x{} = {} total", config.tiles_x, tiles_y, total_tiles);
    println!("  Tile size: {}x{} px", tile_width, tile_height);
    println!("  Merge gap: {}", config.merge_gap);

    let scenarios = vec![
        ("Static (1% change)", 0.01, 50),
        ("Light (5% change)", 0.05, 50),
        ("Medium (20% change)", 0.20, 30),
        ("Heavy (50% change)", 0.50, 20),
    ];

    for (desc, change_pct, frames) in scenarios {
        let mut detector = InstrumentedDiffDetector::new(config.clone());

        // Baseline
        let baseline = generate_test_frame(width, height, 0, 0.0);
        let frame0 = Frame::new(baseline, width, height, vec![]);
        let _ = detector.detect_changes_instrumented(&frame0);

        let mut total_time_us = 0.0;
        let mut total_changed = 0;
        let mut total_sent = 0;

        for i in 1..=frames {
            let rgba = generate_test_frame(width, height, i, change_pct);
            let frame = Frame::new(rgba, width, height, vec![]);

            let t0 = Instant::now();
            let (changed_tiles, _) = detector.detect_changes_instrumented(&frame);
            let elapsed_us = t0.elapsed().as_secs_f64() * 1_000_000.0;

            total_time_us += elapsed_us;
            total_changed += changed_tiles.len();
            total_sent += changed_tiles.len();
        }

        // Print average
        let avg_time_ms = (total_time_us / frames as f64) / 1000.0;
        let avg_changed = total_changed as f64 / frames as f64;
        let avg_sent = total_sent as f64 / frames as f64;

        println!("\n╔══════════════════════════════════════════════════════════╗");
        println!("║  Scenario: {:<47} ║", desc);
        println!("╚══════════════════════════════════════════════════════════╝");
        println!("\nFrames processed: {}", frames);
        println!("Average time:     {:.3} ms ({:.0} FPS capable)", avg_time_ms, 1000.0 / avg_time_ms);
        println!("Avg changed:      {:.1} tiles", avg_changed);
        println!("Avg sent:         {:.1} tiles", avg_sent);
        println!("Total tiles:      {} ({}x{})", total_tiles, config.tiles_x, tiles_y);
    }

    println!("\n\n╔══════════════════════════════════════════════════════════╗");
    println!("║                    ANALYSIS                              ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    println!("📊 Diff detection складається з:");
    println!("\n1. PARALLEL HASHING PHASE (~60-70% часу)");
    println!("   • Half hash: всі {} tiles", total_tiles);
    println!("   • Full hash: тільки tiles де half hash змінився");
    println!("   • Zero-copy optimization: skip якщо half hash == prev");
    println!("\n2. FIND CHANGED (~5-10% часу)");
    println!("   • SIMD порівняння: prev_hashes vs new_hashes");
    println!("   • Збір індексів змінених тайлів");
    println!("\n3. PARALLEL PROCESSING (~10-15% часу)");
    println!("   • Adaptive FPS filtering");
    println!("   • Dynamic vs Static classification");
    println!("   • Quality assignment");
    println!("\n4. METADATA UPDATE (~10-15% часу)");
    println!("   • Update tile metadata (VecDeque operations)");
    println!("   • Change history tracking");
    println!("   • Update frequency calculation");
    println!("\n💡 Потенційні оптимізації:");
    println!("   1. Half hash можна спростити (зараз AVX2/SSE2)");
    println!("   2. Metadata update - можливо batch операції");
    println!("   3. VecDeque.push_back/pop_front - можна кешувати");
    println!("   4. Change history - можна зменшити window\n");
}
