# Ring-2Zero

[![CI](https://github.com/tarilka0gg/ring-2zero/actions/workflows/ci.yml/badge.svg)](https://github.com/tarilka0gg/ring-2zero/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

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
sudo rc-update add tailscale default && sudo rc-service tailscale start
sudo tailscale up                     # opens a browser link to log in
```

Safari (and iOS in general) requires a secure context for WebRTC — set
`RING2ZERO_TLS_CERT`/`RING2ZERO_TLS_KEY` (e.g. from `tailscale cert
<device>.<tailnet>.ts.net`) to serve `wss://` instead of `ws://`, and serve
`client.html` itself over HTTPS too (any static file server with the same
cert works).

If the server machine has more than one network interface (e.g. a LAN port
alongside the Tailscale one), ICE may otherwise advertise a candidate the
remote peer can't reach. Set `RING2ZERO_ICE_INTERFACE=tailscale0` to restrict
candidate gathering to just the VPN interface.

Find the server's Tailscale hostname (`tailscale status`, or rename the
device with `tailscale set --hostname=<name>` for a nicer URL), then open
`client.html?server=<name>.<tailnet>.ts.net:9001` from the viewing device —
or just `client.html` on its own if that's already the default in
`docs/client-examples/client.html`.

## Authentication

The signaling server requires a token, checked during the WebSocket
handshake (`?token=...` query param). By default a random token is
generated on each startup and printed to stdout; set `RING2ZERO_TOKEN` in
the environment to use a fixed one instead. A connection without a
matching token gets an HTTP 401 and is never upgraded.

The client page (`client.html`) doesn't take the token via URL — it prompts
for a password on first load and remembers it in the browser's
`localStorage`, so the token never sits in the address bar/history. This is
what `RING2ZERO_TOKEN` should be set to.

This guards the signaling handshake itself, but is still no substitute for
network-level isolation — prefer keeping the port reachable only over a
VPN (like Tailscale above) rather than a public port-forward.

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

### v0.299 (July 2026)
- **Fixed**: `RTCPeerConnection` was never explicitly closed on drop, leaking the ICE/DTLS/SCTP transport stack on every reconnect — now closed via `Drop`.
- **Fixed**: DataChannel accidentally lost its `ordered: false` setting, reintroducing head-of-line-blocking latency.
- **Fixed**: TLS cert/key load failure now returns a clean error and exits instead of panicking.
- **Changed**: `RING2ZERO_IPV4_ONLY` is now read into `Config` (was previously unconditional) — IPv6-only deployments no longer get zero ICE candidates.
- **Changed**: `RING2ZERO_TLS_CERT/KEY`, `RING2ZERO_ICE_INTERFACE`, `RING2ZERO_MAX_FPS` consolidated into `Config`, matching the existing `RING2ZERO_TOKEN` pattern.

### v0.291 (July 2026)
- **Fixed**: ICE candidates from the client were dropped whenever they arrived before the SDP answer (a common race, since the client fires `onicecandidate` before sending its answer) — they're now buffered and applied once the remote description is set. This was silently breaking nearly every non-localhost connection.
- **Fixed**: Safari/Chrome obfuscate host ICE candidates behind a `<uuid>.local` mDNS name; mDNS resolution is now explicitly enabled so these candidates actually resolve instead of being unusable.
- **Fixed**: a full-screen refresh could merge every dirty tile into one oversized message exceeding the DataChannel's message-size limit — merged tiles are now capped to a bounded grid size.
- **Added**: TLS support (`RING2ZERO_TLS_CERT`/`RING2ZERO_TLS_KEY`) — required for Safari/iOS, which refuses WebRTC on an insecure page.
- **Added**: `RING2ZERO_ICE_INTERFACE` to restrict ICE candidate gathering to one network interface (e.g. `tailscale0`) on multi-homed machines.
- **Added**: password-gated client — `client.html` prompts for the token instead of taking it via URL, and remembers it in `localStorage`.
- **Changed**: ICE disconnect/keepalive timeouts relaxed from same-machine-tuned defaults to values tolerant of real network latency/jitter.

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
