# Packaging

`packaging/package.sh` → `dist/ring-2zero-<version>-linux-<arch>.tar.gz` (+ `.sha256`). `PIPEWIRE=1` also builds the PipeWire capture path. Gentoo: `packaging/gentoo/media-video/ring-2zero/` (live ebuild, USE `pipewire`, branch `remaster`). Desktop entry, AppStream data, man page and `r2zr` alias are included.

Install from the tarball: `./install.sh` (under `/usr/local`, `PREFIX=$HOME/.local` works without root), `DESTDIR=… ./install.sh` to stage, `./install.sh uninstall` to remove
what it installed (it records a manifest). Existing files in `/etc` are never overwritten; the new copy is written as `<name>.new`.
The repository's own top-level `install.sh` is different: it installs build dependencies and compiles from source.
Checked: tarball install/uninstall in a DESTDIR, desktop-file-validate, appstreamcli, `emerge -pv` on the ebuild (not built).
