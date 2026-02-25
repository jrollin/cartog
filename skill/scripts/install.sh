#!/usr/bin/env bash
set -euo pipefail

# Install cartog binary
# 1. Try downloading pre-built binary from GitHub Releases
# 2. Fallback to cargo install

REPO="jrollin/cartog"

if command -v cartog &>/dev/null; then
    echo "cartog is already installed: $(cartog --version)"
    exit 0
fi

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
    local target="$1"
    local latest_tag

    latest_tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')"
    if [ -z "$latest_tag" ]; then
        return 1
    fi

    local url="https://github.com/${REPO}/releases/download/${latest_tag}/cartog-${target}.tar.gz"
    local install_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
    mkdir -p "$install_dir"

    echo "Downloading cartog ${latest_tag} for ${target}..."
    if curl -fsSL "$url" | tar xz -C "$install_dir"; then
        chmod +x "${install_dir}/cartog"
        echo "cartog installed to ${install_dir}/cartog"
        return 0
    fi

    return 1
}

# Try pre-built binary first
if target="$(detect_target)"; then
    if install_from_github "$target"; then
        exit 0
    fi
    echo "Pre-built binary not available, falling back to cargo install..."
fi

# Fallback to cargo install
if command -v cargo &>/dev/null; then
    echo "Installing cartog via cargo..."
    cargo install cartog
    echo "cartog installed successfully."
else
    echo "Error: could not download pre-built binary and cargo not found."
    echo "Install Rust from https://rustup.rs/ then run:"
    echo "  cargo install cartog"
    exit 1
fi
