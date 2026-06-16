# Screen Streamer - Повна документація

**Високопродуктивний стрімер екрану через Wayland wlr-screencopy + WebRTC**

Версія: 3.1 | Дата: 2026-06-15 | FPS: 411-1337

---

## 📋 Зміст

1. [Огляд проекту](#огляд-проекту)
2. [Швидкий старт](#швидкий-старт)
3. [Архітектура](#архітектура)
4. [Оптимізації](#оптимізації)
5. [Performance](#performance)
6. [Встановлення та запуск](#встановлення-та-запуск)
7. [Тестування](#тестування)

---

## Огляд проекту

### Основні можливості

- ✅ **Wayland wlr-screencopy** - захоплення екрану через протокол Wayland
- ✅ **Tile-based encoding** - інтелектуальна детекція змін по тайлах
- ✅ **WebP compression** - адаптивна якість (0.5-8.0)
- ✅ **WebRTC DataChannel** - low-latency streaming
- ✅ **Tile merging** - 97-99% reduction (168→1 tile для відео)
- ✅ **Zero-copy hashing** - SIMD AVX2/SSE2 (88-99% tiles skipped)
- ✅ **Parallel encoding pool** - worker threads для WebP
- ✅ **Metadata optimization** - CircularBuffer замість VecDeque
- ✅ **Persistent cache** - CPU benchmark результати

### Системні вимоги

- **OS**: Linux з Wayland compositor (wlr-screencopy підтримка)
- **CPU**: x86_64 з SSE2 (AVX2 рекомендується)
- **RAM**: 50-200 MB
- **Kernel**: 3.17+ (memfd_create)

### Ключові досягнення

✅ **10 багів виправлено** (2026-06-13) - memory leak, race conditions, inefficient loops  
✅ **Metadata optimization** (2026-06-15) - VecDeque → CircularBuffer (+10-25% швидше)  
✅ **FPS: 411-1337** у реалістичних сценаріях  
✅ **Tile merging: 97-99%** reduction  
✅ **Zero-copy: 88-99%** tiles skipped  
✅ **Hash: 744 ns** (найшвидший з усіх тестованих)

---

## Швидкий старт

### Збірка проекту

```bash
# Перейти в директорію проекту
cd /home/tarilka0gg/Documents/projects/Adds/test-websoket-function

# Основний binary
cargo build --release

# З усіма benchmarks
cargo build --release --bins
```

### Запуск сервера

```bash
# Foreground
./target/release/screen-streamer

# Background
./target/release/screen-streamer > /tmp/screen-streamer.log 2>&1 &

# Перевірка
ps aux | grep screen-streamer
```

### Підключення клієнта

**Локально:**
```
http://localhost:9001/index-optimized.html
```

**Віддалено:**
```bash
# Дізнатись IP
ip addr show | grep "inet " | grep -v 127.0.0.1

# На іншому пристрої
http://[IP]:9001/index-optimized.html
```

**Firewall:**
```bash
sudo ufw allow 9001/tcp
```

### Benchmarks

```bash
# Realistic benchmark (4 scenarios)
./target/release/detailed_bench

# Frame profiler (детальний breakdown)
./target/release/frame_profiler

# Diff profiler (тільки diff detection)
./target/release/diff_profiler

# Hash analyzer (порівняння хеш-функцій)
./target/release/hash_analyzer

# CPU speed test
rustc test_benchmark.rs && ./test_benchmark
```

---

## Архітектура

### Структура модулів

```
src/
├── main.rs              - WebRTC server entry point
├── stream.rs            - Streaming pipeline з tile merging
├── capture.rs           - Wayland wlr-screencopy захоплення
├── diff.rs              - Change detection (SIMD hashing + metadata)
├── encoder.rs           - WebP encoding + TileMerger
├── encoding_pool.rs     - Parallel worker pool
├── tile.rs              - SIMD tile hashing (AVX2/SSE2) + CircularBuffer
├── config.rs            - Configuration з CPU detection
├── frame.rs             - Frame structure (RGBA + dimensions)
├── shm.rs               - Shared memory (memfd + mmap)
├── convert.rs           - SIMD color conversion
└── error.rs             - Error types
```

### Pipeline обробки кадру

```
1. CAPTURE (Wayland)
   ↓ wlr-screencopy → shared memory
   
2. DIFF DETECTION (~0.31-0.38 ms)
   ├─ Half hash (50% даних) → zero-copy skip якщо ==
   ├─ Full hash (AVX2/SSE2) → тільки якщо half hash !=
   ├─ SIMD comparison (prev vs new hashes)
   └─ Metadata update (CircularBuffer для history)
   
3. TILE MERGING (~0.03-0.12 ms)
   ├─ Групування сусідніх tiles
   ├─ 168 tiles → 1 merged tile (для відео)
   └─ Результат: 97-99% reduction
   
4. PRIORITY SORTING (~0.00 ms)
   ├─ Dynamic tiles (32 FPS) > Static tiles (8 FPS)
   ├─ Center > edges
   └─ High frequency > low frequency
   
5. PARALLEL ENCODING (~0.67-12 ms залежно від tiles)
   ├─ Encoding pool з N worker threads
   ├─ WebP encode (quality 0.5-8.0)
   ├─ Cache check/update
   └─ ~0.5 ms per tile
   
6. NETWORK SEND (WebRTC DataChannel)
   ├─ Batch до 8 KB packets
   ├─ Header (6 bytes) + tiles
   └─ MTU-aware fragmentation
```

### Ключові структури даних

#### TileMetadata

```rust
pub struct TileMetadata {
    unchanged_frames: u32,
    last_sent_frame: u64,
    is_dynamic: bool,
    last_sent_as_dynamic: bool,
    change_history: CircularBuffer,  // u64 bitfield!
    update_frequency: f32,
    last_hash_diff: u64,
    prev_half_hash: u64,
    cached_encoded: Option<Vec<u8>>,
    cached_hash: u64,
}
```

**Оптимізація**: `CircularBuffer` - 64 frames історії в 8 байтах (u64 bitfield)
- Було: VecDeque (~32 bytes + allocation)
- Стало: 10 bytes total
- Операції: bitwise shift замість push_back/pop_front
- count_ones(): hardware instruction замість iter().filter().count()

#### CircularBuffer

```rust
pub struct CircularBuffer {
    data: u64,      // Bitfield (true/false = 1/0 bit)
    size: u8,       // Поточна кількість
    capacity: u8,   // Max 64 frames
}
```

**API**:
- `push(value: bool)` - додати значення, зсунути старі
- `count_ones()` - кількість true (hardware instruction)
- `len()` - поточний розмір

---

## Оптимізації

### 10 Виправлених багів (2026-06-13)

#### Критичні (3)

**#1: Memory leak в encoding_pool** (16-20 MB per client)
- **Причина**: `drop(self.task_tx.clone())` не закривав канал
- **Виправлення**: Видалено Drop impl, Rust автоматично drop'ає
- **Результат**: Workers коректно завершуються

**#2: Race condition в tile_metadata**
- **Причина**: Parallel read tile_metadata під час update
- **Виправлення**: Pre-clone cache data перед parallel work
- **Результат**: No data corruption

**#3: Index out of bounds**
- **Причина**: Worker panic → encoded[i] залишається порожнім
- **Виправлення**: Validation + fallback synchronous encoding
- **Результат**: Завжди валідні дані

#### Середні (3)

**#4: Inefficient select! loop** (19× CPU overhead)
- **Причина**: `tokio::select!` з порожньою else
- **Виправлення**: `while let` замість select!
- **Результат**: Правильне блокування

**#5: Repeated allocations** (2.16 MB/sec churn)
- **Причина**: `Vec::with_capacity()` на кожен frame
- **Виправлення**: Reuse buffers через Mutex (clone для Send)
- **Результат**: Менше GC pressure

**#6: Unbounded WebSocket channel** (DoS risk)
- **Причина**: `unbounded_channel()` → OOM якщо client slow
- **Виправлення**: `channel(32)` bounded queue
- **Результат**: Backpressure protection

#### Незначні (4)

- **#7**: Zombie threads (6% CPU waste)
- **#8**: Duplicate benchmark code (300 lines)
- **#9**: Dead code hash_scalar (8 KB bloat)
- **#10**: CPU benchmark overhead (100ms startup)

### Metadata Optimization (2026-06-15)

**VecDeque → CircularBuffer**
- **Виграш**: 10-25% швидше diff detection
- **Пам'ять**: 32 bytes → 10 bytes
- **Операції**: 
  - push_back/pop_front → bitwise shift
  - iter().filter().count() → count_ones() (1 CPU instruction)

**Результати**:
- Static: 0.39 → 0.33 ms (**14% швидше**)
- Light: 0.34 → 0.31 ms (**7% швидше**)
- Heavy: 0.39 → 0.31 ms (**21% швидше**)

### SIMD Optimizations

#### AVX2/SSE2 Hashing

```rust
// AVX2: 32 bytes per cycle
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(data: &[u8]) -> u64 {
    let mut acc = _mm256_setzero_si256();
    let mut seed = _mm256_set1_epi64x(0x9e3779b97f4a7c15);
    
    for chunk in data.chunks_exact(32) {
        let v = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);
        acc = _mm256_xor_si256(acc, v);
        seed = _mm256_add_epi64(seed, _mm256_set1_epi64x(0x9e3779b97f4a7c15));
        acc = _mm256_xor_si256(acc, seed);
    }
    
    // Horizontal reduction 256→64 bit
    // ...
}
```

**Performance**: 744 ns per hash (найшвидший серед усіх тестованих)

**Альтернативи тестовані**:
- xxHash3-64: 1,634 ns (2× повільніше)
- SplitMix64: 17,728 ns
- FNV-1a: 44,045 ns

**Вибір**: Залишили AVX2 Current - найшвидший + 0 false negatives

#### SIMD Batch Operations

**Hash comparison** (4 hashes одночасно):
```rust
unsafe fn find_changed_tiles_avx2(prev: &[u64], new: &[u64]) -> Vec<usize>
```

**Counter increment** (8 counters одночасно):
```rust
unsafe fn increment_counters_avx2(counters: &mut [u32])
```

**Результат**: 
- Hash comparison: 0.5ms → 0.2ms (2.5× швидше)
- Metadata update: 0.3ms → 0.1ms (3× швидше)

#### SIMD Color Conversion

**BGRX → RGBA** з AVX2 (8 pixels одночасно):
```rust
#[target_feature(enable = "avx2")]
unsafe fn convert_bgrx_to_rgba_avx2(src: &[u8], dst: &mut [u8])
```

**Результат**: 5ms → 1.5ms (3.3× швидше) @ 1920×1080

#### Zero-copy optimization

- **Half hash** (50% даних) для швидкої перевірки
- Skip full hash якщо half hash == prev
- **Результат**: 88-99% tiles skipped (44-50% CPU savings)

### Parallel Encoding Pool

**До**:
```rust
// Synchronous encoding з rayon
let encoded = tile_encoder.encode_tiles_with_cache(tiles, ...);
```

**Після**:
```rust
// Persistent worker pool
let pool = EncodingPool::new(num_cpus::get().max(4));

for tile in tiles {
    pool.submit(EncodingTask { tile, data });
}

let results = pool.collect_results(tiles.len());
```

**Виграш**:
- Thread spawn overhead: 0ms (reused workers)
- Encoder creation: 0ms (cached per worker)
- Overlap з diff detection: +bonus performance

### Tile Merging

**Алгоритм**:
1. Групувати vertical runs в кожній колонці
2. Групувати runs з однаковим (ty_start, ty_end)
3. Створити horizontal merged rectangles

**Результати**:
- Video window 640×480: 168 tiles → 1 tile (**99.4%** reduction)
- Horizontal strip: 280 tiles → 1 tile (**99.6%** reduction)
- Text typing: 116 tiles → 3 tiles (**97.4%** reduction)

---

## Performance

### Фінальні benchmarks (2026-06-16)

**Після раунду оптимізацій (+15-100% покращення):**

#### Frame Profiler (реальне WebP encoding):

| Сценарій | До оптимізацій | Після | Покращення |
|----------|---------------|-------|------------|
| 🟢 **Light (5%)** | 1.37 ms (730 FPS) | **0.67 ms (1493 FPS)** | **+104%** 🚀 |
| 🟡 **Medium (20%)** | 1.35 ms (740 FPS) | **1.15 ms (870 FPS)** | **+18%** |
| 🟠 **Heavy (50%)** | 1.35 ms (740 FPS) | **1.18 ms (847 FPS)** | **+15%** |

**Ключові оптимізації:**
- ✅ **Cache hits fix**: Хешування merged tiles → 56-95% cache hit rate
- ✅ **TileBufferPool**: Reusable buffers → менше allocations

#### Detailed Bench (100 frames, realistic scenarios):

| Сценарій | Tiles (до→після merge) | Cache hits | Total (ms) | **FPS** | vs Target |
|----------|------------------------|-----------|------------|---------|-----------|
| 🟢 **Static** | 0 → 0 | 0% | **0.92** | **1087** | **34× швидше** |
| 🟡 **Moderate** | 58 → 4 (93%) | 66.6% | **1.07** | **932** | **29× швидше** |
| 🟠 **Active** | 724 → 13 (98%) | 56.5% | **2.61** | **383** | **12× швидше** |
| 🔴 **Video** | 167 → 24 (86%) | 68.5% | **1.50** | **666** | **21× швидше** |

**Target**: 32 FPS (31.2 ms/frame) - перевищено у **12-34 рази**!

### Breakdown (Heavy scenario з frame_profiler)

```
Total: 1.18 ms → 847 FPS
├── Diff Detection:    0.45 ms (38.4%)
├── Tile Merging:      0.04 ms (3.4%)
├── WebP Encoding:     0.68 ms (57.5%) - тільки 2 tiles!
├── Tile Extraction:   0.00 ms (0.2%)
└── Cache hits:        ~0% (test data змінюється)
```

**Чому так швидко?**
- Zero-copy hashing: 54-99% tiles skipped
- Tile merging: 85-98% reduction (724 → 13 tiles)
- Cache hits: 56-95% у realistic scenarios
- TileBufferPool: reusable buffers
- Parallel encoding pool
- AVX2 SIMD hashing

### Еволюція performance

```
BASELINE (original):
  46ms/frame = 21.7 FPS

+ AVX2 hashing + Adaptive FPS:
  26ms/frame = 38.5 FPS

+ SIMD Batch + Parallel + Pool:
  20ms/frame = 50 FPS

+ SIMD Conversion:
  16.5ms/frame = 60 FPS

+ Metadata CircularBuffer:
  1.35ms/frame = 740 FPS

+ Cache hits fix + TileBufferPool (фінальне):
  0.67-1.18ms/frame = 847-1493 FPS 🚀
```

**Загальний speedup**: 46ms → 0.67ms = **до 69× швидше!**

### Протестовані та відкачені оптимізації

**❌ Неефективні** (2026-06-16):
1. SIMD tile extraction (+24% slower) - overhead > виграш
2. Adaptive network batching (+12% slower) - додаткові перевірки
3. Cache priority calculation (+12% slower) - overhead на dirty flags
4. Dynamic merge gap (-17% slower) - allocation кожен фрейм
5. Multi-resolution coefficient (+17% slower) - менше cache hits

**Висновок**: Тільки 2 з 7 оптимізацій дали реальний виграш. Для high-performance CPU простіші підходи (менше allocations, краща cache locality) працюють краще за складні алгоритми.

---

## Встановлення та запуск

### Конфігурація

```rust
Config {
    ws_port: 9001,
    target_fps: NonZeroU32::new(32),     // Базовий FPS
    tiles_x: 40,                          // Кількість tiles по X
    webp_quality_low: 0.5,                // Якість для dynamic tiles
    webp_quality_high: 8.0,               // Якість для static tiles
    merge_gap: 0,                         // Gap для tile merging
    dynamic_tile_fps: NonZeroU32::new(32), // FPS для dynamic
    static_tile_fps: NonZeroU32::new(8),   // FPS для static
    
    // Priority weights
    priority_frequency_weight: 0.5,
    priority_speed_weight: 0.3,
    priority_center_weight: 0.2,
    priority_history_window: 32,          // Circular buffer size
    
    debug_mode: false,
}
```

### Client examples

Доступні HTML приклади в `docs/client-examples/`:
- `index.html` - оригінальний клієнт
- `index-fixed.html` - з фіксами WebRTC
- `index-optimized.html` - оптимізований клієнт (рекомендується)

### Scripts

Benchmark скрипти в `docs/scripts/`:
- `benchmark.sh` - основний benchmark
- `test-latency.sh` - тест латентності
- `benchmark.log` - результати

---

## Тестування

### Локальне тестування

```bash
# 1. Запустити сервер
./target/release/screen-streamer

# 2. Відкрити клієнт
firefox http://localhost:9001/index-optimized.html

# 3. Перевірити логи
tail -f /tmp/screen-streamer.log
```

### Віддалене тестування

```bash
# 1. Дізнатись IP
ip addr show | grep "inet " | grep -v 127.0.0.1

# 2. На іншому пристрої
firefox http://[IP]:9001/index-optimized.html

# 3. Firewall
sudo ufw allow 9001/tcp
```

### Що перевіряти

✅ **Швидке підключення** - 5-10 секунд  
✅ **Стабільність** - без обривів  
✅ **Auto-reconnect** - автоматичне відновлення  
✅ **FPS** - ~30 FPS для moderate activity  
✅ **Латентність** - <100ms затримка  
✅ **CPU** - <30% на сервері

### Консоль браузера (F12)

Шукайте:
```
[Zero-copy stats] Skipped: X% → вище = краще
[Frame 100] Changed tiles: Y → менше = краще
[Adaptive FPS] Sent: X dynamic, Y static
WebP Encoding: Z ms → менше = краще
```

### Метрики у статус-барі

- **Тайлів/с**: кількість тайлів за секунду
- **Трафік**: кбіт/с
- **Латентність**: середній час декодування WebP (мс)
- **Розмір**: розмір екрану

### Відомі issues

⚠️ **"Failed to add ICE candidate"** - нормально, не впливає  
⚠️ **Перше підключення повільне** - ICE gathering  
✅ **Наступні підключення швидкі** - candidates cached

### WebRTC латентність

Якщо латентність висока, перевірте:
1. Використовується `index-optimized.html` (не `index.html`)
2. Проект перезібрано після змін
3. Метрика "Латентність" у статус-барі
4. Console logs у браузері (F12)

**Оптимізації для зниження латентності**:
- Rendezvous channel (buffer=0) - видалено 1 frame буфер
- Менші пакети (8KB замість 16KB)
- Без STUN для локальних з'єднань
- Unordered DataChannel - без head-of-line blocking
- Desynchronized canvas - рендеринг без vsync

---

## Підсумок

### Досягнення

✅ **10 багів виправлено** (2026-06-13)  
✅ **Metadata optimization** (2026-06-15) → +10-25% швидше  
✅ **FPS: 411-1337** (було 372-1067 до metadata opt)  
✅ **Tile merging: 97-99%** reduction  
✅ **Zero-copy: 88-99%** tiles skipped  
✅ **Hash: 744 ns** (найшвидший)

### Можливості для подальшого розвитку

1. **Підвищити target FPS** до 60-120 (є запас performance)
2. **Збільшити якість WebP** (менше artifacts)
3. **Більше tiles** (більша деталізація)
4. **Багато клієнтів** одночасно
5. **GPU encoding** (NVENC/QuickSync)
6. **Damage tracking** з Wayland compositor

### Структура проекту

```
.
├── src/
│   ├── main.rs              - WebRTC server entry point
│   ├── stream.rs            - Streaming logic з tile merging
│   ├── capture.rs           - Wayland screen capture
│   ├── diff.rs              - Change detection з zero-copy hashing
│   ├── encoder.rs           - WebP encoding & tile merging
│   ├── encoding_pool.rs     - Parallel encoding worker pool
│   ├── tile.rs              - SIMD tile hashing (AVX2/SSE2)
│   ├── config.rs            - Configuration з CPU detection
│   └── bin/
│       └── detailed_bench.rs - Realistic benchmark tool
├── test_benchmark.rs        - CPU speed detection
├── docs/
│   ├── DOCUMENTATION.md     - Ця документація
│   ├── API_REFERENCE.md     - Технічний довідник
│   ├── DEVELOPMENT.md       - Development guide
│   ├── client-examples/     - HTML WebRTC client examples
│   └── scripts/             - Benchmark scripts
├── Cargo.toml
└── README.md
```

---

**Версія документації**: 3.1  
**Дата**: 2026-06-15  
**Автор**: tarilka0gg — Metadata CircularBuffer optimization
