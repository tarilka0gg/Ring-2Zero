# Ring-2Zero

[![CI](https://github.com/tarilka0gg/Ring-2Zero/actions/workflows/ci.yml/badge.svg)](https://github.com/tarilka0gg/Ring-2Zero/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Changelog](https://img.shields.io/badge/changelog-CHANGELOG.md-blue.svg)](CHANGELOG.md)
[![Contributing](https://img.shields.io/badge/contributing-CONTRIBUTING.md-blue.svg)](CONTRIBUTING.md)

A high-performance Wayland screen streaming server, written in Rust, that
streams your desktop to any browser over WebRTC — no VNC client, no X11
forwarding, just a URL.

## Overview

Ring-2Zero captures a Wayland output (via `wlr-screencopy` DMA-BUF, or
PipeWire through the xdg-desktop-portal on GNOME/KDE), cuts it into a grid
of tiles, hashes each tile to see what actually changed since the last
frame, merges the changed tiles into a handful of rectangles, encodes
those to WebP, and ships them to the browser over a WebRTC DataChannel.
The browser client is a single HTML page **embedded in the binary** — the
server is the only thing you run; open its URL and you're watching your
screen.

What that buys you in practice:

- **Low CPU, low bandwidth on a mostly-static desktop.** Two-stage hashing
  skips 85–99% of tiles on real desktop content (idle terminal, editor,
  browser tab) without ever looking at their full pixel data, and only
  the tiles that actually changed get re-encoded and sent.
- **Low latency when things move.** Tiles are prioritised by how often
  they change, how fast they're changing, and how close to the screen
  centre they are, so the parts of the screen you're actually looking at
  and interacting with get sent first and most often.
- **No X server, no VNC.** Capture is native Wayland (`wlr-screencopy`
  DMA-BUF, zero-copy on wlroots compositors) with a PipeWire/portal
  fallback for GNOME and KDE.
- **A real desktop, not a read-only picture** — optional remote mouse and
  keyboard control through the compositor's own virtual-input protocols
  (`--control`, off by default).
- **Works on a LAN, over a VPN like Tailscale, or across the open
  internet** with your own STUN/TURN servers.

It's a single Rust binary plus one embedded HTML file: no Node.js server,
no separate static file host, no system service beyond the binary itself.

## Table of contents

- [Features](#features)
- [How it works](#how-it-works)
- [Quick start](#quick-start)
- [Building](#building)
- [Configuration](#configuration)
- [Remote access](#remote-access)
- [Authentication](#authentication)
- [Remote control](#remote-control)
- [Wire protocol](#wire-protocol)
- [Performance](#performance)
- [Troubleshooting](#troubleshooting)
- [Dependencies](#dependencies)
- [Project structure](#project-structure)
- [Testing](#testing)
- [Contributing](#contributing)
- [Changelog](#changelog)
- [License](#license)

## Features

**Capture**
- **DMA-BUF zero-copy capture** via `wlr-screencopy` v3 + GBM LINEAR
  (niri, sway, and other wlroots compositors), with an SHM fallback when
  DMA-BUF isn't available.
- **PipeWire screencast** via `xdg-desktop-portal`, for GNOME, KDE and
  X11 (`--features pipewire_capture`, opt-in at build time).
- **Automatic backend detection** — picks the best capture path for the
  running compositor at startup, no configuration needed.
- **Wayland damage-region awareness** — when the compositor reports which
  regions actually changed, tiles outside them skip hashing entirely.

**Encoding & transport**
- **Tile-based diff encoding**: the screen is a grid of tiles; each one is
  hashed and only genuinely changed tiles are ever touched again.
- **Two-stage SIMD hashing** (AVX2/SSE2, half-tile pre-check + full-tile
  confirm) to tell "changed" from "unchanged" with minimal work.
- **Adaptive tile merging** — changed tiles are grouped into up to 4×4
  rectangles before encoding, cutting tile count by 83–99% on typical
  content, with the merge aggressiveness auto-tuned to your CPU's WebP
  encode speed at startup.
- **Priority-sorted, adaptive-quality WebP encoding** across a parallel
  worker pool, with a per-tile cache so a tile re-selected without its
  pixels changing skips re-encoding entirely.
- **WebRTC DataChannel streaming**, unordered but reliable, to avoid
  head-of-line blocking without losing tiles outright.
- **ACK-based loss recovery** — the client acknowledges each decoded
  batch; anything unacknowledged after 150 ms is treated as lost and
  force-resent, cell by cell, even for a merged multi-tile region.
- **Auto-reconnect**, both at the WebRTC layer (renegotiation over the
  same WebSocket) and from the browser page itself if the whole
  connection drops.

**Access & control**
- **Token authentication**, sent as the client's first WebSocket message
  (never in the URL), compared in constant time, with a distinct close
  code so the page can tell "wrong password" from "server unreachable".
- **Optional TLS** (`wss://`) for Safari/iOS, served over the same port
  as everything else.
- **STUN/TURN support** (`RING2ZERO_ICE_SERVERS`) for connecting across
  NAT, or none at all when a LAN or VPN already puts both peers on the
  same virtual network.
- **Remote control** (opt-in, `--control`) — drive the host's mouse and
  keyboard from the browser via the compositor's virtual-pointer and
  virtual-keyboard protocols.

**Engineering**
- **SIMD throughout the hot path** — AVX2/SSE2 for hashing, tile
  extraction, and BGRX→RGBA conversion, each with a portable scalar
  fallback.
- **116 unit tests** covering the diff detector, tile merger, protocol
  encode/decode, ACK tracking, ICE-server parsing, auth, and the
  remote-control input state machine, plus a Node-run behavioural test
  of the browser client's input capture. CI enforces `rustfmt` and
  `clippy -D warnings` on every push.

## How it works

```
capture thread                processing thread                 async send loop
───────────────                ─────────────────                 ───────────────
wlr::WlrCapture      Frame     DiffDetector::detect_changes       StreamServer
  or                 ─────►    (diff.rs)                          (stream.rs)
pipewire::PipeWireCapture      │                                  over the DataChannel,
(capture/mod.rs               ▼                                   with ACK tracking
 auto-detects backend)   TileMerger::merge          tiles+encoded  (transport.rs)
                         (encoder.rs)          ─────────────────►       │
                              │                    (mpsc channel)      ▼
                              ▼                                  client (ACKs)
                       priority sort +
                       EncodingPool
                       (encoding_pool.rs, pipeline.rs)
```

Every frame goes through the same short pipeline:

1. **Capture** produces a raw RGBA frame (plus any damage regions the
   compositor reported) on an `mpsc` channel.
2. **Diff** (`diff.rs`) hashes each grid tile in parallel. A cheap
   half-tile hash first checks against last frame's half-hash; only a
   mismatch triggers the full-tile hash — typically an 85–99% skip rate
   on ordinary desktop content. Tiles outside the compositor's reported
   damage regions skip hashing outright (except on the very first frame,
   or a tile force-marked for re-detection after a lost ACK). A changed
   tile is further classified "dynamic" (changing on consecutive frames)
   or "static", each with its own send-rate cap, so something like a
   blinking cursor doesn't get sent at full frame rate forever.
3. **Merge** (`encoder.rs`) groups adjacent changed tiles into rectangles
   up to 4×4 grid cells, capped so a full-screen refresh can't produce
   one oversized message. How aggressively it merges is auto-tuned once
   at startup from a WebP encode-speed microbenchmark of your CPU.
4. **Prioritise + encode**: merged tiles are sorted by a weighted score
   (how often a tile changes, how fast, how close to the screen centre)
   and encoded to WebP across a worker pool. A tile re-selected without
   its pixels actually changing (e.g. a periodic quality refresh) is
   served from a per-tile cache instead of being re-encoded.
5. **Transport** (`transport.rs`, `protocol.rs`) packs encoded tiles into
   DataChannel messages and tracks each batch's sequence number. A batch
   unacknowledged after 150 ms — checked both per-frame and on a 50 ms
   timer, so a loss right before the screen goes static is still caught
   — has every grid cell it covered queued for forced re-send.
6. **Client** (`docs/client-examples/client.html`) decodes each WebP tile
   with `createImageBitmap` and paints it onto an offscreen canvas, only
   acknowledging a batch once every tile in it has actually decoded.

Signaling (who to connect to, ICE candidates, SDP) rides a WebSocket;
media (the DataChannel above) is a normal `RTCPeerConnection`. See
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the full architecture
breakdown, the exact wire format of every message, and a deeper look at
each algorithm above (including why each design decision is the way it
is — most of them fix a real bug found along the way).

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

4. You should now see your screen streaming in the browser tab. To view it from *another* device (phone, laptop, over the internet), see [Remote access](#remote-access). To also control the machine from the browser, see [Remote control](#remote-control).

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

There's no `[profile.release]` override in `Cargo.toml`, so `cargo build --release` uses Cargo's own defaults (`opt-level = 3`, debug assertions and overflow checks off).

## Configuration

Everything is configured via environment variables plus a handful of CLI flags — no config file, so it behaves the same run directly, under systemd, or in a container.

| Variable | Default | Purpose |
|---|---|---|
| `RING2ZERO_TOKEN` | random, printed on startup | Fixed auth token instead of a fresh random one each run — set this if you want to script reconnects without re-reading stdout. |
| `RING2ZERO_TLS_CERT` / `RING2ZERO_TLS_KEY` | unset (plaintext `ws://`) | PEM cert/key paths to serve `wss://` instead. Required for Safari/iOS — see [Remote access](#remote-access). |
| `RING2ZERO_ICE_INTERFACE` | unset (all interfaces) | Restrict ICE candidate gathering to one named interface (e.g. `tailscale0`) on multi-homed machines. |
| `RING2ZERO_IPV4_ONLY` | unset (dual-stack) | Set to any value to exclude IPv6 ICE candidates — works around a dual-stack candidate-selection issue on some hosts. Don't set this on an IPv6-only path, it'll leave you with zero candidates. |
| `RING2ZERO_MAX_FPS` | unset | Caps `target_fps`/`static_tile_fps`/`dynamic_tile_fps` uniformly to N (clamped to 1–1000) — a quick bandwidth-constrained testing knob. |
| `RING2ZERO_ICE_SERVERS` | unset (host candidates only) | Comma-separated STUN/TURN servers: `stun:host:port`, `turn:user:pass@host:port[?transport=tcp]`, `turns:…`. Needed only across NAT — see [Remote access](#remote-access). The server hands the same list to the browser. A malformed entry stops startup with an error. |
| `RING2ZERO_CONTROL` | unset | Same as `--control` — see [Remote control](#remote-control). |

CLI flags:

| Flag | Effect |
|---|---|
| `--no-adaptive` | Skip the startup CPU benchmark, use the default `merge_gap=0`. |
| `--debug` | Verbose per-tile/per-frame stats every 100 frames, plus per-frame send stats (log level `debug` for this crate). |
| `--control` | Allow clients to control this machine's mouse and keyboard — see [Remote control](#remote-control). |
| `-h`, `--help` | Full flag/env var reference, paged through `less`/`$PAGER` on a real terminal. |

Logging goes through `env_logger`: the default is `warn,dtls=error,webrtc_ice=error,screen_streamer=info` (the two crate-specific overrides silence upstream WebRTC noise — a benign warning per TLS extension in every handshake, and per-candidate ICE chatter — that would otherwise bury the useful lines); `RUST_LOG` overrides it entirely. `RUST_LOG=ice=debug,webrtc_ice=debug,mdns=debug,webrtc_mdns=debug` gives verbose ICE/mDNS connectivity diagnostics when troubleshooting a connection that won't complete.

Everything else — tile grid size (`tiles_x`, default 20 columns), WebP quality range, priority weights, per-mode FPS caps — is a compile-time default in `Config` (`src/config.rs::Config::default()`); edit and rebuild to change it. The full field reference is in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#configuration-reference).

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
configure on the client side. [coturn](https://github.com/coturn/coturn)
is a solid self-hosted TURN server if you'd rather not depend on a public
STUN server.

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
- Not currently implemented for the PipeWire capture backend's compositor
  targets outside wlroots — the virtual-input protocols above are
  wlroots-specific.

**Security:** anyone with the token gets full keyboard and mouse access to
your session. It's off by default; if you enable it, keep the port
reachable only over a VPN.

## Wire protocol

A brief map — the full byte-level layout of every message is in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#wire-protocol), and
`src/protocol.rs` (server) / `docs/client-examples/client.html` (client)
are the two implementations that actually have to agree on it.

- **Signaling** rides the WebSocket as JSON text frames: client auth →
  server `hello` (protocol version, ICE servers, whether remote control is
  on) → SDP offer/answer + ICE candidates, each round tagged with a
  session id so a stale renegotiation's leftover messages are ignored.
- **`screen` DataChannel** (unordered, reliable) carries binary,
  little-endian messages: a resolution header, a per-frame sequence
  packet, back-to-back length-prefixed WebP tile payloads, and a 4-byte
  ACK in the other direction.
- **`input` DataChannel** (ordered, reliable; only exists with
  `--control`) carries one binary pointer/wheel/keyboard event per
  message.

The protocol changed in v0.400.0 (auth moved out of the URL, a `hello`
message was added) — an older cached copy of `client.html` won't connect
to this server; always use the page the binary itself serves.

## Performance

Benchmarks with fast-webp encoding (v0.277, July 2026, i7-14650HX — the
diff/merge/encode pipeline these measure hasn't changed since, though the
transport and signaling layers have; see [CHANGELOG.md](CHANGELOG.md) for
what v0.400.0 changed):

| Scenario | Time/Frame | FPS | Pipeline Breakdown |
|----------|-----------|-----|--------------------|
| 🟢 Static content | 0.13 ms | **7968 FPS** | 100% diff detection |
| 🟡 Moderate activity | 0.47 ms | **2142 FPS** | 31% diff, 3% merge, 66% encode |
| 🟠 Active work | 0.63 ms | **1589 FPS** | 39% diff, 3% merge, 58% encode |
| 🔴 Video window | 0.52 ms | **1935 FPS** | 37% diff, 3% merge, 60% encode |

("FPS" here is the pipeline's own throughput ceiling for that workload,
not the stream's actual output rate, which is capped by `target_fps` —
60 by default — and the per-tile static/dynamic send-rate limits.)

Key numbers:
- **Tile merging**: 83–99% tile reduction (e.g., 20 627 → 247 tiles)
- **Cache hits**: 41–67% tiles served without re-encoding
- **DMA-BUF vs SHM**: eliminates one kernel copy per frame on wlroots compositors

Run `cargo run --release --bin frame_profiler --features bench_tools` for a live breakdown on your own hardware — see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#benchmarking--profiling) for the rest of the profiling tools (`diff_profiler`, `hash_analyzer`, `modern_hash_bench`, `advanced_bench`, `detailed_bench`, `webp_codec_bench`).

## Troubleshooting

The full list, with more detail per item, is in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#troubleshooting). The short
version:

- **WebRTC won't connect** — confirm the port is listening
  (`ss -tlnp | grep 9001`), check the browser console (F12) for ICE
  errors, and run the server with
  `RUST_LOG=ice=debug,webrtc_ice=debug,mdns=debug,webrtc_mdns=debug` for
  matching server-side detail. Safari/iOS specifically needs
  `RING2ZERO_TLS_CERT`/`KEY` (see [Remote access](#remote-access)). A
  multi-homed server (LAN + VPN) may need `RING2ZERO_ICE_INTERFACE`.
- **Low FPS / high frame time** — run `frame_profiler` (see
  [Performance](#performance)) to see which pipeline stage dominates:
  diff detection being slow usually just means the content is genuinely
  fully dynamic (full-screen video); encoding being slow means lowering
  `webp_quality_high`/`webp_quality_low` or `tiles_x` in
  `Config::default()` will help.
- **High CPU usage** — check `tiles_x` isn't unreasonably high for the
  resolution, and don't leave `--debug` on in production (it adds
  per-frame logging overhead). `top -H -p $(pgrep ring-2zero)` shows
  per-thread usage; the encoding pool defaults to `num_cpus::get().max(4)`
  worker threads.
- **Build errors** — `./install.sh --dry-run` checks exactly what's
  missing (pkg-config modules, clang, cargo) without changing anything.
  Most commonly it's missing Wayland dev headers or a `clang` binary not
  on `PATH` — see [Building](#building) and
  [Dependencies](#dependencies).
- **PipeWire backend won't capture** — needs `libpipewire-0.3` and
  `libdbus-1` dev headers to build, and a running
  `xdg-desktop-portal` + a compositor-specific portal backend
  (`xdg-desktop-portal-wlr`, `-gnome`, `-kde`, …) to run.

## Dependencies

`./install.sh` detects and installs all of these for you (see [Quick start](#quick-start)). For reference, or if installing by hand:

System libraries required:
- `libwayland-client` — Wayland protocol
- `libgbm` — GBM buffer allocation for DMA-BUF path
- `libdrm` — DRM render node access

Optional (for `--features pipewire_capture`):
- `libpipewire-0.3` — PipeWire stream
- `libdbus-1` — xdg-desktop-portal D-Bus handshake

Plus a C compiler (`clang`, pinned by `.cargo/config.toml`) and a recent
stable Rust toolchain — `install.sh` installs both if missing.

## Project Structure

```
install.sh              — dependency detection/install, build, install, shell alias
CHANGELOG.md             — full version history
CONTRIBUTING.md          — PR checklist, scope, bug-report guidelines
man/ring-2zero.1         — man page, installed by install.sh
src/
├── main.rs               — entry point: CLI flags, logging setup, TCP accept loop
├── server.rs             — WebSocket upgrade + WebRTC session loop, serves the client page over HTTP(S)
├── auth.rs                — first-message token authentication
├── signaling.rs           — SDP offer/answer + ICE candidate exchange
├── webrtc_connection.rs   — PeerConnection/DataChannel setup
├── ice.rs                 — RING2ZERO_ICE_SERVERS (STUN/TURN) parsing
├── stream.rs               — one streaming session: glues the pipeline thread to the send loop
├── pipeline.rs              — diff → merge → prioritise → encode, per frame (Pipeline::process)
├── transport.rs             — ACK tracking + framing onto the DataChannel
├── protocol.rs               — binary wire format (DataChannel messages), pure encode/decode
├── input.rs                — remote control: virtual pointer/keyboard injection (--control)
├── capture/
│   ├── mod.rs               — backend auto-detection
│   ├── wlr.rs                — wlr-screencopy (DMA-BUF + SHM fallback)
│   └── pipewire.rs            — PipeWire via portal (feature-gated)
├── diff.rs                  — tile change detection (two-stage hashing, damage skip, FPS throttling)
├── encoder.rs                — tile merging into rectangles
├── encoding_pool.rs           — parallel WebP encoding worker pool
├── tile.rs                    — Tile/TileMetadata/Grid, SIMD hashing (AVX2/SSE2)
├── tile_extract.rs             — SIMD tile pixel extraction (AVX2/SSE2)
├── convert.rs                   — SIMD BGRX→RGBA conversion (AVX2/SSE2)
├── config.rs                     — Config struct + CPU benchmark cache
└── shm.rs                         — shared memory buffer (memfd), used by the wlr backend
src_c/
└── pw_capture.c            — PipeWire + xdg-desktop-portal D-Bus C helper
docs/
├── DEVELOPMENT.md          — full architecture, config/protocol reference, algorithms, troubleshooting
└── client-examples/
    ├── client.html          — browser WebRTC client, embedded into the binary via include_str!
    └── input-capture.test.js — Node-run behavioural test of the client's input-capture logic
```

For contributor guidelines (PR checklist, scope, bug reports) see [CONTRIBUTING.md](CONTRIBUTING.md); for architecture, the wire protocol, key algorithms, and troubleshooting see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Testing

```bash
cargo test --release                          # 116 unit tests: diff, merge, protocol, ACK tracking, auth, input state, …
cargo test --release --features pipewire_capture
cargo clippy --all-targets -- -D warnings     # enforced in CI, no warnings allowed
cargo fmt --check                              # enforced in CI
node docs/client-examples/input-capture.test.js  # behavioural test of the browser client's input capture
```

CI (`.github/workflows/ci.yml`) runs all of the above, plus a build with
`--features pipewire_capture`, on every push.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full checklist. In short:
`cargo build --release` (both with and without `--features
pipewire_capture`), `cargo test --release`, `cargo fmt`, and no new
`clippy`/`cargo build` warnings, before opening a PR. For a new capture
backend or a change to the WebRTC data-channel protocol, open an issue
first to discuss the approach.

## Changelog

Latest (**v0.400.0**, "remaster"): remote mouse/keyboard control
(`--control`), STUN/TURN support for use across NAT, the auth token moved
out of the URL into the first WebSocket message, the streaming core split
into small tested modules (`pipeline.rs`/`transport.rs`/`protocol.rs`), a
working PipeWire portal backend (the D-Bus negotiation was completely
broken before), and a batch of reliability fixes — lost tiles on a static
screen now get re-sent, frame pacing no longer drifts, a closed tab no
longer keeps the capture thread running for 30 seconds, the rightmost
pixel columns on non-round screen widths now update, and the client
reconnects on its own. **The client/server protocol changed** — use the
page served by the same binary; an older cached `client.html` won't
connect.

Full version history back to the first tagged release: [CHANGELOG.md](CHANGELOG.md).

## License

MIT — see [LICENSE](LICENSE)
