# Ring-2Zero

[![CI](https://github.com/tarilka0gg/Ring-2Zero/actions/workflows/ci.yml/badge.svg)](https://github.com/tarilka0gg/Ring-2Zero/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Changelog](https://img.shields.io/badge/changelog-CHANGELOG.md-blue.svg)](CHANGELOG.md)
[![Contributing](https://img.shields.io/badge/contributing-CONTRIBUTING.md-blue.svg)](CONTRIBUTING.md)

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
- **Remote control** (opt-in, `--control`) — drive the host's mouse and keyboard from the browser, via the compositor's virtual-pointer/virtual-keyboard protocols
- **Works across NAT** with your own STUN/TURN servers (`RING2ZERO_ICE_SERVERS`), or with none at all on a LAN/VPN

## Quick start

The browser client is baked into the binary — there's no separate file to open or static server to run. Once the server is up, one URL is the whole client.

1. **Clone and install**:
   ```bash
   git clone https://github.com/tarilka0gg/Ring-2Zero.git
   cd Ring-2Zero
   ./install.sh
   ```
   `install.sh` detects your distro's package manager and installs missing system libraries (asking for confirmation first), finds or installs a C compiler and Rust toolchain, builds a release binary, installs it as `ring-2zero` on `PATH` (`cargo install --path .` under the hood), adds an `r2zr` alias to whichever shell actually launched it (bash/zsh/fish; anything else falls back to `~/.profile`), and installs a man page (`man ring-2zero`). Every failure prints what went wrong and how to fix it. Options: `-y`/`--yes` (don't prompt before installing packages), `--pipewire` (build with PipeWire capture support too), `--dry-run` (show what it would do), `--no-alias`. See [Dependencies](#dependencies) below for what it's installing, or [Building](#building) to do it by hand instead.

2. **Run the server**:
   ```bash
   ring-2zero            # or the r2zr alias install.sh just added, in a new shell
   ```
   The first run benchmarks your CPU's WebP encoding speed to pick a sensible tile-merging setting, and caches the result (`~/.cache/screen-streamer/cpu_bench.json`) so it only costs a couple seconds once. Skip it with `--no-adaptive`. `--help` prints the full flag/env var reference.

   Startup prints what you need to connect:
   ```
   WebRTC signaling server (WebSocket): ws://0.0.0.0:9001
   TLS disabled — set RING2ZERO_TLS_CERT/RING2ZERO_TLS_KEY for wss:// (required for Safari/iOS remote access)
   Auth token: 3f9a1c...
   Open http://<this-host>:9001 in a browser (password prompt uses the token above) — no separate client file needed, this binary serves the page itself
   ```

3. **Open that URL** in a browser — e.g. `http://localhost:9001`. On first load it prompts for the auth token printed above; paste it once, it's remembered in the browser's `localStorage` from then on. The page auto-detects the server address it was loaded from, so the same URL keeps working unchanged when you switch to [Remote access](#remote-access) (Tailscale, etc.) below — no `?server=` param needed unless you're hosting the page somewhere other than this binary.

4. You should now see your screen streaming in the browser tab. To view it from *another* device (phone, laptop, over the internet), see [Remote access](#remote-access).

## Building

`./install.sh` (see [Quick start](#quick-start)) finds a compiler and does all of this automatically. To do it by hand instead:

```bash
# Standard build (wlr-screencopy only)
cargo build --release

# With PipeWire support (GNOME, KDE, X11)
cargo build --release --features pipewire_capture

# Optional: put `ring-2zero` on PATH instead of typing target/release/ring-2zero
cargo install --path .
```

`.cargo/config.toml` pins the linker to `clang` — if it's not the default `cc`/`clang` on your `PATH`, set `CC`/`CXX` (and put its directory on `PATH`, since the linker itself is resolved there too) before building. `install.sh` does this detection for you, including Gentoo's slotted `/usr/lib/llvm/<version>/bin/clang` layout.

## Configuration

Everything is configured via environment variables plus two CLI flags — no config file, so it behaves the same run directly, under systemd, or in a container.

| Variable | Default | Purpose |
|---|---|---|
| `RING2ZERO_TOKEN` | random, printed on startup | Fixed auth token instead of a fresh random one each run — set this if you want to script reconnects without re-reading stdout. |
| `RING2ZERO_TLS_CERT` / `RING2ZERO_TLS_KEY` | unset (plaintext `ws://`) | PEM cert/key paths to serve `wss://` instead. Required for Safari/iOS — see [Remote access](#remote-access). |
| `RING2ZERO_ICE_INTERFACE` | unset (all interfaces) | Restrict ICE candidate gathering to one named interface (e.g. `tailscale0`) on multi-homed machines. |
| `RING2ZERO_IPV4_ONLY` | unset (dual-stack) | Set to any value to exclude IPv6 ICE candidates — works around a dual-stack candidate-selection issue on some hosts. Don't set this on an IPv6-only path, it'll leave you with zero candidates. |
| `RING2ZERO_MAX_FPS` | unset | Caps `target_fps`/`static_tile_fps`/`dynamic_tile_fps` uniformly to N (clamped to 1–1000) — a quick bandwidth-constrained testing knob. |
| `RING2ZERO_ICE_SERVERS` | unset (host candidates only) | Comma-separated STUN/TURN servers: `stun:host:port`, `turn:user:pass@host:port[?transport=tcp]`, `turns:…`. Needed only across NAT — see [Remote access](#remote-access). The server hands the same list to the browser. A malformed entry stops startup with an error. |
| `RING2ZERO_CONTROL` | unset | Same as `--control`. |

CLI flags:

| Flag | Effect |
|---|---|
| `--no-adaptive` | Skip the startup CPU benchmark, use the default `merge_gap=0`. |
| `--debug` | Verbose per-tile/per-frame stats every 100 frames, plus per-frame send stats (log level `debug`). |
| `--control` | Allow clients to control this machine's mouse and keyboard — see [Remote control](#remote-control). |

Logging goes through `env_logger`: the default is `warn,dtls=error,screen_streamer=info`; `RUST_LOG` overrides it. `RUST_LOG=ice=debug,webrtc_ice=debug,mdns=debug,webrtc_mdns=debug` gives verbose ICE/mDNS connectivity diagnostics when troubleshooting a connection.

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

Without a VPN, peers behind different NATs need STUN (and, behind
symmetric NATs, a TURN relay) — pass your own via `RING2ZERO_ICE_SERVERS`,
e.g. `stun:stun.l.google.com:19302,turn:user:pass@turn.example.org:3478`.
The browser gets the same list after logging in, so there's nothing to
configure on the client side.

Find the server's Tailscale hostname (`tailscale status`, or rename the
device with `tailscale set --hostname=<name>` for a nicer URL), then open
`https://<name>.<tailnet>.ts.net:9001` from the viewing device — the page
auto-detects that address and connects back to it, no `?server=` param
needed.

## Authentication

The signaling server requires a token. By default a random one is
generated on each startup and printed to stdout; set `RING2ZERO_TOKEN` to
use a fixed one instead.

The token is the client's **first WebSocket message**
(`{"type":"auth","token":"…"}`), never part of the URL — so it doesn't end
up in browser history or proxy access logs. It's compared in constant
time; a client that doesn't send a valid one within 5 s is dropped, and a
wrong one is answered (after a 1 s delay, against brute-forcing a weak
`RING2ZERO_TOKEN`) with WebSocket close code `4001`. That code is what lets
the page tell "wrong password" (asks again) apart from "server unreachable"
(keeps the saved password and retries).

The page prompts for the password on first load and remembers it in the
browser's `localStorage`.

This guards the signaling handshake itself, but is still no substitute for
network-level isolation — prefer keeping the port reachable only over a
VPN (like Tailscale above) rather than a public port-forward.

## Remote control

Start the server with `--control` (or `RING2ZERO_CONTROL=1`) and a
**КЕРУВАННЯ** button appears in the page's status bar. While it's on,
mouse, wheel and keyboard input over the picture is sent to the host.

- Requires `zwlr_virtual_pointer_manager_v1` and
  `zwp_virtual_keyboard_manager_v1` (niri, sway and other wlroots
  compositors have both). The virtual keyboard reuses the compositor's
  active keymap, and keys are sent by physical position
  (`KeyboardEvent.code`), so the host's layout decides what gets typed.
- The pointer is bound to the streamed output, so clicks land where you
  see them even with several monitors.
- Everything still held down is released automatically when the tab loses
  focus, control is switched off, or the connection drops — no stuck keys.
- Some shortcuts (Ctrl+W, Ctrl+T, …) are reserved by the browser and never
  reach the page; fullscreen or an installed PWA window lets more through.

**Security:** anyone with the token gets full keyboard and mouse access to
your session. It's off by default; if you enable it, keep the port
reachable only over a VPN.

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

`./install.sh` detects and installs all of these for you (see [Quick start](#quick-start)). For reference, or if installing by hand:

System libraries required:
- `libwayland-client` — Wayland protocol
- `libgbm` — GBM buffer allocation for DMA-BUF path
- `libdrm` — DRM render node access

Optional (for `--features pipewire_capture`):
- `libpipewire-0.3` — PipeWire stream
- `libdbus-1` — xdg-desktop-portal D-Bus handshake

## Project Structure

```
install.sh              — dependency detection/install, build, install, shell alias
CHANGELOG.md            — full version history
man/ring-2zero.1         — man page, installed by install.sh
src/
├── main.rs               — entry point
├── server.rs             — WebSocket + WebRTC server, serves the client page over HTTP(S)
├── auth.rs               — first-message token authentication
├── stream.rs             — one streaming session: glues the threads together
├── pipeline.rs           — diff → merge → prioritise → encode, per frame
├── transport.rs          — ACK tracking + sending over the DataChannel
├── protocol.rs           — binary wire format (DataChannel messages)
├── input.rs              — remote control: virtual pointer/keyboard injection
├── ice.rs                — STUN/TURN configuration
├── capture/
│   ├── mod.rs            — backend auto-detection
│   ├── wlr.rs            — wlr-screencopy (DMA-BUF + SHM fallback)
│   └── pipewire.rs       — PipeWire via portal (feature-gated)
├── diff.rs               — tile change detection
├── encoder.rs            — tile merging
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

Latest (**v0.400.0**, "remaster"): remote mouse/keyboard control (`--control`), STUN/TURN support for use across NAT, the auth token moved out of the URL into the first WebSocket message, the streaming core split into small tested modules, and a batch of reliability fixes (lost tiles on a static screen now get re-sent, frame pacing no longer drifts, the client reconnects on its own). **The client/server protocol changed** — use the page served by the same binary.

Full version history: [CHANGELOG.md](CHANGELOG.md).

## License

MIT — see [LICENSE](LICENSE)
