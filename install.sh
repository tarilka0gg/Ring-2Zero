#!/usr/bin/env bash
# ring-2zero installer: detects/installs system dependencies, finds a
# working C compiler, builds and installs the binary, and adds a shell
# alias (r2zr) — for whatever shell you're actually running, not just fish.
#
# Usage:
#   ./install.sh                 interactive (asks before installing packages)
#   ./install.sh -y               non-interactive (auto-confirm package installs)
#   ./install.sh --pipewire       also build with --features pipewire_capture
#   ./install.sh --no-alias       skip the shell alias step
#   ./install.sh --dry-run        show what would happen, change nothing
#
# Every failure prints what went wrong and, where possible, exactly what to
# run manually to fix it — this script is meant to be safe to re-run.

set -uo pipefail

# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------

if [ -t 1 ]; then
    BOLD=$'\033[1m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
else
    BOLD=''; RED=''; GREEN=''; YELLOW=''; CYAN=''; RESET=''
fi

step()  { printf '\n%s==>%s %s\n' "$BOLD$CYAN" "$RESET$BOLD" "$*$RESET"; }
ok()    { printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn()  { printf '  %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }
info()  { printf '  %s\n' "$*"; }

# fail() prints an error with an optional "how to fix it" hint and exits.
# Centralizing this is the point: every exit path explains itself instead of
# leaving a bare non-zero status or a raw command trace.
fail() {
    printf '\n%s✗ ERROR:%s %s\n' "$RED$BOLD" "$RESET" "$1" >&2
    if [ -n "${2:-}" ]; then
        printf '\n  %s\n' "$2" >&2
    fi
    exit 1
}

trap 'fail "Unexpected failure at ${BASH_SOURCE[0]}:${LINENO} (command: ${BASH_COMMAND})" \
    "This is likely a bug in install.sh itself, not your system — please report it with this output attached."' ERR

DRY_RUN=0
ASSUME_YES=0
WITH_PIPEWIRE=0
WITH_ALIAS=1

for arg in "$@"; do
    case "$arg" in
        -y|--yes) ASSUME_YES=1 ;;
        --pipewire) WITH_PIPEWIRE=1 ;;
        --no-alias) WITH_ALIAS=0 ;;
        --dry-run) DRY_RUN=1 ;;
        -h|--help)
            sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) fail "Unknown option: $arg" "Run '$0 --help' for usage." ;;
    esac
done

run() {
    if [ "$DRY_RUN" = 1 ]; then
        printf '  %s[dry-run]%s %s\n' "$YELLOW" "$RESET" "$*"
        return 0
    fi
    "$@"
}

confirm() {
    local prompt="$1"
    [ "$ASSUME_YES" = 1 ] && return 0
    [ "$DRY_RUN" = 1 ] && return 0
    printf '  %s [Y/n] ' "$prompt"
    read -r reply
    case "$reply" in
        [nN]*) return 1 ;;
        *) return 0 ;;
    esac
}

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_DIR" || fail "Can't cd into $REPO_DIR"

printf '%sring-2zero installer%s\n' "$BOLD" "$RESET"

# ---------------------------------------------------------------------------
# 1. Detect package manager + privilege escalation tool
# ---------------------------------------------------------------------------

step "Detecting package manager"

PKG_MANAGER=""
for pm in emerge apt-get pacman dnf zypper; do
    if command -v "$pm" >/dev/null 2>&1; then
        case "$pm" in
            emerge) PKG_MANAGER="portage" ;;
            apt-get) PKG_MANAGER="apt" ;;
            *) PKG_MANAGER="$pm" ;;
        esac
        break
    fi
done

if [ -z "$PKG_MANAGER" ]; then
    warn "Couldn't detect emerge/apt-get/pacman/dnf/zypper — automatic dependency install is unavailable."
    warn "You'll need to install missing libraries manually; this script will still tell you what's missing."
else
    ok "Detected package manager: $PKG_MANAGER"
fi

AS_ROOT=""
if [ "$(id -u)" -ne 0 ]; then
    if command -v doas >/dev/null 2>&1; then
        AS_ROOT="doas"
    elif command -v sudo >/dev/null 2>&1; then
        AS_ROOT="sudo"
    else
        warn "Not root, and neither doas nor sudo found — can't install packages automatically."
    fi
fi

# Maps a pkg-config module name to this distro's package name. Empty output
# means "don't know the package name for this manager" (script still says
# what pkg-config module was missing so you can look it up).
pkg_name_for() {
    local module="$1"
    case "$PKG_MANAGER:$module" in
        portage:wayland-client)   echo "dev-libs/wayland" ;;
        portage:gbm)              echo "media-libs/mesa" ;;
        portage:libdrm)           echo "x11-libs/libdrm" ;;
        portage:libpipewire-0.3)  echo "media-video/pipewire" ;;
        portage:dbus-1)           echo "sys-apps/dbus" ;;
        portage:pkg-config)       echo "dev-util/pkgconf" ;;
        portage:clang)            echo "sys-devel/clang" ;;

        apt:wayland-client)   echo "libwayland-dev" ;;
        apt:gbm)              echo "libgbm-dev" ;;
        apt:libdrm)           echo "libdrm-dev" ;;
        apt:libpipewire-0.3)  echo "libpipewire-0.3-dev" ;;
        apt:dbus-1)           echo "libdbus-1-dev" ;;
        apt:pkg-config)       echo "pkg-config" ;;
        apt:clang)            echo "clang" ;;

        pacman:wayland-client)   echo "wayland" ;;
        pacman:gbm)              echo "mesa" ;;
        pacman:libdrm)           echo "libdrm" ;;
        pacman:libpipewire-0.3)  echo "libpipewire" ;;
        pacman:dbus-1)           echo "dbus" ;;
        pacman:pkg-config)       echo "pkgconf" ;;
        pacman:clang)            echo "clang" ;;

        dnf:wayland-client)   echo "wayland-devel" ;;
        dnf:gbm)              echo "mesa-libgbm-devel" ;;
        dnf:libdrm)           echo "libdrm-devel" ;;
        dnf:libpipewire-0.3)  echo "pipewire-devel" ;;
        dnf:dbus-1)           echo "dbus-devel" ;;
        dnf:pkg-config)       echo "pkgconf-pkg-config" ;;
        dnf:clang)            echo "clang" ;;

        zypper:wayland-client)   echo "wayland-devel" ;;
        zypper:gbm)              echo "Mesa-libgbm-devel" ;;
        zypper:libdrm)           echo "libdrm-devel" ;;
        zypper:libpipewire-0.3)  echo "pipewire-devel" ;;
        zypper:dbus-1)           echo "dbus-1-devel" ;;
        zypper:pkg-config)       echo "pkg-config" ;;
        zypper:clang)            echo "clang" ;;

        *) echo "" ;;
    esac
}

install_packages() {
    # $@ = distro package names to install
    [ "$#" -eq 0 ] && return 0
    [ -z "$PKG_MANAGER" ] && fail "No package manager detected — install these manually: $*"
    [ -z "$AS_ROOT" ] && [ "$(id -u)" -ne 0 ] && \
        fail "Need root to install packages, but no doas/sudo available." "Install manually: $*"

    local cmd
    case "$PKG_MANAGER" in
        portage) cmd=(emerge --ask=n --noreplace "$@") ;;
        apt)     cmd=(apt-get install -y "$@") ;;
        pacman)  cmd=(pacman -S --needed --noconfirm "$@") ;;
        dnf)     cmd=(dnf install -y "$@") ;;
        zypper)  cmd=(zypper install -y "$@") ;;
    esac

    info "Will run: ${AS_ROOT:+$AS_ROOT }${cmd[*]}"
    confirm "Install these packages now?" || fail "Aborted by user." "Install manually then re-run: ${AS_ROOT:+$AS_ROOT }${cmd[*]}"

    if [ -n "$AS_ROOT" ]; then
        run "$AS_ROOT" "${cmd[@]}"
    else
        run "${cmd[@]}"
    fi || fail "Package install command failed (see output above)." \
              "Try running it manually to see the full package-manager error: ${AS_ROOT:+$AS_ROOT }${cmd[*]}"
}

# ---------------------------------------------------------------------------
# 2. pkg-config itself (needed to check everything else)
# ---------------------------------------------------------------------------

step "Checking for pkg-config"

if ! command -v pkg-config >/dev/null 2>&1; then
    warn "pkg-config not found"
    pkg="$(pkg_name_for pkg-config)"
    if [ -n "$pkg" ]; then
        install_packages "$pkg"
    else
        fail "pkg-config missing and no known package name for '$PKG_MANAGER'." \
             "Install pkg-config (or pkgconf) using your distro's package manager, then re-run this script."
    fi
    command -v pkg-config >/dev/null 2>&1 || fail "pkg-config still not found after install — something went wrong."
fi
ok "pkg-config found: $(command -v pkg-config)"

# ---------------------------------------------------------------------------
# 3. Required (and optional pipewire) system libraries
# ---------------------------------------------------------------------------

step "Checking system libraries"

REQUIRED_LIBS=(wayland-client gbm libdrm)
OPTIONAL_LIBS=()
[ "$WITH_PIPEWIRE" = 1 ] && OPTIONAL_LIBS=(libpipewire-0.3 dbus-1)

missing_pkgs=()
missing_modules=()

check_lib() {
    local module="$1"
    if pkg-config --exists "$module" 2>/dev/null; then
        ok "$module found ($(pkg-config --modversion "$module" 2>/dev/null))"
        return 0
    fi
    warn "$module missing"
    local pkg
    pkg="$(pkg_name_for "$module")"
    if [ -n "$pkg" ]; then
        missing_pkgs+=("$pkg")
    else
        missing_modules+=("$module")
    fi
    return 1
}

for lib in "${REQUIRED_LIBS[@]}"; do check_lib "$lib" || true; done
for lib in "${OPTIONAL_LIBS[@]}"; do check_lib "$lib" || true; done

if [ "${#missing_modules[@]}" -gt 0 ]; then
    fail "Missing libraries with no known package name for '$PKG_MANAGER': ${missing_modules[*]}" \
         "Find and install the -dev/-devel package that provides these pkg-config modules, then re-run."
fi

if [ "${#missing_pkgs[@]}" -gt 0 ]; then
    install_packages "${missing_pkgs[@]}"
    for lib in "${REQUIRED_LIBS[@]}" "${OPTIONAL_LIBS[@]}"; do
        pkg-config --exists "$lib" 2>/dev/null || fail "$lib still missing after install." \
            "The package manager reported success but pkg-config still can't find '$lib' — check the package actually provides a .pc file for it."
    done
fi

# ---------------------------------------------------------------------------
# 4. Find a C compiler (this project's .cargo/config.toml pins linker=clang)
# ---------------------------------------------------------------------------

step "Looking for clang"

CLANG_BIN=""
if command -v clang >/dev/null 2>&1; then
    CLANG_BIN="$(command -v clang)"
else
    # Gentoo (and some other distros) keep versioned clang under slotted
    # paths instead of a bare `clang` on PATH — check the highest version.
    for d in $(ls -d /usr/lib/llvm/*/bin 2>/dev/null | sort -t/ -k4 -Vr); do
        if [ -x "$d/clang" ]; then
            CLANG_BIN="$d/clang"
            break
        fi
    done
fi

if [ -z "$CLANG_BIN" ]; then
    warn "clang not found anywhere on PATH or in /usr/lib/llvm/*/bin"
    pkg="$(pkg_name_for clang)"
    if [ -n "$pkg" ]; then
        install_packages "$pkg"
        CLANG_BIN="$(command -v clang || true)"
        if [ -z "$CLANG_BIN" ]; then
            for d in $(ls -d /usr/lib/llvm/*/bin 2>/dev/null | sort -t/ -k4 -Vr); do
                [ -x "$d/clang" ] && { CLANG_BIN="$d/clang"; break; }
            done
        fi
    fi
    [ -z "$CLANG_BIN" ] && fail "clang still not found after install attempt." \
        "Install clang manually for your distro, then re-run this script."
fi
ok "Using clang: $CLANG_BIN"
CLANG_DIR="$(dirname "$CLANG_BIN")"

# ---------------------------------------------------------------------------
# 5. Find (or install) Rust
# ---------------------------------------------------------------------------

step "Looking for cargo"

if ! command -v cargo >/dev/null 2>&1; then
    warn "cargo not found"
    if confirm "Install Rust via rustup (https://rustup.rs) now?"; then
        run bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable' \
            || fail "rustup install failed (see output above)."
        # shellcheck disable=SC1091
        [ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
    else
        fail "cargo is required to build ring-2zero." \
             "Install Rust yourself (rustup, or your distro's rust/cargo package), then re-run this script."
    fi
fi
command -v cargo >/dev/null 2>&1 || fail "cargo still not found after install attempt." \
    "Open a new shell (so PATH picks up ~/.cargo/bin) and re-run this script."
ok "cargo found: $(command -v cargo) ($(cargo --version))"

# ---------------------------------------------------------------------------
# 6. Build + install the binary
# ---------------------------------------------------------------------------

step "Building ring-2zero (release)"

FEATURES=""
[ "$WITH_PIPEWIRE" = 1 ] && FEATURES="pipewire_capture"

BUILD_LOG="$(mktemp)"
BUILD_ENV=(CC="$CLANG_BIN" CXX="${CLANG_BIN}++" PATH="$CLANG_DIR:$PATH")
BUILD_CMD=(cargo install --path . --force)
[ -n "$FEATURES" ] && BUILD_CMD+=(--features "$FEATURES")

info "Running: ${BUILD_ENV[*]} ${BUILD_CMD[*]}"
if [ "$DRY_RUN" = 1 ]; then
    printf '  %s[dry-run]%s skipped\n' "$YELLOW" "$RESET"
else
    if env "${BUILD_ENV[@]}" "${BUILD_CMD[@]}" 2>&1 | tee "$BUILD_LOG"; then
        ok "Build + install succeeded"
    else
        tail -n 40 "$BUILD_LOG" >&2
        fail "cargo build failed — full output above (last 40 lines)." \
             "Common causes: a missing -dev header this script doesn't check for, or a linker mismatch. Re-run with the same CC/CXX/PATH shown above to reproduce, or open an issue with the output attached."
    fi
    rm -f "$BUILD_LOG"
fi

BIN_PATH="$(command -v ring-2zero || true)"
if [ -z "$BIN_PATH" ]; then
    # cargo install succeeded but the install dir isn't on PATH yet this session
    for c in "$HOME/.cargo/bin/ring-2zero" "$CARGO_HOME/bin/ring-2zero"; do
        [ -x "$c" ] && { BIN_PATH="$c"; break; }
    done
fi
[ -n "$BIN_PATH" ] && ok "Installed: $BIN_PATH"

# ---------------------------------------------------------------------------
# 7. Shell alias
# ---------------------------------------------------------------------------

if [ "$WITH_ALIAS" = 1 ]; then
    step "Adding 'r2zr' shell alias"

    # $SHELL is your registered *login* shell and is frequently stale (set
    # once at login, doesn't update if you exec into a different shell
    # interactively) — the shell that actually matters here is whichever one
    # invoked this script, i.e. our parent process.
    ppid_comm=""
    if [ -r "/proc/$PPID/comm" ]; then
        ppid_comm="$(cat "/proc/$PPID/comm" 2>/dev/null)"
    elif command -v ps >/dev/null 2>&1; then
        ppid_comm="$(ps -o comm= -p "$PPID" 2>/dev/null)"
    fi
    case "$ppid_comm" in
        fish|zsh|bash) target_shell="$ppid_comm" ;;
        *) target_shell="$(basename "${SHELL:-bash}")" ;;
    esac

    rc_file=""
    alias_line=""
    case "$target_shell" in
        fish)
            rc_file="$HOME/.config/fish/config.fish"
            alias_line="abbr -a r2zr ring-2zero"
            ;;
        zsh)
            rc_file="$HOME/.zshrc"
            alias_line="alias r2zr='ring-2zero'"
            ;;
        bash)
            rc_file="$HOME/.bashrc"
            alias_line="alias r2zr='ring-2zero'"
            ;;
        *)
            warn "Unrecognized \$SHELL ('$target_shell') — falling back to ~/.profile with a plain POSIX alias."
            warn "If your shell doesn't source ~/.profile for interactive sessions, add this yourself: alias r2zr='ring-2zero'"
            rc_file="$HOME/.profile"
            alias_line="alias r2zr='ring-2zero'"
            ;;
    esac

    if [ "$DRY_RUN" = 1 ]; then
        info "[dry-run] would add to $rc_file: $alias_line"
    else
        mkdir -p "$(dirname "$rc_file")"
        touch "$rc_file"
        if grep -qF "$alias_line" "$rc_file" 2>/dev/null; then
            ok "Already present in $rc_file"
        else
            printf '\n# ring-2zero shortcut\n%s\n' "$alias_line" >> "$rc_file"
            ok "Added to $rc_file — open a new shell (or re-source it) to use it"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------

step "Done"
info "Run it with: ${BOLD}${BIN_PATH:-ring-2zero}${RESET}"
[ "$WITH_ALIAS" = 1 ] && info "...or, in a new shell: ${BOLD}r2zr${RESET}"
info "Then open http://<this-host>:9001 in a browser. ${BOLD}--help${RESET} for the full flag/env var reference."
