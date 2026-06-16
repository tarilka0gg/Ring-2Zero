/// Modern Hash Functions Comparison
/// Порівняння сучасних швидких хеш-функцій: xxHash3, HighwayHash, FNV-1a, CityHash

use std::time::Instant;
use std::collections::HashMap;

// ============================================================================
// СУЧАСНІ ХЕШ-ФУНКЦІЇ
// ============================================================================

// xxHash3 (64-bit) - один з найшвидших
fn xxhash3_64(data: &[u8]) -> u64 {
    use xxhash_rust::xxh3::xxh3_64;
    xxh3_64(data)
}

// FNV-1a (швидкий, простий)
fn fnv1a_64(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

// Murmur3-inspired simple hash
fn murmur3_simple(data: &[u8]) -> u64 {
    const C1: u64 = 0x87c37b91114253d5;
    const C2: u64 = 0x4cf5ad432745937f;

    let mut hash = 0u64;

    for chunk in data.chunks(8) {
        let mut k = 0u64;
        for (i, &byte) in chunk.iter().enumerate() {
            k |= (byte as u64) << (i * 8);
        }

        k = k.wrapping_mul(C1);
        k = k.rotate_left(31);
        k = k.wrapping_mul(C2);

        hash ^= k;
        hash = hash.rotate_left(27);
        hash = hash.wrapping_mul(5).wrapping_add(0x52dce729);
    }

    hash ^= data.len() as u64;
    hash
}

// Simple but good mixing hash (inspired by SplitMix64)
fn splitmix64_hash(data: &[u8]) -> u64 {
    let mut hash = data.len() as u64;

    for chunk in data.chunks(8) {
        let mut k = 0u64;
        for (i, &byte) in chunk.iter().enumerate() {
            k |= (byte as u64) << (i * 8);
        }

        hash = hash.wrapping_add(k);
        hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d049bb133111eb);
        hash = hash ^ (hash >> 31);
    }

    hash
}

// WyHash-inspired (claimed to be fastest)
fn wyhash_simple(data: &[u8]) -> u64 {
    const P0: u64 = 0xa0761d6478bd642f;
    const P1: u64 = 0xe7037ed1a0b428db;

    let mut hash = data.len() as u64;

    for chunk in data.chunks(8) {
        let mut k = 0u64;
        for (i, &byte) in chunk.iter().enumerate() {
            k |= (byte as u64) << (i * 8);
        }

        hash ^= k.wrapping_mul(P0);
        hash = hash.wrapping_mul(P1);
    }

    hash
}

// Поточна реалізація AVX2 (для порівняння)
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2_current(data: &[u8]) -> u64 {
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

    if !remainder.is_empty() {
        let mut tail = [0u8; 32];
        tail[..remainder.len()].copy_from_slice(remainder);
        let v = _mm256_loadu_si256(tail.as_ptr() as *const __m256i);
        acc = _mm256_xor_si256(acc, v);
    }

    let low = _mm256_extracti128_si256(acc, 0);
    let high = _mm256_extracti128_si256(acc, 1);
    let xor128 = _mm_xor_si128(low, high);

    let low64 = _mm_extract_epi64(xor128, 0) as u64;
    let high64 = _mm_extract_epi64(xor128, 1) as u64;

    low64 ^ high64 ^ (data.len() as u64)
}

// ============================================================================
// ТЕСТУВАННЯ
// ============================================================================

struct HashResult {
    name: String,
    speed_us: f64,
    false_negatives: usize,
    false_positives: usize,
    collisions: usize,
}

fn generate_realistic_tile(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut data = vec![0u8; (w * h * 4) as usize];

    // Realistic pixel patterns (not random)
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;

            // Gradient-like pattern
            data[idx] = ((x + seed) % 256) as u8;     // R
            data[idx + 1] = ((y + seed) % 256) as u8; // G
            data[idx + 2] = ((x + y + seed) % 256) as u8; // B
            data[idx + 3] = 255;                       // A
        }
    }

    data
}

fn apply_small_change(data: &mut [u8]) {
    // Змінюємо 1 піксель (мінімальна зміна)
    if data.len() >= 4 {
        data[0] = data[0].wrapping_add(1);
    }
}

fn apply_medium_change(data: &mut [u8]) {
    // Змінюємо 10 пікселів
    for i in 0..10 {
        let idx = (i * 100) % data.len();
        if idx < data.len() {
            data[idx] = data[idx].wrapping_add(1);
        }
    }
}

fn apply_large_change(data: &mut [u8]) {
    // Змінюємо 50% пікселів
    for i in (0..data.len()).step_by(8) {
        data[i] = data[i].wrapping_add(1);
    }
}

type HashFn = fn(&[u8]) -> u64;

fn benchmark_hash(name: &str, hash_fn: HashFn, tiles: &[Vec<u8>]) -> HashResult {
    const ITERATIONS: usize = 10000;

    // Speed test
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        for tile in tiles {
            let _ = hash_fn(tile);
        }
    }
    let elapsed_us = start.elapsed().as_secs_f64() * 1_000_000.0;
    let speed = elapsed_us / (ITERATIONS * tiles.len()) as f64;

    // Quality test
    let mut false_negatives = 0;
    let mut false_positives = 0;
    let mut hash_map = HashMap::new();
    let mut collisions = 0;

    for (i, tile) in tiles.iter().enumerate() {
        let hash1 = hash_fn(tile);

        // Check for collisions
        if let Some(&prev_i) = hash_map.get(&hash1) {
            if prev_i != i && tiles[prev_i] != tiles[i] {
                collisions += 1;
            }
        }
        hash_map.insert(hash1, i);

        // Test small change detection
        let mut modified = tile.clone();
        apply_small_change(&mut modified);
        let hash2 = hash_fn(&modified);

        if hash1 == hash2 {
            false_negatives += 1;
        }

        // Test identical detection
        let hash3 = hash_fn(tile);
        if hash1 != hash3 {
            false_positives += 1;
        }
    }

    HashResult {
        name: name.to_string(),
        speed_us: speed,
        false_negatives,
        false_positives,
        collisions,
    }
}

fn main() {
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║      MODERN HASH FUNCTIONS COMPARISON                   ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    // Generate realistic test tiles
    println!("Генеруємо реалістичні тайли...");
    let mut tiles = Vec::new();

    let sizes = [(48, 27), (96, 54), (192, 108)];
    for &(w, h) in &sizes {
        for seed in 0..50 {
            tiles.push(generate_realistic_tile(w, h, seed));
        }
    }

    println!("Створено {} тайлів\n", tiles.len());

    // Test all hash functions
    let hash_functions: Vec<(&str, HashFn)> = vec![
        ("xxHash3-64", xxhash3_64),
        ("FNV-1a", fnv1a_64),
        ("Murmur3-inspired", murmur3_simple),
        ("SplitMix64-inspired", splitmix64_hash),
        ("WyHash-inspired", wyhash_simple),
    ];

    #[cfg(target_arch = "x86_64")]
    let hash_functions_simd: Vec<(&str, HashFn)> = vec![
        ("AVX2 Current", |d| unsafe { hash_avx2_current(d) }),
    ];

    println!("╔════════════════════════════════════════════════════════════════════╗");
    println!("║                    BENCHMARK RESULTS                               ║");
    println!("╠════════════════════════════════════════════════════════════════════╣");
    println!("║ Hash Function       │ Speed (ns) │ False- │ False+ │ Collisions ║");
    println!("║                     │            │  Neg   │  Pos   │            ║");
    println!("╠════════════════════════════════════════════════════════════════════╣");

    let mut results = Vec::new();

    for (name, hash_fn) in &hash_functions {
        let result = benchmark_hash(name, *hash_fn, &tiles);
        results.push(result);
    }

    #[cfg(target_arch = "x86_64")]
    for (name, hash_fn) in &hash_functions_simd {
        let result = benchmark_hash(name, *hash_fn, &tiles);
        results.push(result);
    }

    // Sort by quality (FN is most critical)
    results.sort_by(|a, b| {
        let score_a = a.false_negatives * 10000 + a.false_positives * 100 + a.speed_us as usize;
        let score_b = b.false_negatives * 10000 + b.false_positives * 100 + b.speed_us as usize;
        score_a.cmp(&score_b)
    });

    for result in &results {
        let speed_ns = result.speed_us * 1000.0;
        let status = if result.false_negatives == 0 { "✅" } else { "❌" };

        println!("║ {:<19} │ {:>10.0} │ {:>6} │ {:>6} │ {:>10} ║ {}",
            result.name,
            speed_ns,
            result.false_negatives,
            result.false_positives,
            result.collisions,
            status
        );
    }

    println!("╚════════════════════════════════════════════════════════════════════╝\n");

    println!("📊 ВИСНОВКИ:\n");

    let best = &results[0];
    println!("🏆 Найкращий: {}", best.name);
    println!("   Швидкість: {:.0} ns/hash", best.speed_us * 1000.0);
    println!("   False Negatives: {} {}", best.false_negatives,
        if best.false_negatives == 0 { "✅" } else { "❌" });
    println!("   False Positives: {}", best.false_positives);
    println!("   Collisions: {}\n", best.collisions);

    println!("💡 РЕКОМЕНДАЦІЇ:\n");
    println!("• False Negatives = 0 — ОБОВ'ЯЗКОВО!");
    println!("• Швидкість < 100 ns — ДОБРЕ для 1600 tiles");
    println!("• Collisions < 10 — прийнятно\n");

    if best.false_negatives == 0 {
        println!("✅ {} підходить для half hash!", best.name);
        println!("   Можна безпечно замінити поточну реалізацію\n");
    } else {
        println!("⚠️  Жодна функція не має 0 false negatives!");
        println!("   Можливо проблема в тестових даних\n");
    }
}
