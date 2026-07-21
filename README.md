# Ring-2Zero

[![CI](https://github.com/tarilka0gg/Ring-2Zero/actions/workflows/ci.yml/badge.svg)](https://github.com/tarilka0gg/Ring-2Zero/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

High-performance Wayland screen streaming server with WebRTC support.

## Features

- **DMA-BUF zero-copy capture** via wlr-screencopy v3 + GBM LINEAR (niri, sway, wlroots)
- **PipeWire screencast** via xdg-desktop-portal (GNOME, KDE, X11)
- **Auto-detection** of the best available capture backend
- **Tile-based diff encoding** with intelligent change detection
- **Fast WebP compression** with adaptive quality
- **WebRTC DataChannel** streaming to browser clients
- **ACK feedback system** — client confirms received frames, server invalidates and re-sends lost tiles
- **Auto-reconnect** — WebSocket stays alive across WebRTC re-negotiations
- **SIMD optimizations** — AVX2/SSE2 for hashing, tile extraction, BGRX→RGBA conversion
- **Parallel encoding pool** with worker threads

## Quick start

The browser client is baked into the binary — there's no separate file to open or static server to run. Once the server is up, one URL is the whole client.

1. **Install system dependencies** — see [Dependencies](#dependencies) below for the full list and per-distro package names.

2. **Build**:
   ```bash
   git clone https://github.com/tarilka0gg/Ring-2Zero.git
   cd Ring-2Zero
   cargo build --release
   ```
   Not on a wlroots compositor (niri, sway)? Add `--features pipewire_capture` — see [Building](#building). Want a plain `ring-2zero` command instead of typing the `target/release/` path every time? `cargo install --path .` puts it on `PATH` (usually `~/.cargo/bin`).

3. **Run the server**:
   ```bash
   ./target/release/ring-2zero      # or just `ring-2zero` after `cargo install`
   ```
   The first run benchmarks your CPU's WebP encoding speed to pick a sensible tile-merging setting, and caches the result (`~/.cache/screen-streamer/cpu_bench.json`) so it only costs a couple seconds once. Skip it with `--no-adaptive`. `--help` prints the full flag/env var reference.

   Startup prints what you need to connect:
   ```
   WebRTC signaling server (WebSocket): ws://0.0.0.0:9001
   TLS disabled — set RING2ZERO_TLS_CERT/RING2ZERO_TLS_KEY for wss:// (required for Safari/iOS remote access)
   Auth token: 3f9a1c...
   Open http://<this-host>:9001 in a browser (password prompt uses the token above) — no separate client file needed, this binary serves the page itself
   ```

4. **Open that URL** in a browser — e.g. `http://localhost:9001`. On first load it prompts for the auth token printed above; paste it once, it's remembered in the browser's `localStorage` from then on. The page auto-detects the server address it was loaded from, so the same URL keeps working unchanged when you switch to [Remote access](#remote-access) (Tailscale, etc.) below — no `?server=` param needed unless you're hosting the page somewhere other than this binary.

5. You should now see your screen streaming in the browser tab. To view it from *another* device (phone, laptop, over the internet), see [Remote access](#remote-access).

## Building

```bash
# Standard build (wlr-screencopy only)
cargo build --release

# With PipeWire support (GNOME, KDE, X11)
cargo build --release --features pipewire_capture

# Requires Clang on some systems
CC=/usr/lib/llvm/22/bin/clang cargo build --release

# Optional: put `ring-2zero` on PATH instead of typing target/release/ring-2zero
cargo install --path .
```

## Configuration

Everything is configured via environment variables plus two CLI flags — no config file, so it behaves the same run directly, under systemd, or in a container.

| Variable | Default | Purpose |
|---|---|---|
| `RING2ZERO_TOKEN` | random, printed on startup | Fixed auth token instead of a fresh random one each run — set this if you want to script reconnects without re-reading stdout. |
| `RING2ZERO_TLS_CERT` / `RING2ZERO_TLS_KEY` | unset (plaintext `ws://`) | PEM cert/key paths to serve `wss://` instead. Required for Safari/iOS — see [Remote access](#remote-access). |
| `RING2ZERO_ICE_INTERFACE` | unset (all interfaces) | Restrict ICE candidate gathering to one named interface (e.g. `tailscale0`) on multi-homed machines. |
| `RING2ZERO_IPV4_ONLY` | unset (dual-stack) | Set to any value to exclude IPv6 ICE candidates — works around a dual-stack candidate-selection issue on some hosts. Don't set this on an IPv6-only path, it'll leave you with zero candidates. |
| `RING2ZERO_MAX_FPS` | unset | Caps `target_fps`/`static_tile_fps`/`dynamic_tile_fps` uniformly to N (clamped to 1–1000) — a quick bandwidth-constrained testing knob. |

CLI flags:

| Flag | Effect |
|---|---|
| `--no-adaptive` | Skip the startup CPU benchmark, use the default `merge_gap=0`. |
| `--debug` | Verbose per-tile/per-frame stats every 100 frames. |

`RUST_LOG=ice=debug,webrtc_ice=debug,mdns=debug,webrtc_mdns=debug` gives verbose ICE/mDNS connectivity diagnostics when troubleshooting a connection.

Everything else (tile grid size, WebP quality range, priority weights, per-mode FPS) is a compile-time default in `Config` — see `src/config.rs` and [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#configuration-reference) for the full field reference.

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
<device>.<tailnet>.ts.net`) to serve `wss://` instead of `ws://`. The client
page is served by this same binary over the same port, so it automatically
gets HTTPS too — no separate static file server or cert-juggling needed.

If the server machine has more than one network interface (e.g. a LAN port
alongside the Tailscale one), ICE may otherwise advertise a candidate the
remote peer can't reach. Set `RING2ZERO_ICE_INTERFACE=tailscale0` to restrict
candidate gathering to just the VPN interface.

Find the server's Tailscale hostname (`tailscale status`, or rename the
device with `tailscale set --hostname=<name>` for a nicer URL), then open
`https://<name>.<tailnet>.ts.net:9001` from the viewing device — the page
auto-detects that address and connects back to it, no `?server=` param
needed.

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

Run `cargo run --release --bin frame_profiler --features bench_tools` for a live breakdown on your own hardware — see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#benchmarking--profiling) for the rest of the profiling tools.

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
├── main.rs               — entry point
├── server.rs             — WebSocket + WebRTC server, serves the client page over HTTP(S)
├── stream.rs             — streaming loop, ACK system
├── capture/
│   ├── mod.rs            — backend auto-detection
│   ├── wlr.rs            — wlr-screencopy (DMA-BUF + SHM fallback)
│   └── pipewire.rs       — PipeWire via portal (feature-gated)
├── diff.rs               — tile change detection
├── encoder.rs            — WebP encoding + tile merging
├── encoding_pool.rs      — parallel worker pool
├── tile.rs               — Tile/TileMetadata, hashing (AVX2/SSE2)
├── tile_extract.rs       — tile extraction (AVX2/SSE2)
├── convert.rs            — BGRX→RGBA (AVX2/SSE2)
├── config.rs             — configuration + CPU benchmark cache
├── webrtc_connection.rs  — PeerConnection/DataChannel setup
├── signaling.rs          — SDP offer/answer + ICE candidate exchange
└── shm.rs                — shared memory buffer (memfd)
src_c/
└── pw_capture.c          — PipeWire + D-Bus portal C helper
docs/
├── DEVELOPMENT.md        — architecture, config/protocol reference, troubleshooting
└── client-examples/
    └── client.html       — browser WebRTC client, embedded into the binary via include_str!
```

For contributor guidelines (PR checklist, scope, bug reports) see [CONTRIBUTING.md](CONTRIBUTING.md); for architecture, the wire protocol, key algorithms, and troubleshooting see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Changelog

### v0.299.2 (July 2026)
- **Fixed**: the damage-region skip bypassed hash comparison entirely, so a tile force-reset by `invalidate_tiles`/`invalidate_cache` (v0.299.1's headline fix) still never got re-detected once damage tracking was active and the tile fell outside the current frame's damage regions — it now checks a per-tile force-redetect flag before deferring to the skip.
- **Fixed**: ACK-loss recovery only re-armed a merged tile's single representative grid cell instead of every cell it covered (up to 4×4=16), so most of a lost merged-tile region could stay stale indefinitely — invalidation now expands to every covered cell.
- **Fixed**: `invalidate_tiles`/`invalidate_cache` reset hash state but left `last_sent_frame` untouched, so the FPS-throttle interval could still delay the immediate resend the invalidation was meant to trigger.
- **Fixed**: the ACK-loss epoch tag was read asynchronously in the send loop instead of being stamped when a frame's tiles were produced, racing the epoch bump across the buffered encode channel on a resolution change.
- **Fixed**: `frame_profiler.rs`'s cache simulation didn't mirror stream.rs's single-cell cache restriction (from v0.299.1) or its per-tile cache lookup, so its cache-hit/timing numbers no longer reflected production behavior.
- **Changed**: the merged-tile grid-index math (representative cell, single-cell check, covered-cell expansion) is now one shared implementation on `Tile` instead of being hand-copied per call site — the source of several of the above bugs.
- **Changed**: the `changed_mask`/`damaged_tiles` per-frame scratch buffers are reused across frames instead of being reallocated, and `damaged_tiles` is only reset on frames that actually carry damage info.

### v0.299.1 (July 2026)
- **Fixed**: `invalidate_tiles`/`invalidate_cache` (ACK-loss recovery, periodic quality refresh) were silently defeated by the zero-copy half-hash shortcut whenever a tile's pixels weren't actively changing at the moment of invalidation — they now actually force re-detection.
- **Fixed**: tiles held back by FPS throttling were misclassified as "unchanged" in the per-tile change-history/priority stats, and their hash baseline never advanced — both now update correctly even when the tile isn't sent that frame.
- **Fixed**: the per-tile encode cache could serve a stale WebP blob for a merged multi-tile region, since cache validity was keyed on only one representative cell's hash while the cached bytes covered the whole merged block — the fast path is now restricted to single-cell tiles.
- **Fixed**: the damage-region skip could leave background tiles unsent on a client's very first frame if the compositor didn't report full-frame damage on connect.
- **Fixed**: ACK-loss tile indices are now tagged with an epoch counter so they can't be misapplied to a new tile grid after a resolution change.
- **Changed**: removed four full-array clones per frame from the diff hot path (borrowing instead) and replaced two `HashSet<usize>` lookups with dense `Vec<bool>` masks — a measured ~15–25% reduction in diff-detection time on light/medium-change frames.

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
