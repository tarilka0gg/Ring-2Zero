# Screen Streamer - Development Guide

Посібник з розробки, troubleshooting та історія оптимізацій

Версія: 1.0 | Дата: 2026-06-15

---

## 📋 Зміст

1. [Troubleshooting](#troubleshooting)
2. [Development Guide](#development-guide)
3. [Історія оптимізацій](#історія-оптимізацій)
4. [Build Configuration](#build-configuration)

---

## Troubleshooting

### Performance issues

**Проблема**: FPS < 30

**Діагностика**:
```bash
# 1. Запустити profiler
./target/release/frame_profiler

# 2. Перевірити breakdown
# Шукати: який компонент займає найбільше часу
```

**Рішення**:
- **Diff > 1ms** → Перевірити zero-copy stats (має бути 88%+)
- **Encode > 20ms** → Зменшити webp_quality або tiles_x
- **Merge > 0.2ms** → Збільшити merge_gap
- **Convert > 2ms** → Перевірити чи використовується AVX2

**Детальна діагностика**:
```bash
# Hash analyzer - порівняння швидкості хеш-функцій
./target/release/hash_analyzer

# Diff profiler - тільки diff detection
./target/release/diff_profiler

# Modern hash benchmark - альтернативні хеші
./target/release/modern_hash_bench
```

### Memory leak

**Проблема**: RSS зростає

**Діагностика**:
```bash
# Моніторинг пам'яті
watch -n 1 'ps aux | grep screen-streamer'

# Heap profiler (потребує valgrind)
valgrind --tool=massif ./target/release/screen-streamer
```

**Перевірити**:
- EncodingPool::Drop викликається коректно
- Немає `Rc` cycles
- WebSocket channel bounded (не unbounded)
- Cached encoded tiles не накопичуються безмежно

**Відомі виправлені issues**:
- ✅ Memory leak в encoding_pool (16-20 MB per client) - виправлено 2026-06-13
- ✅ Repeated buffer allocations (2.16 MB/sec) - виправлено через reuse

### High CPU usage

**Проблема**: CPU > 50%

**Причини**:
1. **Забагато tiles** → Зменшити tiles_x до 20-30
2. **Високий target_fps** → Знизити до 24
3. **Zombie threads** → Перевірити `ps -eLf | grep screen-streamer`
4. **Inefficient loop** → Перевірити чи немає `tokio::select!` з порожньою else

**Моніторинг**:
```bash
# CPU usage per thread
top -H -p $(pgrep screen-streamer)

# Системний профайлер (якщо є perf)
perf record -F 99 -p $(pgrep screen-streamer) -- sleep 10
perf report
```

**Оптимізації для зниження CPU**:
- Використовувати AVX2 (автоматично на i7-14650HX)
- Збільшити static_tile_fps (менше static tiles)
- Зменшити tiles_x (менше tiles загалом)
- Вимкнути debug_mode

### High latency

**Проблема**: Затримка > 100ms

**Діагностика**:
```bash
# Перевірити latency metrics у браузері (F12)
# Дивитись на "Латентність" у статус-барі
```

**Причини та рішення**:

1. **Pipeline буферизація**
   - Перевірити: channel buffer size = 0 (rendezvous)
   - Файл: `stream.rs:61`

2. **Великі пакети**
   - Зменшити MAX_PACKET_SIZE до 8000 bytes
   - Файл: `stream.rs:220`

3. **STUN сервери для локального з'єднання**
   - Видалити ice_servers для localhost
   - Файл: `main.rs:116-127`

4. **Ordered DataChannel**
   - Використовувати `ordered: false`
   - Файл: `main.rs:172-176`

5. **Canvas rendering**
   - Клієнт: використовувати `desynchronized: true`
   - Файл: `index-optimized.html`

**Тестування латентності**:
```bash
# Скрипт для тесту
./docs/scripts/test-latency.sh
```

### WebRTC не підключається

**Проблема**: Client не отримує дані

**Checklist**:
- ✅ Firewall: `sudo ufw allow 9001/tcp`
- ✅ Server running: `ps aux | grep screen-streamer`
- ✅ Port listening: `netstat -tlnp | grep 9001`
- ✅ Browser console: No errors
- ✅ ICE candidates: Перевірити в F12 console

**Детальна діагностика**:
```bash
# Логи сервера
tail -f /tmp/screen-streamer.log

# Перевірити WebRTC connection
# У браузері (F12 Console):
pc.getStats().then(stats => {
    stats.forEach(report => console.log(report));
});
```

**Відомі помилки (нормальні)**:
- ⚠️ "Failed to add ICE candidate" - клієнт відправляє candidates до remote description
- ⚠️ "STUN request timed out" - якщо STUN сервери недоступні (не критично для localhost)

**Якщо не працює взагалі**:
```bash
# Перезапустити сервер
pkill screen-streamer
./target/release/screen-streamer > /tmp/screen-streamer.log 2>&1 &

# Перевірити логи
cat /tmp/screen-streamer.log

# Тестувати з різними клієнтами
firefox http://localhost:9001/index.html          # Оригінальний
firefox http://localhost:9001/index-fixed.html    # З фіксами
firefox http://localhost:9001/index-optimized.html # Оптимізований
```

### Build errors

**Проблема**: Compilation fails

**Типові помилки**:

1. **Missing dependencies**:
```bash
# Wayland development headers
sudo apt install libwayland-dev  # Debian/Ubuntu
sudo pacman -S wayland           # Arch
emerge wayland                   # Gentoo
```

2. **SIMD compilation errors**:
```bash
# Перевірити target features
rustc --print target-features

# Якщо AVX2 недоступний - код автоматично fallback на SSE2
```

3. **Linking errors**:
```bash
# Перевірити linker
echo $CC
echo $CXX

# Gentoo: переконатись що clang встановлений
emerge sys-devel/clang
```

---

## Development Guide

### Додавання нової оптимізації

**Процес**:

1. **Benchmark baseline**:
```bash
./target/release/detailed_bench > before.txt
```

2. **Імплементувати зміни**

3. **Benchmark після**:
```bash
cargo build --release --bin detailed_bench
./target/release/detailed_bench > after.txt
```

4. **Порівняти**:
```bash
diff before.txt after.txt
```

5. **Документувати**:
- Додати в `DEVELOPMENT.md` (розділ "Історія оптимізацій")
- Оновити performance metrics у `DOCUMENTATION.md`

### Додавання нового benchmark

**Створити файл** `src/bin/my_bench.rs`:
```rust
use screen_streamer::*;
use std::time::Instant;

fn main() {
    // Setup
    let config = Config::default();
    let mut detector = DiffDetector::new(config);
    
    // Test data
    let frame = create_test_frame(1920, 1080);
    
    // Benchmark loop
    let start = Instant::now();
    for i in 0..1000 {
        let (tiles, _) = detector.detect_changes(&frame);
    }
    let elapsed = start.elapsed();
    
    // Report results
    println!("Total: {:?}", elapsed);
    println!("Avg: {:.2}ms", elapsed.as_secs_f64() * 1000.0 / 1000.0);
}

fn create_test_frame(width: u32, height: u32) -> Frame {
    Frame {
        rgba: vec![0u8; (width * height * 4) as usize],
        width,
        height,
        damage_regions: vec![],
    }
}
```

**Додати в** `Cargo.toml`:
```toml
[[bin]]
name = "my_bench"
path = "src/bin/my_bench.rs"
```

**Запустити**:
```bash
cargo build --release --bin my_bench
./target/release/my_bench
```

### Code review checklist

Перед commit перевірити:

- [ ] Додано тести (якщо потрібно)
- [ ] Benchmark показує покращення
- [ ] Немає unsafe без коментарів
- [ ] Документація оновлена (DOCUMENTATION.md, API_REFERENCE.md)
- [ ] Не додано нових dependencies без вагомої причини
- [ ] `cargo check` без warnings
- [ ] `cargo clippy` без помилок
- [ ] `cargo fmt` застосований

**Commands**:
```bash
# Check
cargo check

# Clippy (linter)
cargo clippy -- -W clippy::all

# Format
cargo fmt

# Test (якщо є)
cargo test

# Build і verify
cargo build --release
./target/release/screen-streamer --help
```

### Commit message format

```
<type>: <short summary>

<optional body>

<optional footer>
```

**Types**: 
- `feat` - нова функціональність
- `fix` - виправлення багу
- `perf` - оптимізація performance
- `refactor` - рефакторинг без зміни поведінки
- `docs` - зміни в документації
- `test` - додавання тестів
- `chore` - інші зміни (build, ci, etc.)

**Приклад**:
```
perf: replace VecDeque with CircularBuffer in TileMetadata

Reduces metadata size from 32 bytes to 10 bytes.
Operations are now bitwise instead of iterator-based.

Benchmark: 10-25% faster diff detection
- Static: 0.39 → 0.33 ms (14% faster)
- Light: 0.34 → 0.31 ms (7% faster)
- Heavy: 0.39 → 0.31 ms (21% faster)
```

### Adding new features

**Checklist для нової фічі**:

1. **Дизайн**:
   - Написати короткий design doc у цьому файлі
   - Обговорити trade-offs
   - Оцінити вплив на performance

2. **Імплементація**:
   - Дотримуватись існуючого стилю коду
   - Використовувати існуючі abstractions
   - Додати unsafe коментарі якщо потрібно

3. **Тестування**:
   - Unit tests (якщо можливо)
   - Integration benchmark
   - Manual testing з реальним клієнтом

4. **Документація**:
   - API documentation (/// comments)
   - User documentation (DOCUMENTATION.md)
   - API reference (API_REFERENCE.md)

### SIMD code guidelines

Коли писати SIMD код:

**✅ Добрі кандидати**:
- Обробка великих масивів даних
- Прості операції (XOR, ADD, compare)
- Hot path (викликається часто)
- CPU-bound операції

**❌ Погані кандидати**:
- Складна логіка з багатьма branches
- Малі масиви (< 64 bytes)
- IO-bound операції
- Код що рідко викликається

**Template для SIMD функції**:
```rust
#[cfg(target_arch = "x86_64")]
pub fn process_data(data: &[u8]) -> Vec<u8> {
    if is_x86_feature_detected!("avx2") {
        unsafe { process_data_avx2(data) }
    } else if is_x86_feature_detected!("sse2") {
        unsafe { process_data_sse2(data) }
    } else {
        process_data_scalar(data)
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn process_data(data: &[u8]) -> Vec<u8> {
    process_data_scalar(data)
}

#[target_feature(enable = "avx2")]
unsafe fn process_data_avx2(data: &[u8]) -> Vec<u8> {
    // AVX2 implementation
}

#[target_feature(enable = "sse2")]
unsafe fn process_data_sse2(data: &[u8]) -> Vec<u8> {
    // SSE2 implementation
}

fn process_data_scalar(data: &[u8]) -> Vec<u8> {
    // Scalar fallback
}
```

---

## Історія оптимізацій

### 2026-06-16: Performance Optimization Round

**Протестовано 7 оптимізацій, залишено 2 успішні:**

#### ✅ Успішні оптимізації (впроваджені):

**1. Cache hits fix для merged tiles**
- **Зміни**: Хешування merged tiles замість оригінальних tile indices
- **Проблема**: Після tile merging координати змінюються, тому хеш оригінального tile ≠ хеш merged tile
- **Рішення**: 
  - Перехешування merged tiles перед перевіркою cache
  - Додано метод `get_all_metadata()` до DiffDetector
- **Результати**: 
  - Cache hit rate: 56-68% у realistic сценаріях
  - Static: 95% cache hits
  - Video: 68.5% cache hits
- **Files changed**: `src/diff.rs`, `src/stream.rs`

**2. TileBufferPool - reusable tile buffers**
- **Зміни**: Pre-allocated buffer pool замість allocation кожного фрейму
- **Реалізація**:
  - Новий модуль `tile_buffer_pool.rs`
  - Thread-safe pool з Arc<Mutex<Vec<Vec<u8>>>>
  - Pre-allocate 50 buffers розміром 48×27×4 bytes
- **Результати**:
  - Frame processing: швидше на 15-100% залежно від сценарію
  - Менше memory churn
  - Tile Extraction: 0.44ms → 0.41ms
- **Files changed**: `src/tile_buffer_pool.rs`, `src/stream.rs`, `src/lib.rs`

#### ❌ Відкачені оптимізації (неефективні):

**3. SIMD tile extraction**
- **Причина**: SIMD overhead > виграш для малих тайлів (48×27 px)
- **Результат**: +24% повільніше
- **Висновок**: `copy_from_slice` вже добре оптимізована компілятором

**4. Adaptive network batching**
- **Причина**: Benchmark не вимірює network overhead
- **Результат**: +12% повільніше через додаткові перевірки

**5. Cache priority calculation**
- **Причина**: Priority calculation вже займає 0.00ms, overhead на dirty flags
- **Результат**: +12% повільніше

**6. Dynamic merge gap**
- **Причина**: Створення TileMerger кожен фрейм
- **Результат**: -4% до -17% повільніше

**7. Multi-resolution encoding coefficient**
- **Причина**: Різні quality values → менше cache hits
- **Результат**: +17% повільніше

**Фінальні результати (frame_profiler):**

| Сценарій | До оптимізацій | Після оптимізацій | Покращення |
|----------|---------------|-------------------|------------|
| Light (5%) | 1.37 ms (730 FPS) | 0.67 ms (1493 FPS) | **+104%** |
| Medium (20%) | 1.35 ms (740 FPS) | 1.15 ms (870 FPS) | **+18%** |
| Heavy (50%) | 1.35 ms (740 FPS) | 1.18 ms (847 FPS) | **+15%** |

**Ключові висновки:**
- Тільки **2 з 7** оптимізацій дали реальний виграш
- Найбільший ефект: зменшення allocations та покращення cache locality
- Складні алгоритми часто додають overhead без виграшу
- Для high-performance CPU (i7-14650HX) простіші підходи працюють краще

**Files changed:**
- `src/diff.rs` - cache hits fix, додано `get_all_metadata()`
- `src/stream.rs` - TileBufferPool integration
- `src/tile_buffer_pool.rs` - новий модуль
- `src/lib.rs` - export tile_buffer_pool
- `src/tile.rs` - minor cleanup

**Commit**: `perf: cache hits fix + TileBufferPool (15-100% faster)`

### 2026-06-15: Metadata Optimization

**Зміни**:
- Замінено VecDeque на CircularBuffer (u64 bitfield)
- 64 frames історії в 8 байтах замість 32+ bytes

**Результати**:
- Diff detection: +10-25% швидше
- FPS: 411-1337 (було 372-1067)
- Memory: -22 bytes per tile

**Files changed**:
- `src/tile.rs` - додано CircularBuffer
- `src/diff.rs` - використання CircularBuffer замість VecDeque

**Commit**: `perf: replace VecDeque with CircularBuffer in TileMetadata`

### 2026-06-13: Bug Fixes & Consolidation

**10 виправлених багів**:

1. ✅ Memory leak в encoding_pool (16-20 MB per client)
2. ✅ Race condition в tile_metadata
3. ✅ Index out of bounds на encoding errors
4. ✅ Inefficient select! loop (19× CPU overhead)
5. ✅ Repeated buffer allocations (2.16 MB/sec churn)
6. ✅ Unbounded WebSocket channel (DoS risk)
7. ✅ Zombie threads після disconnect
8. ✅ Duplicate benchmark code (300 lines)
9. ✅ Dead code on x86_64 (hash_scalar)
10. ✅ CPU benchmark overhead (100ms startup)

**Результати**:
- Стабільність: no crashes, no leaks
- CPU: -6% від zombie threads
- Startup: -100ms від CPU benchmark cache

**Files changed**:
- `src/encoding_pool.rs` - fixed Drop impl
- `src/diff.rs` - fixed race condition
- `src/stream.rs` - fixed inefficient loop, bounded channel
- `src/bin/` - consolidated benchmark code

### 2026-06-12: SIMD Optimizations

**Зміни**:
1. AVX2 hashing implementation
2. SIMD batch operations (comparison, increment)
3. Parallel diff detection з rayon
4. Encoding thread pool
5. SIMD color conversion (BGRX→RGBA)

**Результати**:
- Hash: 2-3× швидше (AVX2)
- Comparison: 2.5× швидше (batch ops)
- Metadata: 3× швидше (parallel)
- Conversion: 3.3× швидше (AVX2)
- Загалом: 46ms → 16.5ms per frame

**Files changed**:
- `src/tile.rs` - AVX2/SSE2 hashing, batch ops
- `src/diff.rs` - parallel metadata update
- `src/convert.rs` - AVX2 color conversion
- `src/encoding_pool.rs` - new file для worker pool

**Commits**:
- `perf: add AVX2 SIMD hashing`
- `perf: add SIMD batch operations`
- `perf: parallelize diff detection`
- `feat: add encoding thread pool`
- `perf: add AVX2 color conversion`

### 2026-06-11: Tile Merging & Adaptive FPS

**Зміни**:
1. Tile merging algorithm (97-99% reduction)
2. Adaptive FPS (dynamic 32 FPS, static 8 FPS)
3. Priority-based tile sorting
4. Zero-copy optimization (half hash)

**Результати**:
- Video window: 168 → 1 tile (99.4% reduction)
- Zero-copy: 88-99% tiles skipped
- Traffic: -60% для static контенту

**Files changed**:
- `src/encoder.rs` - TileMerger implementation
- `src/diff.rs` - adaptive FPS logic, half hash
- `src/stream.rs` - priority sorting

### 2026-06-10: WebRTC Latency Fixes

**Зміни**:
1. Rendezvous channel (buffer=0)
2. Smaller packets (16KB → 8KB)
3. No STUN для localhost
4. Unordered DataChannel
5. Desynchronized canvas на клієнті

**Результати**:
- Latency: -139-621ms
- Особливо для першого підключення

**Files changed**:
- `src/stream.rs` - buffer=0, MAX_PACKET_SIZE=8000
- `src/main.rs` - ICE config, DataChannel config
- `docs/client-examples/index-optimized.html` - desynchronized canvas

### Initial Implementation (2026-05-XX)

**Базова функціональність**:
- Wayland wlr-screencopy capture
- Tile-based diff detection
- WebP encoding
- WebRTC DataChannel streaming

**Performance baseline**:
- 46ms per frame = 21.7 FPS
- CPU: 50-60%

---

## Build Configuration

### Compiler flags (make.conf)

```bash
# Gentoo /etc/portage/make.conf
CC="clang"
CXX="clang++"
AR="llvm-ar"
NM="llvm-nm"
RANLIB="llvm-ranlib"

COMMON_FLAGS="-O3 -march=native -mtune=native -flto=thin -fomit-frame-pointer"
CFLAGS="${COMMON_FLAGS}"
CXXFLAGS="${COMMON_FLAGS}"
LDFLAGS="-Wl,-O2 -Wl,--as-needed -fuse-ld=lld -flto=thin"

MAKEOPTS="-j24 -l24"
```

### Cargo configuration

```toml
# Cargo.toml
[profile.release]
opt-level = 3              # Максимальна оптимізація
lto = "thin"               # Link-time optimization
codegen-units = 1          # Краща оптимізація, повільніша збірка
panic = "unwind"           # Stack unwinding для debugging
strip = false              # Не видаляти symbols (для profiling)

[profile.release-stripped]
inherits = "release"
strip = true               # Видалити symbols (менший binary)
```

### Environment variables

```bash
# ~/.config/fish/config.fish
export RUSTC_WRAPPER=sccache          # Кешування компіляції
export MOZ_MAKE_FLAGS="-j12"
export MAKEFLAGS="-j12"

```

### Dependencies

```toml
# Cargo.toml
[dependencies]
wayland-client = "0.31"
wayland-protocols-wlr = { version = "0.3", features = ["client"] }
libc = "0.2"
rayon = "1"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.24"
futures-util = "0.3"
webrtc = "0.17"
bytes = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
webp = "0.3"
xxhash-rust = { version = "0.8", features = ["xxh3"] }
crossbeam = "0.8"
num_cpus = "1.0"
```

### CPU detection

```rust
// Runtime CPU feature detection
if is_x86_feature_detected!("avx2") {
    // Use AVX2 code path
} else if is_x86_feature_detected!("sse2") {
    // Use SSE2 code path
} else {
    // Use scalar code path
}
```

**System capabilities**:
```bash
# Перевірити CPU features
grep flags /proc/cpuinfo | head -1

# На i7-14650HX:
# ✅ avx2, sse2, sse4_1, sse4_2
# ❌ avx512 (не підтримується)
```

---

**Версія**: 1.0  
**Дата**: 2026-06-15  
**Автор**: tarilka0gg
