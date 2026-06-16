// Quick test for adaptive config
use std::time::Instant;

fn main() {
    println!("Testing WebP encoding speed...");

    let tile_width = 48u32;
    let tile_height = 48u32;
    let test_data = vec![128u8; (tile_width * tile_height * 4) as usize];

    // Warm-up
    println!("Warm-up...");
    for _ in 0..2 {
        let _ = webp::Encoder::from_rgba(&test_data, tile_width, tile_height)
            .encode(10.0);
    }

    // Benchmark
    println!("Benchmarking 10 iterations...");
    let start = Instant::now();
    for i in 0..10 {
        let _ = webp::Encoder::from_rgba(&test_data, tile_width, tile_height)
            .encode(10.0);
        println!("  Iteration {} done", i + 1);
    }
    let elapsed = start.elapsed().as_secs_f32() * 1000.0;

    let ms_per_tile = elapsed / 10.0;
    println!("\n✅ Result: {:.2}ms per tile", ms_per_tile);

    if ms_per_tile > 20.0 {
        println!("→ Slow CPU: merge_gap = 3");
    } else if ms_per_tile > 10.0 {
        println!("→ Medium CPU: merge_gap = 1");
    } else {
        println!("→ Fast CPU: merge_gap = 0");
    }
}
