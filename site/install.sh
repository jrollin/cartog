#!/usr/bin/env sh
# cartog installer
#
# Usage:
#   curl -fsSL https://jrollin.github.io/cartog/install.sh | sh
#
# Detects OS + architecture, downloads the matching release tarball from
# GitHub, verifies its SHA-256 against SHA256SUMS, and installs `cartog`
# into the first writable directory of $CARTOG_INSTALL_DIR, /usr/local/bin
# (with sudo if needed), or ~/.local/bin (PATH not modified — message
# printed if it isn't already on PATH).
#
# Environment overrides:
#   CARTOG_VERSION       — install a specific version (default: latest)
#   CARTOG_INSTALL_DIR   — target directory (default: see above)
#   CARTOG_NO_VERIFY     — skip SHA-256 verification (NOT recommended)

set -eu

REPO="jrollin/cartog"
BIN="cartog"

# ── helpers ──────────────────────────────────────────────────────────
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# ── platform detection ───────────────────────────────────────────────
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)

    case "$os" in
        Darwin)
            case "$arch" in
                arm64|aarch64) printf 'aarch64-apple-darwin\n' ;;
                x86_64)        printf 'x86_64-apple-darwin\n' ;;
                *) die "unsupported macOS architecture: $arch" ;;
            esac
            ;;
        Linux)
            case "$arch" in
                aarch64|arm64) printf 'aarch64-unknown-linux-gnu\n' ;;
                x86_64|amd64)  printf 'x86_64-unknown-linux-gnu\n' ;;
                *) die "unsupported Linux architecture: $arch" ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            die "Windows is not supported by this script. Download the .zip from https://github.com/$REPO/releases/latest"
            ;;
        *)
            die "unsupported operating system: $os"
            ;;
    esac
}

# ── resolve install dir ──────────────────────────────────────────────
resolve_install_dir() {
    if [ -n "${CARTOG_INSTALL_DIR:-}" ]; then
        printf '%s\n' "$CARTOG_INSTALL_DIR"
        return
    fi
    # /usr/local/bin is the conventional system path; fall back to a
    # user-local dir if we can't write there (and avoid prompting for
    # sudo when running non-interactively).
    if [ -w /usr/local/bin ] 2>/dev/null; then
        printf '/usr/local/bin\n'
    elif [ -t 1 ] && have sudo; then
        printf '/usr/local/bin\n'
    else
        printf '%s/.local/bin\n' "$HOME"
    fi
}

# ── download helpers ─────────────────────────────────────────────────
fetch() {
    # $1 = url, $2 = dest path
    if have curl; then
        curl --fail --silent --show-error --location --output "$2" "$1"
    elif have wget; then
        wget --quiet --output-document "$2" "$1"
    else
        die "neither curl nor wget found on PATH"
    fi
}

verify_sha256() {
    # $1 = file, $2 = expected hash
    actual=""
    if have sha256sum; then
        actual=$(sha256sum "$1" | awk '{print $1}')
    elif have shasum; then
        actual=$(shasum -a 256 "$1" | awk '{print $1}')
    elif have openssl; then
        actual=$(openssl dgst -sha256 "$1" | awk '{print $NF}')
    else
        die "no sha256 verifier found (need sha256sum, shasum, or openssl). Set CARTOG_NO_VERIFY=1 to bypass (NOT recommended)."
    fi
    if [ "$actual" != "$2" ]; then
        die "SHA-256 mismatch for $1: expected $2, got $actual"
    fi
}

# ── main ─────────────────────────────────────────────────────────────
main() {
    target=$(detect_target)
    version=${CARTOG_VERSION:-latest}

    if [ "$version" = "latest" ]; then
        base="https://github.com/$REPO/releases/latest/download"
    else
        # Strip leading 'v' if user passed one; the release tag is v<version>.
        v=${version#v}
        base="https://github.com/$REPO/releases/download/v$v"
    fi

    archive="$BIN-$target.tar.gz"
    archive_url="$base/$archive"
    sha_url="$base/SHA256SUMS"

    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t cartog-install)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    info "platform: $target"
    info "downloading $archive_url"
    fetch "$archive_url" "$tmp/$archive"

    if [ -z "${CARTOG_NO_VERIFY:-}" ]; then
        info "verifying SHA-256"
        fetch "$sha_url" "$tmp/SHA256SUMS"
        expected=$(awk -v f="$archive" '$2 == f || $2 == "*"f {print $1}' "$tmp/SHA256SUMS")
        if [ -z "$expected" ]; then
            die "no SHA-256 entry for $archive in SHA256SUMS"
        fi
        verify_sha256 "$tmp/$archive" "$expected"
    else
        info "skipping SHA-256 verification (CARTOG_NO_VERIFY is set)"
    fi

    info "extracting"
    ( cd "$tmp" && tar xzf "$archive" )
    [ -f "$tmp/$BIN" ] || die "extracted archive does not contain $BIN"

    install_dir=$(resolve_install_dir)
    # Try an unprivileged mkdir first. If that fails and the directory is still
    # absent, fall back to sudo (sudo install does NOT create parent dirs).
    mkdir -p "$install_dir" 2>/dev/null || true
    if [ ! -d "$install_dir" ]; then
        if have sudo; then
            sudo mkdir -p "$install_dir" \
                || die "could not create $install_dir even with sudo."
        else
            die "cannot create $install_dir and sudo is not available. Set CARTOG_INSTALL_DIR to a writable directory."
        fi
    fi

    if [ -w "$install_dir" ]; then
        install -m 755 "$tmp/$BIN" "$install_dir/$BIN"
    elif have sudo; then
        info "installing into $install_dir (sudo required)"
        sudo install -m 755 "$tmp/$BIN" "$install_dir/$BIN"
    else
        die "cannot write to $install_dir and sudo is not available. Set CARTOG_INSTALL_DIR to a writable directory."
    fi

    info "installed $BIN to $install_dir/$BIN"

    # PATH advisory: only warn if the chosen dir isn't already on PATH.
    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *)
            printf '\n'
            printf 'note: %s is not on your PATH.\n' "$install_dir"
            printf '      add this to your shell rc:\n\n'
            printf '          export PATH="%s:$PATH"\n\n' "$install_dir"
            ;;
    esac

    # Post-install handshake: print version and the recommended next step.
    "$install_dir/$BIN" --version || true
    printf '\nNext: cd into a repo, then run `cartog init` and `cartog index`.\n'
    printf '      Optional — `cartog ide` wires MCP into your editor.\n'
}

main "$@"
