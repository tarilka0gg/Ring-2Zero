# Development Guide

Architecture, configuration/protocol reference, key algorithms, and troubleshooting for contributors. For install/usage/tutorial see the [README](../README.md); for the PR process see [CONTRIBUTING.md](../CONTRIBUTING.md).

## Architecture

```
capture thread                processing thread                 async send loop
───────────────                ─────────────────                 ───────────────
wlr::WlrCapture      Frame     DiffDetector::detect_changes       StreamServer
  or                 ─────►    (diff.rs)                          .handle_client_async
pipewire::PipeWireCapture      │                                  (stream.rs)
(capture/mod.rs               ▼
 auto-detects backend)   TileMerger::merge          tiles+encoded
                         (encoder.rs)          ─────────────────► WebRTC DataChannel
                              │                    (mpsc channel)      │
                              ▼                                       ▼
                       priority sort +                          client (ACKs)
                       EncodingPool
                       (encoding_pool.rs)
```

- **Capture** (`capture/wlr.rs`, `capture/pipewire.rs`) produces `Frame { rgba, width, height, damage_regions }` on an `mpsc` channel. `capture/mod.rs` auto-detects wlr-screencopy (DMA-BUF, preferred on wlroots compositors) vs. PipeWire-via-portal (feature-gated, `GNOME`/`KDE`/X11).
- **Diff** (`diff.rs`) hashes each grid tile, classifies it changed/unchanged/throttled, and decides which changed tiles to actually send this frame. See [Key algorithms](#key-algorithms).
- **Merge** (`encoder.rs`) groups adjacent changed tiles into up to 4×4-cell rectangles.
- **Priority + encode** (`stream.rs`, `encoding_pool.rs`) sorts merged tiles by priority, extracts pixels (`tile_extract.rs`), and encodes to WebP across a worker pool, with a per-tile cache for repeats.
- **Transport** (`stream.rs`, `webrtc_connection.rs`, `signaling.rs`, `server.rs`) frames tiles into DataChannel packets with ACK tracking, and handles the WebSocket signaling handshake (SDP offer/answer, ICE candidates, token auth) with auto-reconnect.
- **Client page** (`server.rs`) — `docs/client-examples/client.html` is embedded into the binary (`include_str!`) and served on any plain HTTP GET to the same port the WebSocket listens on, over the same TLS if configured. `handle_connection`/`handle_connection_tls` sniff the first bytes of a new connection to dispatch between a WebSocket upgrade and a static GET; the TLS path uses a small `PrefixedStream` wrapper to "un-consume" those sniffed bytes before handing the connection to the WebSocket upgrade, since (unlike a raw `TcpStream`) a generic TLS stream has no kernel-level `peek`.

## Configuration reference

The env vars and CLI flags in the [README](../README.md#configuration) cover everything a deployment needs to touch. The rest of `Config` (`src/config.rs`) is compile-time only — edit `Config::default()` and rebuild:

| Field | Default | Notes |
|---|---|---|
| `ws_port` | `9001` | Not env-configurable. |
| `target_fps` | `60` | Overall frame-loop rate; `RING2ZERO_MAX_FPS` overrides this + the two below uniformly. |
| `tiles_x` | `20` | Grid columns; rows derived from aspect ratio via `calculate_tile_dimensions`. |
| `webp_quality_low` / `webp_quality_high` | `1.0` / `10.0` | Used for dynamic vs. static tiles respectively. |
| `merge_gap` | auto-tuned at startup | See [Tile merging](#tile-merging); `0` with `--no-adaptive`. |
| `priority_frequency_weight` / `priority_speed_weight` / `priority_center_weight` | `0.5` / `0.3` / `0.2` | See [Priority scoring](#priority-scoring). |
| `priority_history_window` | `30` | Currently unused — `TileMetadata`'s `CircularBuffer` change history is hardcoded to a 32-frame window (`tile.rs`'s `CircularBuffer::default()`), independent of this field. |
| `static_tile_fps` / `dynamic_tile_fps` | `16` / `60` | Send-rate cap per tile mode — see [FPS throttling](#fps-throttling--adaptive-send-rate). |

## Wire protocol

Binary messages over the WebRTC DataChannel, all little-endian:

1. **Header** (resolution change) — 6 bytes: `0xFFFF (u16) | width (u16) | height (u16)`.
2. **Sequence control packet** (once per frame, before its tiles) — 10 bytes: `0xFFFE (u16) | seq (u32) | tile_count (u32)`. `tile_count` lets the client withhold its ACK until every tile in the batch is actually decoded, not just on marker receipt.
3. **Tile data** — `tile_len (u32) | x (u16) | y (u16) | width (u16) | height (u16) | webp_bytes...`, packed back-to-back up to an 8000-byte packet; a tile that doesn't fit gets its own packet (length-prefixed the same way).
4. **ACK** (client → server) — 4 bytes: `seq (u32)`. See [ACK-loss recovery](#ack-loss-recovery).

The current framing/decoding is implemented once, in `docs/client-examples/client.html` — that's the source of truth for the client side; this list is for writing a new client from scratch.

## Key algorithms

### Two-stage hashing
`hash_tile_half` samples every 2nd row (half the pixel data) and hashes it. If that matches the tile's `prev_half_hash` from last frame (and it's not the first frame), the tile is treated as unchanged without computing the full hash — typically an 85–99% skip rate on real desktop content. Otherwise `hash_tile` hashes the full tile. Both are AVX2 (32B/iteration) with an SSE2 fallback, using a golden-ratio-seeded XOR accumulator — fast, but **not** collision-resistant (this bit a cache-lookup shortcut in `frame_profiler.rs` once, fixed in v0.299.2 by keying lookups to a specific tile index instead of scanning for any hash match).

### Damage-region skip
When the compositor reports damage regions, tiles outside all of them skip hashing entirely — except on the very first frame (client has no prior content yet) or when a tile's `force_redetect` flag is set (see below); either bypasses the skip so a forced re-detection can't be silently swallowed.

### FPS throttling / adaptive send rate
A tile is "dynamic" if its hash changed on both of the last two frames (`prev_prev_hash != prev_hash != current_hash`). The send interval is `target_fps / dynamic_tile_fps` for dynamic tiles or `target_fps / static_tile_fps` for static ones; a changed tile is sent when that interval has elapsed, or immediately on its first transition into dynamic. A tile that changed but was throttled still advances its hash baseline and change-history (the `changed_unsent` path in `diff.rs`) so it isn't miscounted as unchanged.

### Tile merging
`TileMerger` (`encoder.rs`) groups changed tiles into vertical runs per column (tolerating gaps up to `merge_gap` tiles), then groups matching runs into horizontal rectangles. Capped at `MAX_MERGE_TILES_X`/`Y = 4` per rectangle — without the cap, a full-screen refresh would merge into one oversized message that can exceed the DataChannel's size limit. `merge_gap` itself is auto-tuned once at startup from a WebP encode-speed microbenchmark (cached at `~/.cache/screen-streamer/cpu_bench.json`, invalidated on CPU/binary change or after 7 days): >20ms/tile → `3` (aggressive), >10ms → `1`, else `0`.

### Priority scoring
Tiles queued for sending are sorted highest-priority-first: `frequency_score × priority_frequency_weight + change_speed × priority_speed_weight + center_score × priority_center_weight`, where frequency comes from the `CircularBuffer` change history, change_speed is the popcount of the tile's last hash XOR-diff, and center_score favors tiles closer to the screen's center.

### Per-tile encode cache
Each single-cell tile's last WebP encode is cached, keyed by its content hash — a tile re-selected for sending without its pixels changing (e.g. by the periodic quality refresh below) skips extraction and encoding entirely. Restricted to single-cell tiles: a merged multi-cell tile's cache key only covers its one representative cell's hash, so applying the same shortcut to the whole merged region could serve stale bytes for a *different* cell that did change (the bug fixed across v0.299.1/v0.299.2).

### Periodic quality refresh
Every second, `invalidate_cache` resets tiles that were last sent at low ("dynamic") quality, forcing a high-quality re-encode next time they're selected — catches a tile that stopped moving right after being sent at throttled quality.

### ACK-loss recovery
Each frame's DataChannel messages carry a sequence number; the client ACKs once every tile in that batch is decoded. If 150ms pass with no ACK, every grid cell the frame's tiles covered (`Tile::covered_indices` — all cells of a merged tile, not just its representative one) is queued and, on the processing thread's next iteration, passed to `invalidate_tiles`. That resets the tile's hash baseline, cached encode, and sets its `force_redetect` flag, forcing it to be re-sent on the next frame regardless of whether its pixels actually changed. A resolution change bumps an epoch counter; queued indices carry the epoch they were produced under and are dropped rather than applied if it's since changed, so a resize mid-flight can't misapply stale indices to the new tile grid.

## Benchmarking & profiling

All under `src/bin/`, gated behind Cargo features so they don't affect the default build:

```bash
cargo run --release --bin frame_profiler --features bench_tools     # full pipeline timing breakdown, 3 synthetic scenarios
cargo run --release --bin diff_profiler --features bench_tools      # diff-detection phase only
cargo run --release --bin hash_analyzer --features bench_tools      # compare hash function throughput
cargo run --release --bin modern_hash_bench --features bench_tools  # alternative hash algorithms
cargo run --release --bin advanced_bench --features bench_tools     # end-to-end pipeline metrics
cargo run --release --bin detailed_bench --features bench_tools     # multi-scenario summary
cargo run --release --bin webp_codec_bench --features webp_bench    # fast-webp vs. webp/webpx/webp-rust
```

`frame_profiler`'s breakdown attributes time per pipeline stage (WebP encoding usually dominates, 80–90%); `--debug` on the main binary prints the zero-copy hash skip percentage every 100 frames — 85%+ is typical on a mostly-static screen.

## Troubleshooting

### Low FPS / high frame time
Run `frame_profiler` and see which stage dominates:
- **Diff detection high** → check the zero-copy skip percentage (`--debug`); a low rate usually means the content is genuinely fully dynamic (e.g. full-screen video) rather than a bug.
- **Encoding high** → lower `webp_quality_high`/`webp_quality_low` or `tiles_x` in `Config::default()`.
- **Merge high** → increase `merge_gap`, or check `~/.cache/screen-streamer/cpu_bench.json` picked a sane value for your CPU.

### High CPU usage
- Check `tiles_x` isn't unreasonably high for the resolution — more tiles means more per-tile overhead even when nothing changed.
- `top -H -p $(pgrep ring-2zero)` for per-thread usage; the encoding pool defaults to `num_cpus::get().max(4)` workers.
- Don't leave `--debug` on for a production run — it adds per-frame logging overhead.

### WebRTC won't connect
- Confirm the port is actually listening: `ss -tlnp | grep 9001`.
- Browser console (F12): look for ICE candidate errors. `RUST_LOG=ice=debug,webrtc_ice=debug,mdns=debug,webrtc_mdns=debug` on the server gives matching server-side detail.
- Safari/iOS refusing outright: needs a secure context — see [Remote access](../README.md#remote-access) for `RING2ZERO_TLS_CERT`/`KEY`.
- Multi-homed server (LAN + VPN) advertising an unreachable candidate: set `RING2ZERO_ICE_INTERFACE`.
- If a non-localhost connection never completes on a version older than v0.291: that's a known, fixed bug (ICE candidates arriving before the SDP answer were dropped) — update.

### Build errors
- Missing Wayland headers: `libwayland-dev` (Debian/Ubuntu), `wayland` (Arch), `dev-libs/wayland` (Gentoo).
- Linker: `.cargo/config.toml` pins `linker = "clang"` — make sure a `clang` binary is on `PATH` (Gentoo: `emerge sys-devel/clang`; set `CC`/`CXX` explicitly if you have multiple slotted versions).
- `pipewire_capture` build fails: needs `libpipewire-0.3` and `libdbus-1` dev headers/pkg-config files.

## Build configuration

`Cargo.toml` has no `[profile.release]` override, so `cargo build --release` uses Cargo's own release defaults (`opt-level = 3`, `debug-assertions = false`, `overflow-checks = false`). `.cargo/config.toml` pins the linker to `clang`; set `CC`/`CXX` if your system's default isn't compatible (see [Building](../README.md#building)).
