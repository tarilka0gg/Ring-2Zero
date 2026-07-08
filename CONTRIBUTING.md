# Contributing to Ring-2Zero

Thanks for considering a contribution. This project is a Wayland screen streamer over WebRTC — capture, diff, encode, transport.

## Getting started

```bash
git clone https://github.com/tarilka0gg/ring-2zero.git
cd ring-2zero
cargo build --release
```

System dependencies: `libwayland-client`, `libgbm`, `libdrm`. For PipeWire support add `libpipewire-0.3` and `libdbus-1`, then build with `--features pipewire_capture`.

## Before opening a PR

- `cargo build --release` and `cargo build --release --features pipewire_capture` both pass
- `cargo test --release` passes
- `cargo fmt` applied
- No new warnings from `cargo build`

## Scope

- Bug fixes, capture backend improvements, and encoding/performance work are welcome.
- The benchmark binaries under `src/bin/` (gated behind `bench_tools`/`webp_bench` features) are internal tooling — changes there should stay minimal and not affect the default build.
- For larger changes (new capture backend, protocol changes to the WebRTC data channel), open an issue first to discuss the approach.

## Reporting bugs

Include your compositor (niri, sway, GNOME, KDE, etc.), whether you built with `pipewire_capture`, and the exact error from stderr.
