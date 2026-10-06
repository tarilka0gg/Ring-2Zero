# Copyright 2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

CRATES="
	aead@0.5.2
	aes-gcm@0.10.3
	aes@0.8.4
	aho-corasick@1.1.4
	anstream@1.0.0
	anstyle-parse@1.0.0
	anstyle-query@1.1.5
	anstyle-wincon@3.0.11
	anstyle@1.0.14
	arc-swap@1.9.2
	asn1-rs-derive@0.5.1
	asn1-rs-impl@0.2.0
	asn1-rs@0.6.2
	async-trait@0.1.91
	atomic-waker@1.1.2
	autocfg@1.5.1
	base16ct@0.2.0
	base64@0.22.1
	base64ct@1.8.3
	bin-rs@0.0.10
	bitflags@1.3.2
	bitflags@2.13.1
	block-buffer@0.10.4
	block-padding@0.3.3
	bumpalo@3.20.3
	bytecheck@0.8.2
	bytecheck_derive@0.8.2
	bytemuck@1.25.2
	byteorder-lite@0.1.0
	byteorder@1.5.0
	bytes@1.12.1
	cbc@0.1.2
	cc@1.3.0
	ccm@0.5.0
	cfg-if@1.0.4
	chacha20@0.9.1
	chacha20poly1305@0.10.1
	cipher@0.4.4
	colorchoice@1.0.5
	const-oid@0.9.6
	cpufeatures@0.2.17
	crc-catalog@2.5.0
	crc@3.4.0
	crossbeam-channel@0.5.16
	crossbeam-deque@0.8.7
	crossbeam-epoch@0.9.20
	crossbeam-queue@0.3.13
	crossbeam-utils@0.8.22
	crossbeam@0.8.4
	crypto-bigint@0.5.5
	crypto-common@0.1.7
	ctr@0.9.2
	curve25519-dalek-derive@0.1.1
	curve25519-dalek@4.1.3
	data-encoding@2.11.0
	defmt-macros@1.1.1
	defmt-parser@1.0.0
	defmt@1.1.1
	der-parser@9.0.0
	der@0.7.10
	deranged@0.5.8
	digest@0.10.7
	displaydoc@0.2.6
	downcast-rs@1.2.1
	dtls@0.17.2
	ecdsa@0.16.9
	either@1.16.0
	elliptic-curve@0.13.8
	enough@0.4.4
	env_filter@2.0.0
	env_logger@0.11.11
	equivalent@1.0.2
	errno@0.3.14
	fast-webp@0.1.1
	ff@0.13.1
	fiat-crypto@0.2.9
	find-msvc-tools@0.1.9
	fixedbitset@0.5.7
	foldhash@0.1.5
	form_urlencoded@1.2.2
	futures-channel@0.3.33
	futures-core@0.3.33
	futures-executor@0.3.33
	futures-io@0.3.33
	futures-macro@0.3.33
	futures-sink@0.3.33
	futures-task@0.3.33
	futures-util@0.3.33
	futures@0.3.33
	generic-array@0.14.7
	getrandom@0.2.17
	getrandom@0.3.4
	getrandom@0.4.3
	ghash@0.5.1
	glob@0.3.4
	group@0.13.0
	hashbrown@0.15.5
	hashbrown@0.17.1
	hermit-abi@0.5.2
	hex@0.4.3
	hkdf@0.12.4
	hmac@0.12.1
	http@1.4.2
	httparse@1.10.1
	icu_collections@2.2.0
	icu_locale_core@2.2.0
	icu_normalizer@2.2.0
	icu_normalizer_data@2.2.0
	icu_properties@2.2.0
	icu_properties_data@2.2.0
	icu_provider@2.2.0
	idna@1.1.0
	idna_adapter@1.2.2
	image@0.25.10
	imgref@1.12.2
	indexmap@2.14.0
	inout@0.1.4
	interceptor@0.17.2
	ipnet@2.12.0
	is_terminal_polyfill@1.70.2
	itoa@1.0.18
	jiff-core@0.1.0
	jiff-static@0.2.34
	jiff@0.2.34
	jobserver@0.1.35
	js-sys@0.3.103
	lazy_static@1.5.0
	libc@0.2.188
	libwebp-sys@0.14.4
	libwebp-sys@0.9.6
	linux-raw-sys@0.12.1
	litemap@0.8.2
	lock_api@0.4.14
	log@0.4.33
	md-5@0.10.6
	memchr@2.8.3
	memoffset@0.7.1
	minimal-lexical@0.2.1
	mio@1.2.2
	moxcms@0.8.1
	munge@0.4.7
	munge_macro@0.4.7
	nix@0.26.4
	nom@7.1.3
	nom@8.0.0
	num-bigint@0.4.8
	num-conv@0.2.2
	num-integer@0.1.46
	num-traits@0.2.19
	num_cpus@1.17.0
	oid-registry@0.7.1
	once_cell@1.21.4
	once_cell_polyfill@1.70.2
	opaque-debug@0.3.1
	os_pipe@1.2.3
	p256@0.13.2
	p384@0.13.1
	parking_lot@0.12.5
	parking_lot_core@0.9.12
	pem-rfc7468@0.7.0
	pem@3.0.6
	percent-encoding@2.3.2
	petgraph@0.8.3
	pin-project-lite@0.2.17
	pin-utils@0.1.0
	pkcs8@0.10.2
	pkg-config@0.3.33
	poly1305@0.8.0
	polyval@0.6.2
	portable-atomic-util@0.2.7
	portable-atomic@1.14.0
	potential_utf@0.1.5
	powerfmt@0.2.0
	ppv-lite86@0.2.21
	primeorder@0.13.6
	proc-macro2@1.0.107
	ptr_meta@0.3.1
	ptr_meta_derive@0.3.1
	pxfm@0.1.30
	quick-xml@0.39.4
	quote@1.0.47
	r-efi@5.3.0
	r-efi@6.0.0
	rancor@0.1.2
	rand@0.8.7
	rand@0.9.5
	rand_chacha@0.3.1
	rand_chacha@0.9.0
	rand_core@0.6.4
	rand_core@0.9.5
	rayon-core@1.13.0
	rayon@1.12.0
	rcgen@0.13.2
	redox_syscall@0.5.18
	regex-automata@0.4.16
	regex-syntax@0.8.11
	regex@1.13.1
	rend@0.5.4
	rfc6979@0.4.0
	rgb@0.8.53
	ring@0.17.14
	rkyv@0.8.17
	rkyv_derive@0.8.17
	rtcp@0.17.2
	rtp@0.17.2
	rustc_version@0.4.1
	rusticata-macros@4.1.0
	rustix@1.1.4
	rustls-pemfile@2.2.0
	rustls-pki-types@1.15.0
	rustls-webpki@0.103.13
	rustls@0.23.42
	rustversion@1.0.23
	scopeguard@1.2.0
	sdp@0.17.2
	sec1@0.7.3
	semver@1.0.28
	serde@1.0.229
	serde_core@1.0.229
	serde_derive@1.0.229
	serde_json@1.0.151
	sha1@0.10.7
	sha2@0.10.9
	shlex@2.0.1
	signal-hook-registry@1.4.8
	signature@2.2.0
	simdutf8@0.1.5
	slab@0.4.12
	smallvec@1.15.2
	smol_str@0.2.2
	socket2@0.5.10
	socket2@0.6.5
	spki@0.7.3
	stable_deref_trait@1.2.1
	stun@0.17.2
	substring@1.4.5
	subtle@2.6.1
	syn@2.0.119
	syn@3.0.2
	synstructure@0.13.2
	thiserror-impl@1.0.69
	thiserror-impl@2.0.19
	thiserror@1.0.69
	thiserror@2.0.19
	time-core@0.1.9
	time-macros@0.2.32
	time@0.3.54
	tinystr@0.8.3
	tinyvec@1.12.0
	tinyvec_macros@0.1.1
	tokio-macros@2.7.1
	tokio-rustls@0.26.4
	tokio-tungstenite@0.24.0
	tokio-util@0.7.19
	tokio@1.53.1
	tree_magic_mini@3.2.2
	tungstenite@0.24.0
	turn@0.17.2
	typenum@1.20.1
	unicase@2.9.0
	unicode-ident@1.0.24
	universal-hash@0.5.1
	untrusted@0.9.0
	url@2.5.8
	utf-8@0.7.6
	utf8_iter@1.0.4
	utf8parse@0.2.2
	uuid@1.24.0
	version_check@0.9.5
	waitgroup@0.1.2
	wasi@0.11.1+wasi-snapshot-preview1
	wasip2@1.0.4+wasi-0.2.12
	wasm-bindgen-macro-support@0.2.126
	wasm-bindgen-macro@0.2.126
	wasm-bindgen-shared@0.2.126
	wasm-bindgen@0.2.126
	wayland-backend@0.3.15
	wayland-client@0.31.14
	wayland-protocols-misc@0.3.12
	wayland-protocols-wlr@0.3.12
	wayland-protocols@0.32.13
	wayland-scanner@0.31.10
	wayland-sys@0.31.11
	webp-rust@0.2.1
	webp@0.3.1
	webpx@0.4.0
	webrtc-data@0.17.2
	webrtc-ice@0.17.2
	webrtc-mdns@0.17.2
	webrtc-media@0.17.2
	webrtc-sctp@0.17.2
	webrtc-srtp@0.17.2
	webrtc-util@0.17.2
	webrtc@0.17.2
	whereat@0.1.5
	winapi-i686-pc-windows-gnu@0.4.0
	winapi-x86_64-pc-windows-gnu@0.4.0
	winapi@0.3.9
	windows-link@0.2.1
	windows-sys@0.52.0
	windows-sys@0.61.2
	windows-targets@0.52.6
	windows_aarch64_gnullvm@0.52.6
	windows_aarch64_msvc@0.52.6
	windows_i686_gnu@0.52.6
	windows_i686_gnullvm@0.52.6
	windows_i686_msvc@0.52.6
	windows_x86_64_gnu@0.52.6
	windows_x86_64_gnullvm@0.52.6
	windows_x86_64_msvc@0.52.6
	wit-bindgen@0.57.1
	wl-clipboard-rs@0.9.3
	writeable@0.6.3
	x25519-dalek@2.0.1
	x509-parser@0.16.0
	xxhash-rust@0.8.18
	yasna@0.5.2
	yoke-derive@0.8.2
	yoke@0.8.3
	zerocopy-derive@0.8.55
	zerocopy@0.8.55
	zerofrom-derive@0.1.7
	zerofrom@0.1.8
	zeroize@1.9.0
	zeroize_derive@1.5.0
	zerotrie@0.2.4
	zerovec-derive@0.11.3
	zerovec@0.11.6
	zmij@1.0.23
"

RUST_MIN_VER="1.89"

inherit cargo desktop optfeature xdg

DESCRIPTION="Wayland screen streamer: tile-diffed WebP over WebRTC to any browser"
HOMEPAGE="https://github.com/tarilka0gg/Ring-2Zero"
SRC_URI="
	https://github.com/tarilka0gg/Ring-2Zero/archive/refs/tags/v${PV}.tar.gz -> ${P}.tar.gz
	${CARGO_CRATE_URIS}
"
S="${WORKDIR}/Ring-2Zero-${PV}"

LICENSE="MIT"
# Dependent crate licenses (generated by packaging/gentoo/gen-ebuild-data.py)
LICENSE+=" Apache-2.0 Apache-2.0-with-LLVM-exceptions BSD BSD-1 BSD-2 Boost-1.0 ISC LGPL-2.1+ MIT Unicode-3.0 Unlicense ZLIB openssl"
SLOT="0"
KEYWORDS="~amd64"
IUSE="pipewire"

RDEPEND="
	dev-libs/wayland
	media-libs/mesa
	x11-libs/libdrm
	pipewire? (
		media-video/pipewire:=
		sys-apps/dbus
	)
"
DEPEND="${RDEPEND}"
# .cargo/config.toml links with clang
BDEPEND="
	llvm-core/clang
	virtual/pkgconfig
"

QA_FLAGS_IGNORED="usr/bin/ring-2zero"

pkg_setup() {
	rust_pkg_setup
}

src_configure() {
	local myfeatures=( $(usev pipewire pipewire_capture) )
	cargo_src_configure
}

src_compile() {
	cargo_src_compile --bin ring-2zero
}

src_test() {
	cargo_src_test --lib --bins --
}

src_install() {
	cargo_src_install --bin ring-2zero
	dosym ring-2zero /usr/bin/r2zr

	doman man/ring-2zero.1
	domenu packaging/io.github.tarilka0gg.Ring2Zero.desktop
	insinto /usr/share/metainfo
	doins packaging/io.github.tarilka0gg.Ring2Zero.metainfo.xml
	insinto /usr/share/icons/hicolor/scalable/apps
	doins packaging/icons/ring-2zero.svg
	dodoc CHANGELOG.md README.md
}

pkg_postinst() {
	xdg_pkg_postinst
	optfeature "TURN/STUN servers for streaming across NAT" net-im/coturn
}

pkg_postrm() {
	xdg_pkg_postrm
}
