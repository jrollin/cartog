#!/usr/bin/env bash
set -euo pipefail

# Install cartog binary
# 1. Try downloading pre-built binary from GitHub Releases (requires curl + tar)
# 2. Fallback to cargo install (requires Rust 1.70+)

REPO="jrollin/cartog"
MIN_RUST_MAJOR=1
MIN_RUST_MINOR=70
REQUESTED_VERSION="${1:-}"

if command -v cartog &>/dev/null; then
    local_version="$(cartog --version | sed -E 's/^cartog //')"
    if [ -z "$REQUESTED_VERSION" ]; then
        echo "cartog is already installed: cartog $local_version"
        exit 0
    fi
    if [ "$local_version" = "$REQUESTED_VERSION" ]; then
        echo "cartog $REQUESTED_VERSION already installed."
        exit 0
    fi
    echo "Upgrading cartog from $local_version to $REQUESTED_VERSION..."
fi

has_cmd() { command -v "$1" &>/dev/null; }

check_rust_version() {
    if ! has_cmd rustc; then
        return 1
    fi
    local version
    version="$(rustc --version | sed -E 's/rustc ([0-9]+\.[0-9]+).*/\1/')"
    local major minor
    major="${version%%.*}"
    minor="${version##*.}"
    if [ "$major" -gt "$MIN_RUST_MAJOR" ] || { [ "$major" -eq "$MIN_RUST_MAJOR" ] && [ "$minor" -ge "$MIN_RUST_MINOR" ]; }; then
        return 0
    fi
    echo "Warning: Rust $version found, but cartog requires >= $MIN_RUST_MAJOR.$MIN_RUST_MINOR"
    return 1
}

detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            case "$arch" in
                x86_64)  echo "x86_64-unknown-linux-gnu" ;;
                aarch64) echo "aarch64-unknown-linux-gnu" ;;
                *)       return 1 ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64)  echo "x86_64-apple-darwin" ;;
                arm64)   echo "aarch64-apple-darwin" ;;
                *)       return 1 ;;
            esac
            ;;
        *)
            return 1
            ;;
    esac
}

install_from_github() {
    if ! has_cmd curl; then
        echo "curl not found, skipping binary download."
        return 1
    fi
    if ! has_cmd tar; then
        echo "tar not found, skipping binary download."
        return 1
    fi

    local target="$1"
    local tag

    if [ -n "$REQUESTED_VERSION" ]; then
        tag="v${REQUESTED_VERSION}"
    else
        tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')"
        if [ -z "$tag" ]; then
            return 1
        fi
    fi

    local url="https://github.com/${REPO}/releases/download/${tag}/cartog-${target}.tar.gz"
    local install_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
    mkdir -p "$install_dir"

    echo "Downloading cartog ${tag} for ${target}..."
    if curl -fsSL "$url" | tar xz -C "$install_dir"; then
        chmod +x "${install_dir}/cartog"
        echo "cartog installed to ${install_dir}/cartog"
        return 0
    fi

    return 1
}

verify_install() {
    local bin="${CARGO_HOME:-$HOME/.cargo}/bin/cartog"
    local target_bin=""

    if has_cmd cartog; then
        target_bin="cartog"
    elif [ -x "$bin" ]; then
        target_bin="$bin"
    else
        echo "Error: cartog binary not found after install."
        return 1
    fi

    local version_output
    if version_output=$("$target_bin" --version 2>&1); then
        echo "Verified: $version_output"
        if [ "$target_bin" = "$bin" ]; then
            echo "Note: ${CARGO_HOME:-$HOME/.cargo}/bin is not in your PATH."
            echo "  Add it with: export PATH=\"\${CARGO_HOME:-\$HOME/.cargo}/bin:\$PATH\""
        fi
        return 0
    fi

    echo "Error: cartog binary exists but failed to run (wrong architecture?)."
    echo "  Output: $version_output"
    rm -f "$bin"
    return 1
}

# Try pre-built binary first
if target="$(detect_target)"; then
    if install_from_github "$target"; then
        verify_install
        exit 0
    fi
    echo "Pre-built binary not available, falling back to cargo install..."
fi

# Fallback to cargo install
if ! has_cmd cargo; then
    echo "Error: could not download pre-built binary and cargo not found."
    echo "Install Rust from https://rustup.rs/ then run:"
    echo "  cargo install cartog"
    exit 1
fi

if ! check_rust_version; then
    echo "Error: Rust toolchain too old. Update with: rustup update"
    exit 1
fi

echo "Installing cartog via cargo..."
if [ -n "$REQUESTED_VERSION" ]; then
    cargo install "cartog@${REQUESTED_VERSION}"
else
    cargo install cartog
fi
verify_install
