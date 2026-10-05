#!/bin/bash
# package.sh - build a release and pack it: dist/ring-2zero-<version>-linux-<arch>.tar.gz (+ .sha256).
#   packaging/package.sh                 build with cargo and pack
#   PIPEWIRE=1 packaging/package.sh      also build the PipeWire capture path (GNOME/KDE; needs libpipewire, dbus)
#   SKIP_BUILD=1 packaging/package.sh    pack what target/release already holds
# The tarball has `install.sh` (install / uninstall, PREFIX, DESTDIR) and `prefix/`. The repo's own
# top-level install.sh is different: it installs build dependencies and compiles from source.
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=ring-2zero
ID=io.github.tarilka0gg.Ring2Zero
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
# .cargo/config.toml links with clang; Gentoo keeps it under /usr/lib/llvm/<n>/bin instead of on PATH.
if ! command -v clang >/dev/null 2>&1; then
    for d in $(ls -d /usr/lib/llvm/*/bin 2>/dev/null | sort -rV); do
        [ -x "$d/clang" ] && { PATH="$d:$PATH"; break; }
    done
fi
if [ "${SKIP_BUILD:-}" != 1 ]; then
    cargo build --release --locked --bin ring-2zero ${PIPEWIRE:+--features pipewire_capture}
fi

D=dist/$NAME-$VERSION
rm -rf "${D:?}"
install -Dm755 target/release/ring-2zero -t "$D/prefix/bin"
ln -s ring-2zero "$D/prefix/bin/r2zr"
install -Dm644 man/ring-2zero.1 -t "$D/prefix/share/man/man1"
install -Dm644 "packaging/$ID.desktop" -t "$D/prefix/share/applications"
install -Dm644 "packaging/$ID.metainfo.xml" -t "$D/prefix/share/metainfo"
install -Dm644 packaging/icons/$NAME.svg -t "$D/prefix/share/icons/hicolor/scalable/apps"
install -Dm644 README.md LICENSE CHANGELOG.md -t "$D/prefix/share/doc/$NAME"
install -m755 packaging/install.sh "$D/install.sh"
cat > "$D/POST-INSTALL.txt" <<'TXT'
Run `ring-2zero` (or `r2zr`) inside your Wayland session and open the printed URL in a browser; the token it
prints is the password. Needs a wlroots compositor (niri, sway, ...) or, with the PipeWire build, GNOME/KDE.
Runtime libraries: wayland, libdrm, gbm (mesa); pipewire and dbus for the PipeWire build.
TXT

OUT=dist/$NAME-$VERSION-linux-$ARCH.tar.gz
tar -C dist --owner=0 --group=0 -czf "$OUT" "$NAME-$VERSION"
(cd dist && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")
echo "$OUT"
