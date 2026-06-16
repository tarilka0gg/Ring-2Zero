# Screen Streamer - API Reference

Технічний довідник для розробників

Версія: 1.0 | Дата: 2026-06-15

---

## 📋 Зміст

1. [API Reference](#api-reference)
2. [Протокол передачі даних](#протокол-передачі-даних)
3. [Алгоритми](#алгоритми)
4. [Приклади коду](#приклади-коду)

---

## API Reference

### Config

```rust
pub struct Config {
    pub ws_port: u16,                              // WebSocket порт (default: 9001)
    pub target_fps: NonZeroU32,                    // Target FPS (default: 32)
    pub tiles_x: u32,                              // Кількість tiles по X (default: 40)
    pub webp_quality_low: f32,                     // WebP якість для dynamic (0.5)
    pub webp_quality_high: f32,                    // WebP якість для static (8.0)
    pub merge_gap: u32,                            // Gap між tiles для merge (0)
    pub dynamic_tile_fps: NonZeroU32,              // FPS для dynamic tiles (32)
    pub static_tile_fps: NonZeroU32,               // FPS для static tiles (8)
    pub priority_frequency_weight: f32,            // Вага frequency в priority (0.5)
    pub priority_speed_weight: f32,                // Вага speed (0.3)
    pub priority_center_weight: f32,               // Вага center distance (0.2)
    pub priority_history_window: usize,            // Розмір history (32 frames, max 64)
    pub debug_mode: bool,                          // Debug logging (false)
}

impl Config {
    pub fn frame_duration(&self) -> Duration;      // 1000ms / target_fps
}
```

### Frame

```rust
pub struct Frame {
    pub rgba: Vec<u8>,                             // RGBA pixel data (width×height×4)
    pub width: u32,                                // Ширина в пікселях
    pub height: u32,                               // Висота в пікселях
    pub damage_regions: Vec<DamageRegion>,        // Wayland damage hints
}

pub struct DamageRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
```

### Tile

```rust
#[derive(Copy, Clone)]
pub struct Tile {
    pub x: u32,          // Позиція X (пікселі)
    pub y: u32,          // Позиція Y (пікселі)
    pub width: u32,      // Ширина (пікселі)
    pub height: u32,     // Висота (пікселі)
    pub quality: f32,    // WebP quality (0.5-8.0)
}

impl Tile {
    pub fn new(x: u32, y: u32, width: u32, height: u32, quality: f32) -> Self;
    pub fn distance_from_center(&self, cx: u32, cy: u32) -> u32;
}
```

### TileMetadata

```rust
#[derive(Clone)]
pub struct TileMetadata {
    pub unchanged_frames: u32,                     // Кількість frames без змін
    pub last_sent_frame: u64,                      // Номер останнього відправленого frame
    pub is_dynamic: bool,                          // Dynamic tile (рух)
    pub last_sent_as_dynamic: bool,                // Відправлено як dynamic
    pub change_history: CircularBuffer,            // 64-bit history (1=change, 0=no change)
    pub update_frequency: f32,                     // Частота змін (0.0-1.0)
    pub last_hash_diff: u64,                       // XOR prev vs current hash
    pub prev_half_hash: u64,                       // Half hash (50% даних)
    pub cached_encoded: Option<Vec<u8>>,           // Кешований WebP
    pub cached_hash: u64,                          // Hash для cache validation
}
```

### CircularBuffer

```rust
#[derive(Clone, Debug)]
pub struct CircularBuffer {
    data: u64,           // 64-bit bitfield (1=changed, 0=unchanged)
    size: u8,            // Поточний розмір (0-64)
    capacity: u8,        // Максимум (≤64)
}

impl CircularBuffer {
    pub fn new(capacity: usize) -> Self;           // capacity обмежена 64
    pub fn push(&mut self, value: bool);           // Додати value, shift old
    pub fn count_ones(&self) -> u32;               // Кількість true (hardware count_ones)
    pub fn len(&self) -> usize;                    // Поточний розмір
}
```

**Переваги**:
- 64 frames історії в 8 байтах
- Bitwise операції замість iterator
- Hardware instruction для count_ones()
- Було: VecDeque (32 bytes + allocation)

### DiffDetector

```rust
pub struct DiffDetector {
    prev_hashes: Vec<u64>,                         // Хеші попереднього frame
    prev_prev_hashes: Vec<u64>,                    // Хеші pre-попереднього (для motion)
    tile_metadata: Vec<TileMetadata>,              // Metadata для кожного tile
    config: Config,
    frame_count: u64,                              // Лічильник frames
    skipped_hashes: u64,                           // Статистика zero-copy
    total_hashes: u64,
}

impl DiffDetector {
    pub fn new(config: Config) -> Self;
    
    // Головний метод: детекція змін
    pub fn detect_changes(&mut self, frame: &Frame) -> (Vec<Tile>, Vec<usize>);
    
    // Отримати metadata для tile
    pub fn get_metadata(&self, index: usize) -> &TileMetadata;
    pub fn get_all_metadata_mut(&mut self) -> &mut [TileMetadata];
    
    // Отримати поточні хеші
    pub fn get_current_hashes(&self) -> &[u64];
    
    // Reset (при зміні resolution)
    pub fn reset(&mut self);
}
```

### TileMerger

```rust
pub struct TileMerger {
    merge_gap: u32,      // Максимальний gap між tiles для merge
}

impl TileMerger {
    pub fn new(merge_gap: u32) -> Self;
    
    // Злиття tiles в прямокутники
    pub fn merge(
        &self,
        tiles: &[Tile],
        tiles_x: u32,
        tiles_y: u32,
        tile_width: u32,
        tile_height: u32,
        frame_width: u32,
        frame_height: u32,
    ) -> Vec<Tile>;
}
```

### EncodingPool

```rust
pub struct EncodingPool {
    task_tx: Sender<EncodingTask>,
    result_rx: Receiver<EncodingResult>,
    workers: Vec<JoinHandle<()>>,          // Worker threads
}

pub struct EncodingTask {
    pub tile: Tile,
    pub tile_data: Vec<u8>,                // RGBA data
    pub tile_idx: usize,
}

pub struct EncodingResult {
    pub tile_idx: usize,
    pub data: Vec<u8>,                     // WebP encoded data
}

impl EncodingPool {
    pub fn new(num_workers: usize) -> Self;
    pub fn submit(&self, task: EncodingTask) -> Result<()>;
    pub fn collect_results(&self, count: usize) -> Vec<EncodingResult>;
}

// Drop автоматично закриває канали і зупиняє workers
impl Drop for EncodingPool { }
```

### SIMD Functions

```rust
// AVX2 tile hashing (32 bytes per iteration)
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(data: &[u8]) -> u64;

// SSE2 fallback (16 bytes per iteration)
#[target_feature(enable = "sse2")]
unsafe fn hash_sse2(data: &[u8]) -> u64;

// Public API з runtime detection
pub fn hash_tile(rgba: &[u8], x: u32, y: u32, w: u32, h: u32, width: u32) -> u64;

// Batch hash comparison (4 hashes at once)
pub fn find_changed_tiles(prev: &[u64], new: &[u64]) -> Vec<usize>;

// Batch counter increment (8 counters at once)
pub fn increment_unchanged_counters(counters: &mut [u32]);

// BGRX→RGBA conversion (8 pixels at once)
#[target_feature(enable = "avx2")]
unsafe fn convert_bgrx_to_rgba_avx2(src: &[u8], dst: &mut [u8]);
```

---

## Протокол передачі даних

### WebRTC DataChannel

**Message types:**

#### 1. Header (Resolution change)

```
Bytes: [0xFF, 0xFF, W_lo, W_hi, H_lo, H_hi]
Total: 6 bytes

0xFFFF       - Magic marker (u16 big-endian)
W_lo, W_hi   - Width (u16 little-endian)
H_lo, H_hi   - Height (u16 little-endian)
```

#### 2. Tile data

```
Bytes: [Len0, Len1, Len2, Len3, X0, X1, Y0, Y1, W0, W1, H0, H1, ...WebP data...]

Len (u32 LE) - Довжина tile data (header + WebP)
X (u16 LE)   - X позиція
Y (u16 LE)   - Y позиція
W (u16 LE)   - Width
H (u16 LE)   - Height
WebP data    - Закодовані дані
```

#### 3. Frame packet (multiple tiles)

```
[Tile1_len, Tile1_data, Tile2_len, Tile2_data, ...]

MAX_PACKET_SIZE = 8000 bytes (8 KB)
```

### Приклад decode (JavaScript)

```javascript
function processBinaryMessage(data) {
    const view = new DataView(data.buffer);
    let offset = 0;
    
    while (offset < data.byteLength) {
        // Перевірка на header
        if (offset + 6 <= data.byteLength) {
            const marker = view.getUint16(offset, false); // big-endian
            if (marker === 0xFFFF) {
                const width = view.getUint16(offset + 2, true);
                const height = view.getUint16(offset + 4, true);
                console.log(`Resolution: ${width}x${height}`);
                canvas.width = width;
                canvas.height = height;
                offset += 6;
                continue;
            }
        }
        
        // Tile data
        const tileLen = view.getUint32(offset, true);
        offset += 4;
        
        const x = view.getUint16(offset, true);
        const y = view.getUint16(offset + 2, true);
        const w = view.getUint16(offset + 4, true);
        const h = view.getUint16(offset + 6, true);
        offset += 8;
        
        const webpData = data.slice(offset, offset + tileLen - 8);
        offset += tileLen - 8;
        
        // Decode WebP and draw
        decodeTile(x, y, w, h, webpData);
    }
}
```

### WebRTC Configuration

```rust
// Server-side
let config = RTCConfiguration {
    ice_servers: vec![],  // Порожньо для локального з'єднання
    ..Default::default()
};

// DataChannel init
let dc_init = RTCDataChannelInit {
    ordered: Some(false),      // Неупорядковані повідомлення
    max_retransmits: Some(0),  // Без ретрансмісій
    ..Default::default()
};

// ICE timeouts (агресивні для низької латентності)
s.set_ice_timeouts(
    Some(Duration::from_secs(5)),      // disconnected
    Some(Duration::from_secs(10)),     // failed
    Some(Duration::from_millis(500)),  // keepalive
);
```

```javascript
// Client-side
const config = {
    iceServers: []  // Порожньо для локального з'єднання
};

const pc = new RTCPeerConnection(config);

// Canvas з низькою латентністю
const ctx = canvas.getContext('2d', {
    alpha: false,
    desynchronized: true  // Рендеринг без vsync
});
```

---

## Алгоритми

### 1. Half Hash Zero-Copy

**Проблема**: Хешування 1600 tiles × 48×27 px = дорого

**Рішення**: Two-stage hashing

```rust
// Stage 1: Half hash (кожен 2-й рядок, 50% даних)
let half_hash = hash_tile_half(frame_data, x, y, w, h, width);

if half_hash == metadata.prev_half_hash {
    // ZERO-COPY! Skip full hash
    new_hash = prev_hash;
    skipped += 1;
} else {
    // Stage 2: Full hash (всі дані)
    new_hash = hash_tile(frame_data, x, y, w, h, width);
}
```

**Результат**: 88-99% tiles skipped (44-50% CPU savings)

### 2. AVX2 Hashing

```rust
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(data: &[u8]) -> u64 {
    let mut acc = _mm256_setzero_si256();          // 256-bit accumulator
    let mut seed = _mm256_set1_epi64x(SEED);       // Golden ratio seed
    
    // Process 32 bytes per iteration
    for chunk in data.chunks_exact(32) {
        let v = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);
        acc = _mm256_xor_si256(acc, v);            // XOR data
        seed = _mm256_add_epi64(seed, SEED_INC);   // Rotate seed
        acc = _mm256_xor_si256(acc, seed);         // Mix
    }
    
    // Horizontal reduction: 256 → 64 bits
    let low = _mm256_extracti128_si256(acc, 0);   // Lower 128 bits
    let high = _mm256_extracti128_si256(acc, 1);  // Upper 128 bits
    let xor128 = _mm_xor_si128(low, high);
    
    let low64 = _mm_extract_epi64(xor128, 0) as u64;
    let high64 = _mm_extract_epi64(xor128, 1) as u64;
    
    low64 ^ high64 ^ (data.len() as u64)
}
```

**Performance**: 744 ns per hash (48×27 px = 5,184 bytes)

**Fallback chain**:
1. AVX2 (2013+ CPUs) → 32 bytes/iter
2. SSE2 (2006+ CPUs) → 16 bytes/iter
3. Scalar → 8 bytes/iter

### 3. Tile Merging Algorithm

**Мета**: Об'єднати сусідні tiles в прямокутники

```
Input: [(tx, ty), ...] - список змінених tiles

Algorithm:
1. Для кожної колонки tx:
   - Знайти vertical runs (безперервні послідовності)
   - Gap ≤ merge_gap дозволяє перервати run

2. Групувати runs з однаковим (ty_start, ty_end):
   - Key: (ty_start, ty_end)
   - Value: [tx1, tx2, tx3, ...]

3. Для кожної групи:
   - Сортувати tx
   - Знайти horizontal runs (tx[i+1] == tx[i] + 1)
   - Створити merged tile для кожного run

Output: [Tile {...}, ...] - merged rectangles
```

**Приклад**:
```
Input: Video window 640×480 = 168 tiles (scattered)
Output: 1 merged tile (640×480)
Reduction: 99.4%
```

**Псевдокод**:
```rust
fn merge_tiles(changed: Vec<(u32, u32)>, merge_gap: u32) -> Vec<Tile> {
    // 1. Групувати по колонках
    let mut columns: HashMap<u32, Vec<u32>> = HashMap::new();
    for (tx, ty) in changed {
        columns.entry(tx).or_default().push(ty);
    }
    
    // 2. Знайти vertical runs в кожній колонці
    let mut runs: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (tx, mut tys) in columns {
        tys.sort();
        let vertical_runs = find_runs(&tys, merge_gap);
        for (ty_start, ty_end) in vertical_runs {
            runs.entry((ty_start, ty_end)).or_default().push(tx);
        }
    }
    
    // 3. Знайти horizontal runs для кожного vertical run
    let mut merged = Vec::new();
    for ((ty_start, ty_end), mut txs) in runs {
        txs.sort();
        let horizontal_runs = find_runs(&txs, merge_gap);
        for (tx_start, tx_end) in horizontal_runs {
            merged.push(Tile {
                x: tx_start * tile_width,
                y: ty_start * tile_height,
                width: (tx_end - tx_start + 1) * tile_width,
                height: (ty_end - ty_start + 1) * tile_height,
                quality,
            });
        }
    }
    
    merged
}
```

### 4. Priority Calculation

```rust
fn calculate_priority(
    tile: &Tile,
    metadata: &TileMetadata,
    width: u32,
    height: u32,
    config: &Config,
) -> f32 {
    // Frequency score (0.0-1.0)
    let frequency = metadata.update_frequency;
    
    // Change speed (0.0-1.0) - скільки bits змінилось
    let speed = (metadata.last_hash_diff.count_ones() as f32) / 64.0;
    
    // Distance from center (0.0-1.0, closer = higher)
    let center_x = width / 2;
    let center_y = height / 2;
    let distance = tile.distance_from_center(center_x, center_y) as f32;
    let max_distance = ((width * width + height * height) / 4) as f32;
    let center_score = 1.0 - (distance / max_distance).sqrt();
    
    // Weighted sum
    frequency * config.priority_frequency_weight
        + speed * config.priority_speed_weight
        + center_score * config.priority_center_weight
}
```

**Використання**:
```rust
// Сортування tiles за priority (вищий = першим)
tiles_with_priority.sort_by(|a, b| {
    b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
});
```

### 5. Adaptive FPS

**Motion detection** (3-frame comparison):
```rust
let is_dynamic = 
    prev_prev_hash != prev_hash &&   // Зміна в frame N-1
    prev_hash != current_hash;        // Зміна в frame N
```

**FPS selection**:
```rust
let interval = if is_dynamic {
    target_fps / dynamic_tile_fps    // 32 / 32 = 1 (кожен frame)
} else {
    target_fps / static_tile_fps     // 32 / 8 = 4 (кожен 4-й frame)
};
```

**Send decision**:
```rust
let should_send = 
    is_first_frame ||
    (!was_sent_as_dynamic && is_dynamic) ||  // State change
    frames_since_last >= interval;            // Interval expired
```

**Update frequency calculation**:
```rust
let update_frequency = metadata.change_history.count_ones() as f32 
                     / metadata.change_history.len() as f32;
// Скільки разів tile змінювався за останні N frames
```

### 6. CircularBuffer Implementation

```rust
impl CircularBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: 0,
            size: 0,
            capacity: capacity.min(64) as u8,
        }
    }
    
    pub fn push(&mut self, value: bool) {
        // Shift left, insert new bit at position 0
        self.data = (self.data << 1) | (value as u64);
        
        if self.size < self.capacity {
            self.size += 1;
        }
    }
    
    pub fn count_ones(&self) -> u32 {
        if self.size < self.capacity {
            // Only count valid bits
            let mask = (1u64 << self.size) - 1;
            (self.data & mask).count_ones()
        } else {
            self.data.count_ones()
        }
    }
    
    pub fn len(&self) -> usize {
        self.size as usize
    }
}
```

**Переваги**:
- `push()`: одна bitwise операція замість `VecDeque::push_back` + `pop_front`
- `count_ones()`: hardware instruction замість `iter().filter().count()`
- Пам'ять: 10 bytes замість 32+ bytes

---

## Приклади коду

### Створення сервера

```rust
use screen_streamer::*;

#[tokio::main]
async fn main() -> Result<()> {
    // Config
    let config = Config {
        ws_port: 9001,
        target_fps: NonZeroU32::new(32).unwrap(),
        tiles_x: 40,
        webp_quality_low: 0.5,
        webp_quality_high: 8.0,
        ..Default::default()
    };
    
    // Initialize components
    let capture = WaylandCapture::new()?;
    let diff_detector = DiffDetector::new(config.clone());
    let tile_encoder = TileEncoder::new();
    let tile_merger = TileMerger::new(config.merge_gap);
    let encoding_pool = EncodingPool::new(num_cpus::get());
    
    // Run server
    run_server(config, capture, diff_detector, tile_encoder, 
               tile_merger, encoding_pool).await?;
    
    Ok(())
}
```

### Custom benchmark

```rust
use screen_streamer::*;
use std::time::Instant;

fn benchmark_diff_detection() {
    let config = Config::default();
    let mut detector = DiffDetector::new(config);
    
    // Create test frame
    let frame = Frame {
        rgba: vec![0u8; 1920 * 1080 * 4],
        width: 1920,
        height: 1080,
        damage_regions: vec![],
    };
    
    // Benchmark
    let start = Instant::now();
    for _ in 0..1000 {
        let (tiles, _indices) = detector.detect_changes(&frame);
    }
    let elapsed = start.elapsed();
    
    println!("Avg diff time: {:.2}ms", elapsed.as_secs_f64() * 1000.0 / 1000.0);
}
```

### Custom tile processing

```rust
fn process_tiles_custom(
    tiles: Vec<Tile>,
    frame: &Frame,
    metadata: &[TileMetadata],
) -> Vec<Vec<u8>> {
    tiles.par_iter()
        .enumerate()
        .map(|(i, tile)| {
            // Extract tile data
            let tile_data = extract_tile_rgba(frame, tile);
            
            // Custom processing
            if metadata[i].is_dynamic {
                encode_low_quality(&tile_data, tile)
            } else {
                encode_high_quality(&tile_data, tile)
            }
        })
        .collect()
}
```

### Client-side decode

```javascript
async function decodeTile(x, y, w, h, webpData) {
    // Create blob from WebP data
    const blob = new Blob([webpData], { type: 'image/webp' });
    const url = URL.createObjectURL(blob);
    
    // Load image
    const img = new Image();
    img.src = url;
    await img.decode();
    
    // Draw to canvas
    ctx.drawImage(img, x, y, w, h);
    
    // Cleanup
    URL.revokeObjectURL(url);
}
```

---

**Версія**: 1.0  
**Дата**: 2026-06-15  
**Автор**: tarilka0gg
