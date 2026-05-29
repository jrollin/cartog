#!/usr/bin/env bash
set -euo pipefail

# Install cartog binary by downloading a pre-built release tarball from GitHub
# Releases (requires curl + tar). Release-only: there is no cargo-install
# fallback — if no matching pre-built binary is available, the script fails
# with a clear message rather than silently building from source.

REPO="jrollin/cartog"
REQUESTED_VERSION="${1:-}"

# Marker hands the chosen install dir from the writer to verify_install.
# Clear in case PID reuse left a stale file from a crashed run.
INSTALL_DIR_MARKER="${TMPDIR:-/tmp}/cartog-install-dir.$$"
rm -f "$INSTALL_DIR_MARKER"

if command -v cartog &>/dev/null; then
    # `cartog --version` prints multiple lines on >=0.14: version, build SHA,
    # features, rustc. Restrict to the first line and strip the build suffix.
    # Tolerate non-zero exit from cartog --version (e.g. broken binary, wrong
    # arch): treat as "version unknown" so the rest of the script can decide
    # whether to install fresh. The trailing `|| true` shields the parse
    # pipeline from `set -e` / `set -o pipefail`.
    local_version="$( { cartog --version 2>/dev/null || true; } | head -n1 | sed -E 's/^cartog //' | awk '{print $1}')"
    if [ -z "$local_version" ]; then
        # Don't silently re-download or print "Upgrading from  to X". Surface
        # the parser breakage so a future banner-format change is visible.
        echo "Warning: cartog is on PATH but --version did not yield a parseable version string." >&2
        echo "Proceeding with a fresh install (target: ${REQUESTED_VERSION:-latest})." >&2
    else
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
fi

has_cmd() { command -v "$1" &>/dev/null; }

# Pick the install directory. Preference order:
#   1. $CARTOG_INSTALL_DIR — explicit override
#   2. Directory of an existing cartog on PATH — upgrades in place, no duplicates
#   3. ~/.local/bin if present — XDG-style user bin, on PATH by default
#   4. $CARGO_HOME/bin (or ~/.cargo/bin) — last-resort fallback
pick_install_dir() {
    if [ -n "${CARTOG_INSTALL_DIR:-}" ]; then
        printf '%s\n' "$CARTOG_INSTALL_DIR"
        return
    fi
    local existing
    # Require an absolute path — `command -v` returns the function/alias name
    # when cartog is shadowed by one, and dirname of that is ".".
    if existing="$(command -v cartog 2>/dev/null)" && [ -n "$existing" ] && [ "${existing#/}" != "$existing" ]; then
        local resolved
        resolved="$(cd "$(dirname "$existing")" 2>/dev/null && pwd)" || resolved=""
        if [ -n "$resolved" ]; then
            printf '%s\n' "$resolved"
            return
        fi
    fi
    if [ -d "$HOME/.local/bin" ]; then
        printf '%s\n' "$HOME/.local/bin"
        return
    fi
    printf '%s\n' "${CARGO_HOME:-$HOME/.cargo}/bin"
}

# Component-wise PATH match; trailing slashes normalised on both sides.
dir_on_path() {
    local needle="${1%/}"
    local entry IFS=:
    for entry in $PATH; do
        [ "${entry%/}" = "$needle" ] && return 0
    done
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
    local install_dir
    install_dir="$(pick_install_dir)"
    mkdir -p "$install_dir"

    echo "Downloading cartog ${tag} for ${target}..."
    if curl -fsSL "$url" | tar xz -C "$install_dir"; then
        chmod +x "${install_dir}/cartog"
        echo "cartog installed to ${install_dir}/cartog"
        printf '%s\n' "$install_dir" > "$INSTALL_DIR_MARKER"
        return 0
    fi

    return 1
}

verify_install() {
    # Marker presence means we just wrote the binary; only then is it safe to
    # delete on exec failure. Otherwise we'd nuke the user's existing install
    # when --version regression-breaks a future release.
    local install_dir="" just_installed=0
    if [ -f "$INSTALL_DIR_MARKER" ]; then
        install_dir="$(cat "$INSTALL_DIR_MARKER")"
        rm -f "$INSTALL_DIR_MARKER"
        just_installed=1
    else
        install_dir="$(pick_install_dir)"
    fi
    local bin="${install_dir}/cartog"
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
        if [ "$target_bin" = "$bin" ] && ! dir_on_path "$install_dir"; then
            echo "Note: $install_dir is not in your PATH."
            echo "  Add it with: export PATH=\"$install_dir:\$PATH\""
        fi
        return 0
    fi

    echo "Error: cartog binary exists but failed to run (wrong architecture?)."
    echo "  Output: $version_output"
    [ "$just_installed" = "1" ] && rm -f "$bin"
    return 1
}

# === main (release-only install) ===
# Everything above is function/const definitions; the executable flow starts
# here. The sentinel comment on the line above is a stable anchor for
# test_install.sh, which sources only the definitions — keep it in sync if you
# rename it.
#
# Download a pre-built binary from GitHub Releases. There is no cargo-install
# fallback — a failure here is surfaced, not silently worked around by building
# from source.
if ! target="$(detect_target)"; then
    echo "Error: unsupported platform — no pre-built cartog binary for this OS/arch."
    echo "Build from source instead: https://github.com/${REPO}#install"
    exit 1
fi

if ! install_from_github "$target"; then
    echo "Error: could not download a pre-built cartog binary for ${target}."
    if [ -n "$REQUESTED_VERSION" ]; then
        echo "Check that release v${REQUESTED_VERSION} publishes an asset for ${target}:"
        echo "  https://github.com/${REPO}/releases/tag/v${REQUESTED_VERSION}"
    else
        echo "Check your network/proxy, or see https://github.com/${REPO}/releases"
    fi
    echo "To build from source instead: https://github.com/${REPO}#install"
    exit 1
fi

verify_install
