# Screen Streamer - Documentation Index

Повна документація проекту Screen Streamer

Версія: 3.1 | Дата: 2026-06-15

---

## 📚 Головні документи

### 🎯 [DOCUMENTATION.md](DOCUMENTATION.md)
**Повна документація для користувачів**

Все в одному файлі: огляд, швидкий старт, архітектура, оптимізації, performance, встановлення, тестування.

**Для кого**: Користувачі, які хочуть встановити та запустити проект.

**Містить**:
- Огляд проекту та можливості
- Швидкий старт (5 хвилин до першого запуску)
- Архітектура та pipeline обробки
- Всі оптимізації та їх результати
- Performance benchmarks
- Інструкції з встановлення
- Посібник з тестування

---

### 🔧 [API_REFERENCE.md](API_REFERENCE.md)
**Технічний довідник для розробників**

API reference, структури даних, протоколи, алгоритми, приклади коду.

**Для кого**: Розробники, які хочуть інтегрувати або модифікувати проект.

**Містить**:
- Повний API reference (Config, Frame, Tile, DiffDetector, etc.)
- Протокол передачі даних (WebRTC DataChannel)
- Детальні алгоритми (hashing, tile merging, priority calculation)
- SIMD функції (AVX2/SSE2)
- Приклади коду (server, benchmarks, client decode)

---

### 🛠️ [DEVELOPMENT.md](DEVELOPMENT.md)
**Посібник з розробки та troubleshooting**

Troubleshooting, development workflows, історія оптимізацій, build configuration.

**Для кого**: Контриб'ютори та maintainers.

**Містить**:
- Troubleshooting (performance, memory, CPU, latency, WebRTC)
- Development guide (додавання оптимізацій, benchmarks, code review)
- Повна історія оптимізацій (2026-06-10 до 2026-06-15)
- Build configuration (compiler flags, Cargo settings)
- SIMD code guidelines

---

## 🚀 Швидка навігація

### Я хочу...

**...встановити та запустити проект**
→ [DOCUMENTATION.md](DOCUMENTATION.md#швидкий-старт)

**...зрозуміти як працює проект**
→ [DOCUMENTATION.md](DOCUMENTATION.md#архітектура)

**...інтегрувати з моїм проектом**
→ [API_REFERENCE.md](API_REFERENCE.md#api-reference)

**...оптимізувати performance**
→ [DEVELOPMENT.md](DEVELOPMENT.md#troubleshooting)

**...додати нову фічу**
→ [DEVELOPMENT.md](DEVELOPMENT.md#development-guide)

**...дізнатись про оптимізації**
→ [DEVELOPMENT.md](DEVELOPMENT.md#історія-оптимізацій)

---

## 📊 Performance (2026-06-16)

**Після раунду оптимізацій (+15-100% покращення):**

### Frame Profiler (реальне encoding):

| Сценарій | До оптимізацій | Після оптимізацій | Покращення |
|----------|---------------|-------------------|------------|
| 🟢 Light (5% змін) | 730 FPS (1.37 ms) | **1493 FPS (0.67 ms)** | **+104%** 🚀 |
| 🟡 Medium (20% змін) | 740 FPS (1.35 ms) | **870 FPS (1.15 ms)** | **+18%** |
| 🟠 Heavy (50% змін) | 740 FPS (1.35 ms) | **847 FPS (1.18 ms)** | **+15%** |

### Detailed Bench (100 frames):

| Сценарій | FPS | Час на кадр | vs Target (32 FPS) |
|----------|-----|-------------|-------------------|
| 🟢 Static | **1087 FPS** | 0.92 ms | 34× швидше |
| 🟡 Moderate | **932 FPS** | 1.07 ms | 29× швидше |
| 🟠 Active | **383 FPS** | 2.61 ms | 12× швидше |
| 🔴 Video | **666 FPS** | 1.50 ms | 21× швидше |

### Ключові досягнення

- ✅ **Cache hits fix** (2026-06-16) - 56-95% cache hit rate (+15-100% performance)
- ✅ **TileBufferPool** (2026-06-16) - reusable buffers (менше allocations)
- ✅ **10 багів виправлено** (2026-06-13) - memory leak, race conditions, etc.
- ✅ **Metadata optimization** (2026-06-15) - VecDeque → CircularBuffer (+10-25%)
- ✅ **Tile merging**: 85-98% reduction (724 tiles → 13 tiles)
- ✅ **Zero-copy hashing**: 54-99% tiles skipped (27-50% CPU savings)
- ✅ **SIMD optimization**: AVX2/SSE2 (744 ns per hash)
- ✅ **Parallel encoding**: Worker pool з автоматичним cleanup

**Протестовано 7 оптимізацій, впроваджено 2 найефективніші.**

---

## 🗂️ Додаткові ресурси

### Client Examples

HTML WebRTC client приклади в `client-examples/`:
- `index.html` - оригінальний клієнт
- `index-fixed.html` - з фіксами WebRTC
- `index-optimized.html` - оптимізований (рекомендується)

### Scripts

Benchmark та utility скрипти в `scripts/`:
- `benchmark.sh` - основний benchmark
- `test-latency.sh` - тест латентності
- `benchmark.log` - результати

---

## 📖 Що читати далі?

### Новачкам
1. [DOCUMENTATION.md](DOCUMENTATION.md) - почніть звідси
2. Запустіть `./target/release/screen-streamer`
3. Відкрийте `http://localhost:9001/index-optimized.html`

### Розробникам
1. [API_REFERENCE.md](API_REFERENCE.md) - API та структури даних
2. [DEVELOPMENT.md](DEVELOPMENT.md#development-guide) - workflow
3. Створіть свій benchmark у `src/bin/`

### Тим, хто стикнувся з проблемами
1. [DEVELOPMENT.md](DEVELOPMENT.md#troubleshooting) - діагностика
2. Запустіть `./target/release/frame_profiler`
3. Перевірте логи у `/tmp/screen-streamer.log`

---

## 📝 Історія змін документації

### 2026-06-15: Консолідація (v3.1)
- ✅ Об'єднано 14 MD файлів у 3 логічні документи
- ✅ Видалено дублювання контенту
- ✅ Покращена структура та навігація
- ✅ Додано швидкі посилання

**Було**:
```
docs/
├── DOCUMENTATION.md
├── INDEX.md
├── README.md
├── TECHNICAL_REFERENCE.md
├── architecture/ (5 файлів)
├── optimizations/ (4 файли)
└── guides/ (1 файл)
```

**Стало**:
```
docs/
├── INDEX.md                ← Ця сторінка
├── DOCUMENTATION.md        ← Повна документація
├── API_REFERENCE.md        ← Технічний довідник
├── DEVELOPMENT.md          ← Development guide
├── client-examples/        ← HTML клієнти
└── scripts/                ← Benchmark скрипти
```

### Попередні версії
- **v3.0** (2026-06-15): Metadata optimization documentation
- **v2.0** (2026-06-13): Bug fixes documentation
- **v1.0** (2026-06-12): Initial complete documentation

---

## 🔗 Зовнішні посилання

### Wayland Protocols
- [wlr-screencopy](https://wayland.app/protocols/wlr-screencopy-unstable-v1)
- [Wayland documentation](https://wayland.freedesktop.org/docs/html/)

### WebRTC
- [WebRTC API](https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API)
- [WebRTC for the Curious](https://webrtcforthecurious.com/)

### SIMD
- [Intel Intrinsics Guide](https://www.intel.com/content/www/us/en/docs/intrinsics-guide/)
- [Rust SIMD](https://doc.rust-lang.org/std/simd/)

---

**Остання версія**: 3.1  
**Дата консолідації**: 2026-06-15  
**Автор**: tarilka0gg
