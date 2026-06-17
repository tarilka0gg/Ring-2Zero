use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct BenchmarkCache {
    cpu_model: String,
    ms_per_tile: f32,
    merge_gap: u32,
    timestamp: u64,
    binary_mtime: u64,
}

#[derive(Clone)]
pub struct Config {
    pub ws_port: u16,
    pub target_fps: std::num::NonZeroU64,
    pub tiles_x: u32,
    pub webp_quality_low: f32,
    pub webp_quality_high: f32,
    pub merge_gap: u32,
    pub priority_history_window: usize,
    pub priority_frequency_weight: f32,
    pub priority_speed_weight: f32,
    pub priority_center_weight: f32,
    pub static_tile_fps: std::num::NonZeroU64,
    pub dynamic_tile_fps: std::num::NonZeroU64,
    pub debug_mode: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ws_port: 9001,
            target_fps: std::num::NonZeroU64::new(32).unwrap(),
            tiles_x: 20,
            webp_quality_low: 0.5,
            webp_quality_high: 8.0,
            merge_gap: 0,
            priority_history_window: 30,
            priority_frequency_weight: 0.5,
            priority_speed_weight: 0.3,
            priority_center_weight: 0.2,
            static_tile_fps: std::num::NonZeroU64::new(4).unwrap(),
            dynamic_tile_fps: std::num::NonZeroU64::new(32).unwrap(),
            debug_mode: false,
        }
    }
}

impl Config {
    pub fn frame_duration(&self) -> std::time::Duration {
        std::time::Duration::from_millis(1000 / self.target_fps.get())
    }

    /// Auto-detect optimal merge_gap based on CPU encoding speed
    pub fn with_auto_merge_gap() -> Self {
        let mut config = Self::default();

        // Try to load from cache first
        if let Some(cached_gap) = Self::load_cached_merge_gap() {
            println!("✅ [Adaptive] Using cached benchmark result: merge_gap={}", cached_gap);
            config.merge_gap = cached_gap;
            return config;
        }

        println!("🔍 [Adaptive] Running CPU benchmark (first run or cache invalid)...");

        // Benchmark encoding speed
        let ms_per_tile = Self::benchmark_encoding_speed();

        // Determine merge_gap based on performance
        config.merge_gap = if ms_per_tile > 20.0 {
            println!("🐌 [Adaptive] Slow CPU detected ({:.1}ms/tile) → merge_gap=3 (aggressive merging)", ms_per_tile);
            3  // Weak: 40 tiles → ~8-12 merged tiles
        } else if ms_per_tile > 10.0 {
            println!("⚡ [Adaptive] Medium CPU detected ({:.1}ms/tile) → merge_gap=1 (moderate merging)", ms_per_tile);
            1  // Medium: 40 tiles → ~20-25 merged tiles
        } else {
            println!("🚀 [Adaptive] Fast CPU detected ({:.1}ms/tile) → merge_gap=0 (no merging)", ms_per_tile);
            0  // Strong: 40 tiles → ~35-40 tiles (almost no merge)
        };

        // Save to cache
        Self::save_benchmark_cache(ms_per_tile, config.merge_gap);

        config
    }

    fn cache_file_path() -> std::path::PathBuf {
        // Use XDG Base Directory specification
        let cache_dir = std::env::var("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                std::path::PathBuf::from(home).join(".cache")
            });

        let app_cache = cache_dir.join("screen-streamer");
        let _ = std::fs::create_dir_all(&app_cache);
        app_cache.join("cpu_bench.json")
    }

    fn get_cpu_model() -> String {
        // Read from /proc/cpuinfo on Linux
        if let Ok(contents) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in contents.lines() {
                if line.starts_with("model name") {
                    if let Some(name) = line.split(':').nth(1) {
                        return name.trim().to_string();
                    }
                }
            }
        }
        "unknown".to_string()
    }

    fn get_binary_mtime() -> u64 {
        // Get modification time of current executable
        if let Ok(exe) = std::env::current_exe() {
            if let Ok(metadata) = std::fs::metadata(&exe) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                        return duration.as_secs();
                    }
                }
            }
        }
        0
    }

    fn load_cached_merge_gap() -> Option<u32> {
        let cache_path = Self::cache_file_path();

        // Read cache file
        let contents = std::fs::read_to_string(&cache_path).ok()?;
        let cache: BenchmarkCache = serde_json::from_str(&contents).ok()?;

        // Validate cache
        let current_cpu = Self::get_cpu_model();
        let current_mtime = Self::get_binary_mtime();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Check CPU model matches
        if cache.cpu_model != current_cpu {
            println!("  Cache invalid: CPU changed ({} → {})", cache.cpu_model, current_cpu);
            return None;
        }

        // Check binary not recompiled
        if cache.binary_mtime != current_mtime {
            println!("  Cache invalid: Binary recompiled");
            return None;
        }

        // Check cache age < 7 days
        let age_days = (now - cache.timestamp) / 86400;
        if age_days > 7 {
            println!("  Cache invalid: Too old ({} days)", age_days);
            return None;
        }

        Some(cache.merge_gap)
    }

    fn save_benchmark_cache(ms_per_tile: f32, merge_gap: u32) {
        let cache_path = Self::cache_file_path();

        let cache = BenchmarkCache {
            cpu_model: Self::get_cpu_model(),
            ms_per_tile,
            merge_gap,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            binary_mtime: Self::get_binary_mtime(),
        };

        if let Ok(json) = serde_json::to_string_pretty(&cache) {
            if let Err(e) = std::fs::write(&cache_path, json) {
                eprintln!("⚠️  Failed to save benchmark cache: {}", e);
            } else {
                println!("✅ Benchmark cached to {}", cache_path.display());
            }
        }
    }

    fn benchmark_encoding_speed() -> f32 {
        use std::time::Instant;

        println!("  Creating test data...");
        // Create typical tile (48x48 RGBA)
        let tile_width = 48u32;
        let tile_height = 48u32;
        let test_data = vec![128u8; (tile_width * tile_height * 4) as usize];

        println!("  Warm-up (2 iterations)...");
        // Warm-up (fill CPU cache)
        for i in 0..2 {
            println!("    Warm-up iteration {}", i + 1);
            let encoder = webp::Encoder::from_rgba(&test_data, tile_width, tile_height);
            let _result = encoder.encode(10.0);
            println!("    Done");
        }

        println!("  Running benchmark (10 iterations)...");
        // Actual benchmark (10 iterations for stability)
        let start = Instant::now();
        for i in 0..10 {
            println!("    Benchmark iteration {}", i + 1);
            let encoder = webp::Encoder::from_rgba(&test_data, tile_width, tile_height);
            let _result = encoder.encode(10.0);
        }
        let elapsed = start.elapsed().as_secs_f32() * 1000.0;  // Convert to ms

        println!("  Benchmark complete: {:.2}ms total", elapsed);

        elapsed / 10.0  // Average ms per tile
    }
}
