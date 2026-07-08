# Ring-2Zero

High-performance Wayland screen streaming server with WebRTC support.

## Features

- **DMA-BUF zero-copy capture** via wlr-screencopy v3 + GBM LINEAR (niri, sway, wlroots)
- **PipeWire screencast** via xdg-desktop-portal (GNOME, KDE, X11)
- **Auto-detection** of the best available capture backend
- **Tile-based diff encoding** with intelligent change detection
- **Fast WebP compression** with adaptive quality
- **WebRTC DataChannel** streaming to browser clients
- **ACK feedback system** — client confirms received frames, server invalidates lost tiles
- **Auto-reconnect** — WebSocket stays alive across WebRTC re-negotiations
- **SIMD optimizations** — AVX2/SSE2 for hashing, tile extraction, BGRX→RGBA conversion
- **Parallel encoding pool** with worker threads

## Performance

Benchmarks with fast-webp encoding (v0.277, July 2026, i7-14650HX):

| Scenario | Time/Frame | FPS | Pipeline Breakdown |
|----------|-----------|-----|--------------------|
| 🟢 Static content | 0.13 ms | **7968 FPS** | 100% diff detection |
| 🟡 Moderate activity | 0.47 ms | **2142 FPS** | 31% diff, 3% merge, 66% encode |
| 🟠 Active work | 0.63 ms | **1589 FPS** | 39% diff, 3% merge, 58% encode |
| 🔴 Video window | 0.52 ms | **1935 FPS** | 37% diff, 3% merge, 60% encode |

Key numbers:
- **Tile merging**: 83–99% tile reduction (e.g., 20 627 → 247 tiles)
- **Cache hits**: 41–67% tiles served without re-encoding
- **DMA-BUF vs SHM**: eliminates one kernel copy per frame on wlroots compositors

## Building

```bash
# Standard build (wlr-screencopy only)
cargo build --release

# With PipeWire support (GNOME, KDE, X11)
cargo build --release --features pipewire_capture

# Requires Clang on some systems
CC=/usr/lib/llvm/22/bin/clang cargo build --release
```

## Running

```bash
# Start the server (default: ws://localhost:9001)
./target/release/ring-2zero

# Open the browser client
xdg-open docs/client-examples/client.html
```

## Remote access

The server binds to `0.0.0.0:9001`, so it's reachable from any network
interface — including a VPN. The signaling exchange (WebSocket) works fine
over a VPN; the WebRTC media itself needs no separate STUN/TURN setup in
this case, since both peers appear to be on the same virtual LAN.

Recommended: [Tailscale](https://tailscale.com/) — install it on both the
server machine and the viewing device, then:

```bash
sudo emerge --ask net-vpn/tailscale   # Gentoo; use your distro's package manager otherwise
sudo rc-update add tailscaled default && sudo rc-service tailscaled start
sudo tailscale up                     # opens a browser link to log in
```

Find the server's Tailscale IP with `tailscale ip -4`, then open the client
with that address instead of localhost:

```
docs/client-examples/client.html?server=100.x.x.x:9001
```

There is currently no authentication on the signaling server — treat the
port as equivalent to full remote access to your screen. Restricting it to
a VPN (rather than a public port-forward) is the safe default; do not
expose port 9001 directly to the internet without adding your own
authentication in front of it.

## Dependencies

System libraries required:
- `libwayland-client` — Wayland protocol
- `libgbm` — GBM buffer allocation for DMA-BUF path
- `libdrm` — DRM render node access

Optional (for `--features pipewire_capture`):
- `libpipewire-0.3` — PipeWire stream
- `libdbus-1` — xdg-desktop-portal D-Bus handshake

## Project Structure

```
src/
├── main.rs              — entry point
├── server.rs            — WebSocket + WebRTC server
├── stream.rs            — streaming loop, ACK system
├── capture/
│   ├── mod.rs           — backend auto-detection
│   ├── wlr.rs           — wlr-screencopy (DMA-BUF + SHM fallback)
│   └── pipewire.rs      — PipeWire via portal (feature-gated)
├── diff.rs              — tile change detection
├── encoder.rs           — WebP encoding + tile merging
├── encoding_pool.rs     — parallel worker pool
├── tile.rs              — tile hashing (AVX2/SSE2)
├── tile_extract.rs      — tile extraction (AVX2/SSE2)
├── convert.rs           — BGRX→RGBA (AVX2/SSE2)
├── config.rs            — configuration + CPU benchmark cache
└── shm.rs               — shared memory buffer (memfd)
src_c/
└── pw_capture.c         — PipeWire + D-Bus portal C helper
docs/
└── client-examples/
    └── client.html      — browser WebRTC client
```

## Changelog

### v0.277 (July 2026)
- **Added**: DMA-BUF zero-copy capture via wlr-screencopy v3 + libgbm (LINEAR GBM buffer, mmap read)
- **Added**: PipeWire screencast backend via xdg-desktop-portal (GNOME, KDE, X11)
- **Added**: Auto-detection of capture backend at startup
- **Added**: ACK feedback system — 6-byte control packet, client confirms frame, server re-sends lost tiles
- **Added**: Auto-reconnect — stream survives WebRTC re-negotiations without restarting WebSocket

### v0.202 (June 19, 2026)
- Fixed duplicate function in webp_codec_bench
- FPS metrics now correctly reflect fast-webp performance

### v0.181 (June 18, 2026)
- Upgraded webp 0.3 → fast-webp 0.1.1 (2–3× faster encoding)
- Arc-based tile cache (zero-copy, ~120 MB/s bandwidth saved)
- SIMD tile extraction (AVX2/SSE2)
- 3× overall pipeline speedup

### v0.160 (June 17, 2026)
- Tile merging (83–99% reduction)
- Zero-copy hashing with SIMD
- CPU benchmark caching

## License

MIT — see [LICENSE](LICENSE)
