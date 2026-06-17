# Screen Streamer

High-performance Wayland screen streaming server with WebRTC support.

## 🚀 Features

- **Wayland wlr-screencopy protocol** for efficient screen capture
- **Tile-based encoding** with intelligent change detection
- **WebP compression** with adaptive quality
- **WebRTC DataChannel** streaming
- **Tile merging** optimization (97-99% reduction in typical scenarios)
- **Zero-copy hashing** with SIMD (AVX2/SSE2)
- **Parallel encoding** pool with worker threads
- **Persistent CPU benchmark cache**

## 📊 Performance

Real-world benchmarks with tile merging (June 17, 2026, v0.160, averaged over 10 runs):

| Scenario | Time/Frame | FPS | vs Target (32 FPS) |
|----------|-----------|-----|-------------------|
| 🟢 Static content | 0.37 ms | **2732 FPS** | 85.4× faster |
| 🟡 Moderate activity | 0.44 ms | **2271 FPS** | 71.0× faster |
| 🟠 Active work | 0.96 ms | **1043 FPS** | 32.6× faster |
| 🔴 Video window | 0.65 ms | **1531 FPS** | 47.8× faster |

**Key optimizations:**
- **Tile grid optimization**: 20×20 grid (96×54px tiles) reduces overhead by 75% vs 40×40
- **Tile merging**: 83-99% tile reduction (e.g., 20627 → 247 tiles)
- **Cache hits**: 41-67% tiles served from cache
- **Zero-copy hashing**: 54-99% tiles skipped, ~27-50% CPU savings
- **Adaptive FPS**: Dynamic 32 FPS for changed content, 4 FPS for static
- **Variance**: <4% across multiple runs (highly stable)
- **Thread safety**: All data races fixed with snapshot pattern (+9% performance boost)

## 🔧 Building

```bash
cargo build --release
```

## 🎯 Running

```bash
# Start the server
./target/release/screen-streamer

# Run benchmark
./target/release/detailed_bench

# Test CPU speed (for adaptive config)
rustc test_benchmark.rs && ./test_benchmark
```

## 📁 Project Structure

```
.
├── src/
│   ├── main.rs              - WebRTC server entry point
│   ├── stream.rs            - Streaming logic with tile merging
│   ├── capture.rs           - Wayland screen capture
│   ├── diff.rs              - Change detection with zero-copy hashing
│   ├── encoder.rs           - WebP encoding & tile merging
│   ├── encoding_pool.rs     - Parallel encoding worker pool
│   ├── tile.rs              - SIMD tile hashing (AVX2/SSE2)
│   ├── config.rs            - Configuration with CPU detection
│   └── bin/
│       └── detailed_bench.rs - Realistic benchmark tool
├── test_benchmark.rs        - CPU speed detection
└── docs/
    ├── client-examples/     - HTML WebRTC client examples
    ├── scripts/             - Benchmark scripts and logs
    ├── OPTIMIZATIONS.md     - Optimization techniques
    ├── TESTING.md           - Testing procedures
    └── *.md                 - Technical documentation
```

## 📖 Documentation

- [Architecture Overview](docs/SMM_ARCHITECTURE.md)
- [Optimizations Applied](docs/OPTIMIZATIONS_APPLIED.md)
- [Encoding Pool Design](docs/ENCODING_POOL.md)
- [SIMD Optimizations](docs/SIMD_CONVERSION.md)
- [Latency Fixes](docs/LATENCY_FIX.md)
- [Testing Guide](docs/TESTING.md)

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

All fixes verified with benchmarks showing 372-1067 FPS in realistic scenarios.

## 📝 License

[Add your license here]
