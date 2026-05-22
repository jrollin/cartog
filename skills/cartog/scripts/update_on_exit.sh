#!/usr/bin/env bash
set -euo pipefail

# SessionEnd hook: TRANSITIONAL — scoped to the pre-self-update cohort.
#
# Cartog binaries released before 0.14.0 do not have `cartog self update`.
# For that cohort, this hook is the only path to upgrade their binary
# without a manual reinstall. Once a user is on >= 0.14.0, the explicit
# repair verb is `/cartog-install` (handled by the cartog-install skill),
# which is also what new users get from a marketplace install.
#
# Sunset: this hook is scheduled for removal in the next plugin release
# after the one that introduced /cartog-install. Long-dormant <0.14.0
# users past the sunset will need to reinstall manually.
#
# Failure modes are written to ~/.cache/cartog/last-error and surfaced by
# ensure_indexed.sh on the next session start.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd)" || SCRIPT_DIR="."

# Coordinated with ensure_indexed.sh — refuse to swap the binary while a
# RAG pipeline (or background install) is mid-flight on the same disk.
LOCK_DIR="${CARTOG_LOCK_DIR:-/tmp/cartog-rag-index.lock}"

PLUGIN_JSON="${CARTOG_PLUGIN_JSON:-${SCRIPT_DIR}/../../../.claude-plugin/plugin.json}"
PLUGIN_VERSION="$( { sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PLUGIN_JSON" 2>/dev/null || true; } | head -n 1)"

SESSION_LOG_DIR="${CARTOG_LOG_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cartog}"
if ! mkdir -p "$SESSION_LOG_DIR" 2>/dev/null; then
    SESSION_LOG_DIR="/tmp"
fi
SESSION_LOG="$SESSION_LOG_DIR/session.log"
LAST_ERROR_FILE="$SESSION_LOG_DIR/last-error"

# Bail quietly if cartog isn't installed — nothing to update, and the
# missing-binary case is handled by ensure_indexed.sh + /cartog-install.
command -v cartog >/dev/null 2>&1 || exit 0

# Semver compare: returns 0 iff $1 > $2 component-wise (pre-release suffix stripped).
version_gt() {
    local IFS=.
    local -a a b
    read -ra a <<< "${1%%-*}"
    read -ra b <<< "${2%%-*}"
    local i
    for ((i=0; i<${#a[@]} || i<${#b[@]}; i++)); do
        local ai="${a[i]:-0}" bi="${b[i]:-0}"
        if [ "$ai" -gt "$bi" ] 2>/dev/null; then return 0; fi
        if [ "$ai" -lt "$bi" ] 2>/dev/null; then return 1; fi
    done
    return 1
}

# Returns 0 if a background pipeline (install.sh, index, RAG) is running.
# Swapping the binary during that window would leave the in-flight process
# on an unlinked inode.
rag_pipeline_running() {
    [ -d "$LOCK_DIR" ] || return 1
    local lock_mtime now age
    lock_mtime="$(stat -c %Y "$LOCK_DIR" 2>/dev/null || stat -f %m "$LOCK_DIR" 2>/dev/null || echo 0)"
    case "$lock_mtime" in ''|*[!0-9]*) lock_mtime=0 ;; esac
    now="$(date +%s)"
    age=$((now - lock_mtime))
    [ "$age" -lt 3600 ]
}

run_update() {
    local installed
    installed="$(cartog --version 2>/dev/null | head -n 1 | sed -E 's/^cartog ([^ ]+).*/\1/')"
    [ -n "$installed" ] || return 0

    # Modern binaries (>= 0.14.0) update via /cartog-install (or `cartog self
    # update`). This hook does not handle them. Drift is the user's call.
    if ! version_gt "0.14.0" "$installed"; then
        return 0
    fi

    # No work if there's nothing to upgrade to.
    if [ -z "$PLUGIN_VERSION" ] || [ "$installed" = "$PLUGIN_VERSION" ]; then
        return 0
    fi

    if rag_pipeline_running; then
        echo "Skipping pre-0.14 upgrade: background pipeline still running (lock: $LOCK_DIR)."
        return 0
    fi

    echo "Upgrading pre-self-update cartog $installed → ${PLUGIN_VERSION} via install.sh..."
    if ! bash "$SCRIPT_DIR/install.sh" "$PLUGIN_VERSION"; then
        echo "install.sh failed."
        return 1
    fi
    return 0
}

{
    echo "=== cartog session-end update $(date '+%Y-%m-%d %H:%M:%S') ==="
    if ! run_update; then
        printf 'See %s for details (session-end update failed).\n' "$SESSION_LOG" > "$LAST_ERROR_FILE"
        echo "=== session-end update exit 1 ==="
        exit 0
    fi
    echo "=== session-end update exit 0 ==="
} >> "$SESSION_LOG" 2>&1

exit 0
