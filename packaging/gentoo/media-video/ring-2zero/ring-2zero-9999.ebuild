# Copyright 1999-2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

CRATES=""
# Live ebuild: cargo_live_src_unpack fetches the crates at unpack time (network needed, allowed for 9999).
# For a versioned ebuild, generate CRATES from Cargo.lock with pycargoebuild.

inherit cargo git-r3 xdg

DESCRIPTION="Wayland screen streaming server: tile-diffed WebP over WebRTC to any browser"
HOMEPAGE="https://github.com/tarilka0gg/Ring-2Zero"
EGIT_REPO_URI="https://github.com/tarilka0gg/Ring-2Zero.git"
EGIT_BRANCH="remaster"

LICENSE="MIT"
SLOT="0"
KEYWORDS=""
IUSE="pipewire"

# The repository's .cargo/config.toml links with clang.
RDEPEND="
	dev-libs/wayland
	media-libs/mesa
	x11-libs/libdrm
	pipewire? (
		media-video/pipewire
		sys-apps/dbus
	)
"
DEPEND="${RDEPEND}"
BDEPEND="
	llvm-core/clang
	virtual/pkgconfig
"

QA_FLAGS_IGNORED="usr/bin/ring-2zero"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_compile() {
	cargo_src_compile --bin ring-2zero $(usev pipewire '--features pipewire_capture')
}

src_install() {
	cargo_src_install --bin ring-2zero $(usev pipewire '--features pipewire_capture')
	dosym ring-2zero /usr/bin/r2zr

	doman man/ring-2zero.1
	domenu packaging/io.github.tarilka0gg.Ring2Zero.desktop
	insinto /usr/share/metainfo
	doins packaging/io.github.tarilka0gg.Ring2Zero.metainfo.xml
	insinto /usr/share/icons/hicolor/scalable/apps
	doins packaging/icons/ring-2zero.svg
	dodoc README.md CHANGELOG.md
}
