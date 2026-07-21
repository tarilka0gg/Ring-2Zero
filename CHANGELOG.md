# Changelog

## v0.300.0 (July 2026)
- **Added**: `install.sh` — detects the system package manager, checks/installs the required libraries and a C compiler, installs Rust if missing, builds and installs the binary, adds an `r2zr` shell alias (bash/zsh/fish), and installs a `man ring-2zero` page.
- **Added**: a real man(1) page (`man/ring-2zero.1`) covering the same ground as `--help`.
- **Added**: a startup splash banner (block-letter wordmark), shown on an actual terminal only, colorless when `NO_COLOR` is set.
- **Changed**: `--help` now goes through a pager (`less -R -F -X`, or `$PAGER`) when stdout is a terminal, opening at the top like `man` — the banner used to scroll off the top of a normal-height terminal before you could read it. Piped/redirected `--help` output is untouched (plain text, no pager).
- **Changed**: `Cargo.lock` is now committed instead of gitignored — this is a binary application, so a reproducible dependency set matters more than the flexibility a library gets from omitting it.
- **Added**: 70 unit tests across every previously-untested module (`diff.rs`, `stream.rs`, `encoder.rs`, `tile.rs`, `config.rs`, `server.rs`, `convert.rs`, `encoding_pool.rs`, `shm.rs`, `frame.rs`, `error.rs`), including regression tests for the two `invalidate_tiles`/throttling bugs fixed in v0.299.1/.2. Also found and documented (not fixed) a real hash collision: `hash_tile` can hash two *different* solid-color tiles of the same size identically, since the XOR-accumulator's `low64 ^ high64` reduction cancels to 0 for any byte-uniform buffer — narrow in practice (real screen content is rarely perfectly uniform), but real.
- **Changed**: documentation overhaul — README gained a real Quick Start tutorial and a full env var/CLI flag reference; `docs/DEVELOPMENT.md` replaced three stale, pre-0.299 docs files with one architecture/protocol/algorithm/troubleshooting reference; full version history moved out of README into this file.
- **Removed**: tracked build artifacts (`librust_out.rlib`, `test_benchmark.rs`) and three superseded client-examples HTML drafts that predated `client.html`.

## v0.299.2 (July 2026)
- **Fixed**: the damage-region skip bypassed hash comparison entirely, so a tile force-reset by `invalidate_tiles`/`invalidate_cache` (v0.299.1's headline fix) still never got re-detected once damage tracking was active and the tile fell outside the current frame's damage regions — it now checks a per-tile force-redetect flag before deferring to the skip.
- **Fixed**: ACK-loss recovery only re-armed a merged tile's single representative grid cell instead of every cell it covered (up to 4×4=16), so most of a lost merged-tile region could stay stale indefinitely — invalidation now expands to every covered cell.
- **Fixed**: `invalidate_tiles`/`invalidate_cache` reset hash state but left `last_sent_frame` untouched, so the FPS-throttle interval could still delay the immediate resend the invalidation was meant to trigger.
- **Fixed**: the ACK-loss epoch tag was read asynchronously in the send loop instead of being stamped when a frame's tiles were produced, racing the epoch bump across the buffered encode channel on a resolution change.
- **Fixed**: `frame_profiler.rs`'s cache simulation didn't mirror stream.rs's single-cell cache restriction (from v0.299.1) or its per-tile cache lookup, so its cache-hit/timing numbers no longer reflected production behavior.
- **Changed**: the merged-tile grid-index math (representative cell, single-cell check, covered-cell expansion) is now one shared implementation on `Tile` instead of being hand-copied per call site — the source of several of the above bugs.
- **Changed**: the `changed_mask`/`damaged_tiles` per-frame scratch buffers are reused across frames instead of being reallocated, and `damaged_tiles` is only reset on frames that actually carry damage info.
- **Added**: `install.sh` — detects/installs system dependencies and a C compiler, builds, installs the binary, and adds an `r2zr` shell alias.
- **Added**: the browser client is now served directly by the binary (embedded via `include_str!`) over the same host/port as the WebSocket, including over TLS — no separate static file server needed.
- **Added**: `-h`/`--help`.

## v0.299.1 (July 2026)
- **Fixed**: `invalidate_tiles`/`invalidate_cache` (ACK-loss recovery, periodic quality refresh) were silently defeated by the zero-copy half-hash shortcut whenever a tile's pixels weren't actively changing at the moment of invalidation — they now actually force re-detection.
- **Fixed**: tiles held back by FPS throttling were misclassified as "unchanged" in the per-tile change-history/priority stats, and their hash baseline never advanced — both now update correctly even when the tile isn't sent that frame.
- **Fixed**: the per-tile encode cache could serve a stale WebP blob for a merged multi-tile region, since cache validity was keyed on only one representative cell's hash while the cached bytes covered the whole merged block — the fast path is now restricted to single-cell tiles.
- **Fixed**: the damage-region skip could leave background tiles unsent on a client's very first frame if the compositor didn't report full-frame damage on connect.
- **Fixed**: ACK-loss tile indices are now tagged with an epoch counter so they can't be misapplied to a new tile grid after a resolution change.
- **Changed**: removed four full-array clones per frame from the diff hot path (borrowing instead) and replaced two `HashSet<usize>` lookups with dense `Vec<bool>` masks — a measured ~15–25% reduction in diff-detection time on light/medium-change frames.

## v0.299 (July 2026)
- **Fixed**: `RTCPeerConnection` was never explicitly closed on drop, leaking the ICE/DTLS/SCTP transport stack on every reconnect — now closed via `Drop`.
- **Fixed**: DataChannel accidentally lost its `ordered: false` setting, reintroducing head-of-line-blocking latency.
- **Fixed**: TLS cert/key load failure now returns a clean error and exits instead of panicking.
- **Changed**: `RING2ZERO_IPV4_ONLY` is now read into `Config` (was previously unconditional) — IPv6-only deployments no longer get zero ICE candidates.
- **Changed**: `RING2ZERO_TLS_CERT/KEY`, `RING2ZERO_ICE_INTERFACE`, `RING2ZERO_MAX_FPS` consolidated into `Config`, matching the existing `RING2ZERO_TOKEN` pattern.

## v0.291 (July 2026)
- **Fixed**: ICE candidates from the client were dropped whenever they arrived before the SDP answer (a common race, since the client fires `onicecandidate` before sending its answer) — they're now buffered and applied once the remote description is set. This was silently breaking nearly every non-localhost connection.
- **Fixed**: Safari/Chrome obfuscate host ICE candidates behind a `<uuid>.local` mDNS name; mDNS resolution is now explicitly enabled so these candidates actually resolve instead of being unusable.
- **Fixed**: a full-screen refresh could merge every dirty tile into one oversized message exceeding the DataChannel's message-size limit — merged tiles are now capped to a bounded grid size.
- **Added**: TLS support (`RING2ZERO_TLS_CERT`/`RING2ZERO_TLS_KEY`) — required for Safari/iOS, which refuses WebRTC on an insecure page.
- **Added**: `RING2ZERO_ICE_INTERFACE` to restrict ICE candidate gathering to one network interface (e.g. `tailscale0`) on multi-homed machines.
- **Added**: password-gated client — `client.html` prompts for the token instead of taking it via URL, and remembers it in `localStorage`.
- **Changed**: ICE disconnect/keepalive timeouts relaxed from same-machine-tuned defaults to values tolerant of real network latency/jitter.

## v0.277 (July 2026)
- **Added**: DMA-BUF zero-copy capture via wlr-screencopy v3 + libgbm (LINEAR GBM buffer, mmap read)
- **Added**: PipeWire screencast backend via xdg-desktop-portal (GNOME, KDE, X11)
- **Added**: Auto-detection of capture backend at startup
- **Added**: ACK feedback system — 6-byte control packet, client confirms frame, server re-sends lost tiles
- **Added**: Auto-reconnect — stream survives WebRTC re-negotiations without restarting WebSocket

## v0.202 (June 19, 2026)
- Fixed duplicate function in webp_codec_bench
- FPS metrics now correctly reflect fast-webp performance

## v0.181 (June 18, 2026)
- Upgraded webp 0.3 → fast-webp 0.1.1 (2–3× faster encoding)
- Arc-based tile cache (zero-copy, ~120 MB/s bandwidth saved)
- SIMD tile extraction (AVX2/SSE2)
- 3× overall pipeline speedup

## v0.160 (June 17, 2026)
- Tile merging (83–99% reduction)
- Zero-copy hashing with SIMD
- CPU benchmark caching
