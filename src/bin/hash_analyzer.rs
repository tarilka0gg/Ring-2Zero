/// Hash Quality Analyzer - аналіз різних хеш-функцій для tile detection
/// Порівнює: AVX2/SSE2 hash, XOR checksum, sampling, та інші варіанти

use std::time::Instant;
use std::collections::HashMap;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

// ============================================================================
// ПОТОЧНІ РЕАЛІЗАЦІЇ (з tile.rs)
// ============================================================================

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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn hash_sse2_current(data: &[u8]) -> u64 {
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

    if !remainder.is_empty() {
        let mut tail = [0u8; 16];
        tail[..remainder.len()].copy_from_slice(remainder);
        let v = _mm_loadu_si128(tail.as_ptr() as *const __m128i);
        acc = _mm_xor_si128(acc, v);
    }

    let low64 = _mm_extract_epi64(acc, 0) as u64;
    let high64 = _mm_extract_epi64(acc, 1) as u64;

    low64 ^ high64 ^ (data.len() as u64)
}

// ============================================================================
// АЛЬТЕРНАТИВНІ РЕАЛІЗАЦІЇ ДЛЯ HALF HASH
// ============================================================================

// Variant 1: Простий XOR checksum (найшвидший)
fn hash_xor_simple(data: &[u8]) -> u64 {
    let mut hash = 0u64;
    for chunk in data.chunks(8) {
        let mut bytes = [0u8; 8];
        bytes[..chunk.len()].copy_from_slice(chunk);
        hash ^= u64::from_le_bytes(bytes);
    }
    hash ^ (data.len() as u64)
}

// Variant 2: XOR checksum з rotating
fn hash_xor_rotate(data: &[u8]) -> u64 {
    let mut hash = 0u64;
    for chunk in data.chunks(8) {
        let mut bytes = [0u8; 8];
        bytes[..chunk.len()].copy_from_slice(chunk);
        let value = u64::from_le_bytes(bytes);
        hash = hash.rotate_left(7) ^ value;
    }
    hash ^ (data.len() as u64)
}

// Variant 3: Sampling (кожен N-й піксель)
fn hash_sample_pixels(data: &[u8], stride: usize) -> u64 {
    let mut hash = 0u64;
    let mut count = 0u64;

    for i in (0..data.len()).step_by(stride * 4) {
        if i + 3 < data.len() {
            // Sample один піксель (RGBA = 4 bytes)
            let pixel = u32::from_le_bytes([
                data[i],
                data[i + 1],
                data[i + 2],
                data[i + 3],
            ]);
            hash ^= pixel as u64;
            count += 1;
        }
    }

    hash ^ count
}

// Variant 4: Адаптивний sampling (більше samples для маленьких тайлів)
fn hash_adaptive_sample(data: &[u8]) -> u64 {
    let pixels = data.len() / 4;

    // Для маленьких тайлів - більше samples
    let stride = if pixels < 100 {
        2  // Кожен 2-й піксель
    } else if pixels < 1000 {
        4  // Кожен 4-й
    } else {
        8  // Кожен 8-й
    };

    hash_sample_pixels(data, stride)
}

// Variant 5: First N bytes checksum
fn hash_first_bytes(data: &[u8], n: usize) -> u64 {
    let sample_size = n.min(data.len());
    hash_xor_rotate(&data[..sample_size])
}

// Variant 6: AVX2 XOR-only (без seed mixing)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2_simple(data: &[u8]) -> u64 {
    let mut acc = _mm256_setzero_si256();

    let chunks = data.chunks_exact(32);
    let remainder = chunks.remainder();

    for chunk in chunks {
        let v = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);
        acc = _mm256_xor_si256(acc, v);
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

    low64 ^ high64
}

// ============================================================================
// ТЕСТУВАННЯ
// ============================================================================

struct HashTestResult {
    name: String,
    time_us: f64,
    collisions: usize,
    false_negatives: usize,
    false_positives: usize,
}

fn generate_tile_data(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let size = (width * height * 4) as usize;
    let mut data = vec![0u8; size];

    for i in 0..size {
        data[i] = ((i * 7 + seed as usize * 13) % 256) as u8;
    }

    data
}

fn create_variant(original: &[u8], change_type: &str) -> Vec<u8> {
    let mut variant = original.to_vec();

    match change_type {
        "1px" => {
            // Змінюємо 1 піксель
            if variant.len() >= 4 {
                variant[0] ^= 1;
            }
        }
        "10px" => {
            // 10 пікселів
            for i in 0..10 {
                let idx = (i * 4) % variant.len();
                if idx < variant.len() {
                    variant[idx] ^= 1;
                }
            }
        }
        "row" => {
            // Один рядок
            let pixels_per_row = (variant.len() / 4) / 48; // припускаємо 48 рядків
            for i in 0..pixels_per_row {
                let idx = i * 4;
                if idx + 3 < variant.len() {
                    variant[idx] ^= 0xFF;
                }
            }
        }
        "50%" => {
            // 50% змін
            for i in (0..variant.len()).step_by(8) {
                variant[i] ^= 0xFF;
            }
        }
        _ => {}
    }

    variant
}

type HashFn = fn(&[u8]) -> u64;

fn test_hash_function(
    name: &str,
    hash_fn: HashFn,
    test_data: &[(Vec<u8>, Vec<u8>)],
) -> HashTestResult {
    let iterations = 1000;

    // Benchmark speed
    let start = Instant::now();
    for _ in 0..iterations {
        for (original, _) in test_data {
            let _ = hash_fn(original);
        }
    }
    let elapsed_us = start.elapsed().as_secs_f64() * 1_000_000.0;
    let time_per_hash = elapsed_us / (iterations as f64 * test_data.len() as f64);

    // Test collision resistance
    let mut hashes = HashMap::new();
    let mut collisions = 0;

    for (i, (original, _)) in test_data.iter().enumerate() {
        let hash = hash_fn(original);
        if let Some(prev_idx) = hashes.insert(hash, i) {
            if prev_idx != i {
                collisions += 1;
            }
        }
    }

    // Test detection accuracy
    let mut false_negatives = 0;
    let mut false_positives = 0;

    for (original, variant) in test_data {
        let hash1 = hash_fn(original);
        let hash2 = hash_fn(variant);

        let detected_change = hash1 != hash2;
        let actual_change = original != variant;

        if actual_change && !detected_change {
            false_negatives += 1;
        } else if !actual_change && detected_change {
            false_positives += 1;
        }
    }

    HashTestResult {
        name: name.to_string(),
        time_us: time_per_hash,
        collisions,
        false_negatives,
        false_positives,
    }
}

fn main() {
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║         HASH QUALITY ANALYZER                            ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    // Generate test data
    println!("Генеруємо тестові дані...");
    let tile_sizes = vec![
        (48, 27),   // Typical tile
        (96, 54),   // 2x tile
        (192, 108), // 4x tile
    ];

    let mut test_data = Vec::new();

    for &(w, h) in &tile_sizes {
        for seed in 0..10 {
            let original = generate_tile_data(w, h, seed);

            // Створюємо варіанти
            for change_type in &["1px", "10px", "row", "50%"] {
                let variant = create_variant(&original, change_type);
                test_data.push((original.clone(), variant));
            }

            // Також identical copy для false positive test
            test_data.push((original.clone(), original));
        }
    }

    println!("Створено {} тестових пар\n", test_data.len());

    // Test all hash functions
    let hash_functions: Vec<(&str, HashFn)> = vec![
        ("XOR Simple", hash_xor_simple),
        ("XOR Rotate", hash_xor_rotate),
        ("Sample Every 4px", |d| hash_sample_pixels(d, 4)),
        ("Sample Every 8px", |d| hash_sample_pixels(d, 8)),
        ("Adaptive Sample", hash_adaptive_sample),
        ("First 256 bytes", |d| hash_first_bytes(d, 256)),
        ("First 512 bytes", |d| hash_first_bytes(d, 512)),
    ];

    #[cfg(target_arch = "x86_64")]
    let hash_functions_simd: Vec<(&str, HashFn)> = vec![
        ("AVX2 Current (full)", |d| unsafe { hash_avx2_current(d) }),
        ("AVX2 Simple XOR", |d| unsafe { hash_avx2_simple(d) }),
        ("SSE2 Current", |d| unsafe { hash_sse2_current(d) }),
    ];

    println!("╔══════════════════════════════════════════════════════════════════════════╗");
    println!("║                         BENCHMARK RESULTS                                ║");
    println!("╠══════════════════════════════════════════════════════════════════════════╣");
    println!("║ Method               │ Time (μs) │ Collisions │ False- │ False+ │ Score ║");
    println!("║                      │           │            │  Neg   │  Pos   │       ║");
    println!("╠══════════════════════════════════════════════════════════════════════════╣");

    let mut results = Vec::new();

    for (name, hash_fn) in &hash_functions {
        let result = test_hash_function(name, *hash_fn, &test_data);
        results.push(result);
    }

    #[cfg(target_arch = "x86_64")]
    for (name, hash_fn) in &hash_functions_simd {
        let result = test_hash_function(name, *hash_fn, &test_data);
        results.push(result);
    }

    // Sort by quality score (lower is better)
    results.sort_by(|a, b| {
        let score_a = a.false_negatives * 1000 + a.false_positives * 100 + (a.time_us * 10.0) as usize;
        let score_b = b.false_negatives * 1000 + b.false_positives * 100 + (b.time_us * 10.0) as usize;
        score_a.cmp(&score_b)
    });

    for result in &results {
        let score = result.false_negatives * 1000 + result.false_positives * 100 + (result.time_us * 10.0) as usize;
        println!("║ {:<20} │ {:>9.3} │ {:>10} │ {:>6} │ {:>6} │ {:>5} ║",
            result.name,
            result.time_us,
            result.collisions,
            result.false_negatives,
            result.false_positives,
            score
        );
    }

    println!("╚══════════════════════════════════════════════════════════════════════════╝\n");

    println!("📊 АНАЛІЗ:\n");
    println!("Score = FalseNeg×1000 + FalsePos×100 + Time×10");
    println!("  • False Negatives (пропущені зміни) - КРИТИЧНО!");
    println!("  • False Positives (помилкові спрацювання) - небажано");
    println!("  • Time - важливо, але вторинно\n");

    println!("🎯 РЕКОМЕНДАЦІЇ:\n");
    println!("Для HALF HASH (первинна перевірка):");
    println!("  • Допустимі false positives (перейдемо до full hash)");
    println!("  • НЕДОПУСТИМІ false negatives (пропустимо зміни!)");
    println!("  • Швидкість критична (1600 tiles × кожен frame)\n");

    println!("Для FULL HASH (остаточна перевірка):");
    println!("  • НУЛЬ false negatives");
    println!("  • Мінімум false positives");
    println!("  • Швидкість менш критична (тільки ~2-10 tiles)\n");
}
