#!/bin/bash
# Benchmark script для порівняння оптимізацій

echo "=== Screen Streamer Performance Benchmark ==="
echo "Date: $(date)"
echo "System: $(uname -a)"
echo ""

# Перевірка чи є старий binary для порівняння
if [ -f "target/release/screen-streamer.old" ]; then
    echo "📊 Comparing OLD vs NEW version"
    echo ""
else
    echo "⚠️  No old binary found. Running only new version."
    echo "   To compare, copy current binary: cp target/release/screen-streamer target/release/screen-streamer.old"
    echo ""
fi

# CPU info
echo "🖥️  CPU Information:"
lscpu | grep "Model name" || echo "Model: $(cat /proc/cpuinfo | grep 'model name' | head -1 | cut -d: -f2)"
lscpu | grep "CPU MHz" | head -1 || echo "Frequency: $(cat /proc/cpuinfo | grep 'cpu MHz' | head -1 | cut -d: -f2) MHz"
echo ""

# SIMD support detection
echo "🚀 SIMD Support:"
if grep -q avx2 /proc/cpuinfo; then
    echo "   ✅ AVX2 available"
else
    echo "   ❌ AVX2 not available"
fi

if grep -q avx512 /proc/cpuinfo; then
    echo "   ✅ AVX-512 available"
else
    echo "   ⚠️  AVX-512 not available"
fi

if grep -q sse2 /proc/cpuinfo; then
    echo "   ✅ SSE2 available"
else
    echo "   ❌ SSE2 not available (unexpected!)"
fi
echo ""

# Binary size comparison
echo "📦 Binary Size:"
if [ -f "target/release/screen-streamer.old" ]; then
    OLD_SIZE=$(stat -c%s target/release/screen-streamer.old)
    NEW_SIZE=$(stat -c%s target/release/screen-streamer)
    echo "   Old: $(numfmt --to=iec-i --suffix=B $OLD_SIZE)"
    echo "   New: $(numfmt --to=iec-i --suffix=B $NEW_SIZE)"
    DIFF=$((NEW_SIZE - OLD_SIZE))
    if [ $DIFF -gt 0 ]; then
        echo "   📈 Increased by $(numfmt --to=iec-i --suffix=B $DIFF)"
    else
        echo "   📉 Decreased by $(numfmt --to=iec-i --suffix=B ${DIFF#-})"
    fi
else
    NEW_SIZE=$(stat -c%s target/release/screen-streamer)
    echo "   New: $(numfmt --to=iec-i --suffix=B $NEW_SIZE)"
fi
echo ""

echo "⏱️  Performance Test:"
echo "   Starting server for 30 seconds..."
echo "   Open browser at http://localhost:8080/index.html"
echo "   (Press Ctrl+C to stop early)"
echo ""

# Run server with timeout
timeout 30s ./target/release/screen-streamer 2>&1 | tee benchmark.log || true

echo ""
echo "📈 Results saved to benchmark.log"
echo ""
echo "Look for these metrics in the log:"
echo "  - [Zero-copy stats] Skipped: X% - higher is better"
echo "  - [Damage tracking] Skipped tiles - more skipped = faster"
echo "  - X тайлів / Y кбіт / Z мс - lower Z (ms) is better"
echo ""
echo "✅ Benchmark complete!"
