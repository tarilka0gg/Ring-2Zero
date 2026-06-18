/// WebP Codec Benchmark
/// Compares webp, webpx, fast-webp, and webp-rust implementations

use std::time::Instant;

fn generate_test_tile(width: u32, height: u32, pattern: &str) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    match pattern {
        "gradient" => {
            // Smooth gradient - compresses well
            for y in 0..height {
                for x in 0..width {
                    let idx = ((y * width + x) * 4) as usize;
                    rgba[idx] = (x * 255 / width) as u8;
                    rgba[idx + 1] = (y * 255 / height) as u8;
                    rgba[idx + 2] = 128;
                    rgba[idx + 3] = 255;
                }
            }
        }
        "noise" => {
            // Random noise - hard to compress
            for i in (0..rgba.len()).step_by(4) {
                rgba[i] = ((i * 7 + 13) % 256) as u8;
                rgba[i + 1] = ((i * 11 + 17) % 256) as u8;
                rgba[i + 2] = ((i * 13 + 19) % 256) as u8;
                rgba[i + 3] = 255;
            }
        }
        "text" => {
            // Text-like pattern - sharp edges
            for y in 0..height {
                for x in 0..width {
                    let idx = ((y * width + x) * 4) as usize;
                    let is_text = (x % 8 < 4) && (y % 16 < 12);
                    let color = if is_text { 255 } else { 240 };
                    rgba[idx] = color;
                    rgba[idx + 1] = color;
                    rgba[idx + 2] = color;
                    rgba[idx + 3] = 255;
                }
            }
        }
        _ => {}
    }

    rgba
}

struct BenchResult {
    name: String,
    encode_time_ms: f64,
    output_size: usize,
    success: bool,
    error: Option<String>,
}

fn bench_fast_webp_current(data: &[u8], width: u32, height: u32, quality: f32) -> BenchResult {
    let start = Instant::now();
    let result = fast_webp::encode_rgba(
        data,
        width,
        height,
        fast_webp::WebpOptions {
            quality: quality,
            ..Default::default()
        },
    );
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(encoded) => BenchResult {
            name: "fast-webp 0.1.1 (current)".to_string(),
            encode_time_ms: elapsed,
            output_size: encoded.len(),
            success: true,
            error: None,
        },
        Err(e) => BenchResult {
            name: "fast-webp 0.1.1 (current)".to_string(),
            encode_time_ms: elapsed,
            output_size: 0,
            success: false,
            error: Some(format!("{:?}", e)),
        }
    }
}

#[cfg(feature = "webp_bench")]
fn bench_webp_old(data: &[u8], width: u32, height: u32, quality: f32) -> BenchResult {
    let start = Instant::now();
    let result = webp::Encoder::from_rgba(data, width, height).encode(quality);
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    BenchResult {
        name: "webp 0.3 (old)".to_string(),
        encode_time_ms: elapsed,
        output_size: result.len(),
        success: true,
        error: None,
    }
}

#[cfg(not(feature = "webp_bench"))]
fn bench_webp_old(_data: &[u8], _width: u32, _height: u32, _quality: f32) -> BenchResult {
    BenchResult {
        name: "webp 0.3 (old)".to_string(),
        encode_time_ms: 0.0,
        output_size: 0,
        success: false,
        error: Some("Not compiled with webp_bench feature".to_string()),
    }
}

#[cfg(feature = "webp_bench")]
fn bench_webpx(data: &[u8], width: u32, height: u32, quality: f32) -> BenchResult {
    use webpx::{Encoder, Unstoppable};

    let start = Instant::now();

    let result = Encoder::new_rgba(data, width, height)
        .quality(quality)
        .encode(Unstoppable);

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(encoded) => BenchResult {
            name: "webpx 0.4.0".to_string(),
            encode_time_ms: elapsed,
            output_size: encoded.len(),
            success: true,
            error: None,
        },
        Err(e) => BenchResult {
            name: "webpx 0.4.0".to_string(),
            encode_time_ms: elapsed,
            output_size: 0,
            success: false,
            error: Some(format!("{:?}", e)),
        }
    }
}

#[cfg(not(feature = "webp_bench"))]
fn bench_webpx(_data: &[u8], _width: u32, _height: u32, _quality: f32) -> BenchResult {
    BenchResult {
        name: "webpx 0.4.0".to_string(),
        encode_time_ms: 0.0,
        output_size: 0,
        success: false,
        error: Some("Not compiled with webp_bench feature".to_string()),
    }
}


#[cfg(feature = "webp_bench")]
fn bench_webp_rust(data: &[u8], width: u32, height: u32, quality: f32) -> BenchResult {
    use webp_rust::{encode_lossy, ImageBuffer};

    let start = Instant::now();

    // Create ImageBuffer with public fields
    let buffer = ImageBuffer {
        width: width as usize,
        height: height as usize,
        rgba: data.to_vec(),
    };

    let result = encode_lossy(
        &buffer,
        0,  // optimize: 0 = fast
        quality as usize,
        None  // no EXIF
    );

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(encoded) => BenchResult {
            name: "webp-rust 0.2.1".to_string(),
            encode_time_ms: elapsed,
            output_size: encoded.len(),
            success: true,
            error: None,
        },
        Err(e) => BenchResult {
            name: "webp-rust 0.2.1".to_string(),
            encode_time_ms: elapsed,
            output_size: 0,
            success: false,
            error: Some(format!("{:?}", e)),
        }
    }
}

#[cfg(not(feature = "webp_bench"))]
fn bench_webp_rust(_data: &[u8], _width: u32, _height: u32, _quality: f32) -> BenchResult {
    BenchResult {
        name: "webp-rust 0.2.1".to_string(),
        encode_time_ms: 0.0,
        output_size: 0,
        success: false,
        error: Some("Not compiled with webp_bench feature".to_string()),
    }
}

fn print_results(pattern: &str, width: u32, height: u32, results: &[BenchResult]) {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Pattern: {} ({}×{} = {} pixels)", pattern, width, height, width * height);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    println!("{:<25} {:>12} {:>12} {:>10}", "Codec", "Time (ms)", "Size (KB)", "Speed");
    println!("{}", "─".repeat(65));

    let baseline_time = results.iter()
        .find(|r| r.name.contains("fast-webp") && r.name.contains("current"))
        .map(|r| r.encode_time_ms)
        .unwrap_or(1.0);

    for result in results {
        if result.success {
            let speedup = baseline_time / result.encode_time_ms;
            println!(
                "{:<25} {:>12.2} {:>12.1} {:>9.2}×",
                result.name,
                result.encode_time_ms,
                result.output_size as f64 / 1024.0,
                speedup
            );
        } else {
            println!(
                "{:<25} {:>12} {:>12} {}",
                result.name,
                "FAILED",
                "-",
                result.error.as_ref().unwrap_or(&"Unknown error".to_string())
            );
        }
    }
}

fn main() {
    println!("\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║            WebP Codec Implementations Benchmark                 ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");

    #[cfg(not(feature = "webp_bench"))]
    {
        println!("\n⚠️  WARNING: Compiled without --features webp_bench");
        println!("   Only fast-webp 0.1.1 (current) will be tested.");
        println!("   Run: cargo run --release --bin webp_codec_bench --features webp_bench\n");
    }

    let test_cases = vec![
        ("gradient", 96, 54),   // Typical tile size (tiles_x=20)
        ("text", 96, 54),       // UI/text pattern
        ("noise", 96, 54),      // Worst case
        ("gradient", 120, 68),  // Larger tile (tiles_x=16)
    ];

    let quality = 75.0;

    for (pattern, width, height) in test_cases {
        let data = generate_test_tile(width, height, pattern);

        let mut results = Vec::new();

        // Warmup
        let _ = fast_webp::encode_rgba(&data, width, height, fast_webp::WebpOptions::default());

        // Benchmark all implementations
        results.push(bench_fast_webp_current(&data, width, height, quality));
        results.push(bench_webp_old(&data, width, height, quality));
        results.push(bench_webpx(&data, width, height, quality));
        results.push(bench_webp_rust(&data, width, height, quality));

        print_results(pattern, width, height, &results);
    }

    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("💡 Interpretation:");
    println!("   - Speed: Higher is better (e.g., 2× = twice as fast)");
    println!("   - Size: Smaller is better (better compression)");
    println!("   - Baseline: fast-webp 0.1.1 (current) = 1.0×");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
}
