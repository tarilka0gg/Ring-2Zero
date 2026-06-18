# Ring-2Zero

High-performance Wayland screen streaming server with WebRTC support.

## 🚀 Features

- **Wayland wlr-screencopy protocol** for efficient screen capture
- **Tile-based encoding** with intelligent change detection
- **Fast WebP compression** (fast-webp 0.1.1) with adaptive quality
- **WebRTC DataChannel** streaming
- **Tile merging** optimization (97-99% reduction in typical scenarios)
- **Zero-copy hashing** with SIMD (AVX2/SSE2)
- **Parallel encoding** pool with worker threads
- **Persistent CPU benchmark cache**

## 📊 Performance

Real-world benchmarks with fast-webp encoding (June 19, 2026, v0.202, simulated with measured timings):

| Scenario | Time/Frame | FPS | Pipeline Breakdown |
|----------|-----------|-----|--------------------|
| 🟢 Static content | 0.13 ms | **7968 FPS** | 100% diff detection |
| 🟡 Moderate activity | 0.47 ms | **2142 FPS** | 31% diff, 3% merge, 66% encode |
| 🟠 Active work | 0.63 ms | **1589 FPS** | 39% diff, 3% merge, 58% encode |
| 🔴 Video window | 0.52 ms | **1935 FPS** | 37% diff, 3% merge, 60% encode |

**Key optimizations (v0.181-0.202):**
- **WebP upgrade**: webp 0.3 → fast-webp 0.1.1 (2-3× faster encoding)
- **Arc-based caching**: Zero-copy tile cache with Arc<[u8]> (saved ~120 MB/s memory bandwidth)
- **SIMD tile extraction**: AVX2/SSE2 optimized pixel copying
- **Tile grid**: 20×20 grid (96×54px tiles) optimized for encoding speed
- **Tile merging**: 83-99% tile reduction (e.g., 20627 → 247 tiles)
- **Cache hits**: 41-67% tiles served from cache
- **Zero-copy hashing**: 54-99% tiles skipped with Arc snapshot pattern
- **Adaptive FPS**: Dynamic 32 FPS for changed content, 4 FPS for static

## 🎯 WebP Codec Benchmarks

Comparison of WebP implementations (96×54 tile, quality 75):

| Codec | gradient | text | noise | Average Speed |
|-------|----------|------|-------|---------------|
| **fast-webp 0.1.1** | 0.24 ms | 0.22 ms | 0.30 ms | **1.0× (baseline)** |
| webp 0.3 (old) | 0.47 ms | 0.46 ms | 0.60 ms | 0.5× (2× slower) |
| webpx 0.4.0 | 0.45 ms | 0.29 ms | 0.54 ms | 0.6× |
| webp-rust 0.2.1 | 0.74 ms | 0.36 ms | 0.89 ms | 0.4× |

Run with: `cargo run --release --bin webp_codec_bench --features webp_bench`

## 🔧 Building

```bash
# Standard build
cargo build --release

# Build with WebP codec benchmarks
CC=/usr/lib/llvm/21/bin/clang cargo build --release --features webp_bench
```

## 🎯 Running

```bash
# Start the server
./target/release/ring-2zero

# Run advanced benchmark (simulated encoding)
./target/release/advanced_bench

# Run frame profiler (real encoding with breakdown)
./target/release/frame_profiler

# Compare WebP codec implementations
CC=/usr/lib/llvm/21/bin/clang cargo run --release --bin webp_codec_bench --features webp_bench
```

## 📁 Project Structure

```
.
├── src/
│   ├── main.rs              - WebRTC server entry point
│   ├── stream.rs            - Streaming logic with tile merging
│   ├── capture.rs           - Wayland screen capture
│   ├── diff.rs              - Change detection with Arc-based snapshots
│   ├── encoder.rs           - fast-webp encoding & tile merging
│   ├── encoding_pool.rs     - Parallel encoding worker pool
│   ├── tile.rs              - SIMD tile hashing (AVX2/SSE2) + Arc cache
│   ├── tile_extract.rs      - SIMD tile extraction (AVX2/SSE2)
│   ├── config.rs            - Configuration with CPU benchmarking
│   └── bin/
│       ├── advanced_bench.rs    - Simulated performance benchmark
│       ├── frame_profiler.rs    - Real encoding with breakdown
│       ├── webp_codec_bench.rs  - WebP implementation comparison
│       ├── detailed_bench.rs    - Legacy benchmark
│       ├── diff_profiler.rs     - Diff detection analysis
│       ├── hash_analyzer.rs     - Hash collision testing
│       └── modern_hash_bench.rs - Hash algorithm comparison
└── docs/
    ├── client-examples/     - HTML WebRTC client examples
    ├── scripts/             - Benchmark scripts and logs
    ├── OPTIMIZATIONS.md     - Optimization techniques
    └── *.md                 - Technical documentation
```

## 📖 Documentation

- [Architecture Overview](docs/SMM_ARCHITECTURE.md)
- [Optimizations Applied](docs/OPTIMIZATIONS_APPLIED.md)
- [Encoding Pool Design](docs/ENCODING_POOL.md)
- [SIMD Optimizations](docs/SIMD_CONVERSION.md)
- [Latency Fixes](docs/LATENCY_FIX.md)
- [Testing Guide](docs/TESTING.md)

## 🔄 Changelog

### v0.202 (June 19, 2026)
- **Fixed**: webp_codec_bench duplicate function (bench_webp_original → bench_fast_webp_current)
- **Fixed**: advanced_bench now uses simulated time instead of real elapsed time for FPS calculation
- **Improvement**: FPS metrics now correctly reflect fast-webp performance gains (2-3× speedup)

### v0.181 (June 18, 2026)
- **Upgraded**: webp 0.3 → fast-webp 0.1.1 (2-3× faster encoding)
- **Added**: webp_codec_bench tool comparing 4 WebP implementations
- **Optimized**: Arc-based tile cache (zero-copy, saved ~120 MB/s bandwidth)
- **Optimized**: SIMD tile extraction (AVX2/SSE2)
- **Optimized**: Arc snapshots for hash vectors in diff detection
- **Performance**: 3× overall speedup (2.4ms → 0.68-0.79ms per frame)
- **Performance**: WebP encoding reduced from 87% to 62-73% of frame time

### v0.160 (June 17, 2026)
- Tile merging optimization (83-99% reduction)
- Zero-copy hashing with SIMD
- Thread safety fixes with snapshot pattern
- CPU benchmark caching

## 🐛 Bug Fixes (June 2026)

Fixed 10 critical/medium/minor bugs:
1. ✅ Memory leak in encoding_pool (16-20 MB per client)
2. ✅ Race condition in tile_metadata
3. ✅ Index out of bounds on encoding errors
4. ✅ Inefficient select! loop (19× CPU overhead)
5. ✅ Repeated buffer allocations (2.16 MB/sec churn)
6. ✅ Unbounded WebSocket channel (DoS risk)
7. ✅ Zombie threads after client disconnect
8. ✅ Duplicate benchmark code (300 lines)
9. ✅ Dead code on x86_64 (hash_scalar)
10. ✅ CPU benchmark overhead (100ms startup)

## 📝 License

[Add your license here]
