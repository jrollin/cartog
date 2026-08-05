#!/usr/bin/env bash
set -euo pipefail

# SessionEnd hook: bring the binary in line with the plugin pin at the safe
# boundary (the serve peer is exiting, so the binary is no longer inode-pinned).
# One gate, probed not assumed — can the binary apply a deferred update itself?
#   has --apply-pending (>= 0.20.0): apply any update armed in-session.
#   otherwise: pin-exact install.sh upgrade — covers the 0.14..0.20 band (has
#     `self update` but not the deferred flags) and pre-0.14.0 (no `self update`).
#
# Failure modes are written to ~/.cache/cartog/last-error and surfaced by
# ensure_indexed.sh on the next session start. A successful boundary swap
# writes ~/.cache/cartog/last-update, also surfaced next session.

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
# Kill-safe breadcrumb: bracketed around the swap, so only a SIGKILL mid-swap
# leaves it. ensure_indexed.sh surfaces a leftover next SessionStart.
APPLY_MARKER_FILE="$SESSION_LOG_DIR/apply-in-progress"

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

# True if `self update` accepts --apply-pending (landed in 0.20.0). Probe the
# help text, not the version, so the gate tracks real capability across releases.
# Require the probe itself to succeed: a non-zero `self update --help` (binary too
# old to have the subcommand, or broken) routes to the installer, never a false
# positive into the deferred path.
supports_deferred_update() {
    local help
    help=$(cartog self update --help 2>&1) || return 1
    printf '%s' "$help" | grep -q -- '--apply-pending'
}

# Apply a deferred update on a binary with deferred-flag support (>= 0.20.0).
# Runs `cartog self update --apply-pending`, which no-ops when nothing is armed.
# Owns its own last-error breadcrumb so the caller can treat its return value
# as "did something need surfacing" rather than a raw exit code.
apply_pending_update() {
    if rag_pipeline_running; then
        echo "Skipping deferred-update apply: background pipeline still running (lock: $LOCK_DIR)."
        return 0
    fi

    # Auto-arm the pin on drift so a PASSIVE user (who never runs /cartog-install)
    # still converges to the plugin's pinned version. We pin via --to so no
    # network fetch is needed and the apply can't overshoot; `cartog self update`
    # decide_apply blocks any downgrade, and --defer is idempotent (re-arming the
    # same target, or arming when already current, is harmless). Only arm when
    # the binary is strictly OLDER than the pin (installed < PLUGIN_VERSION).
    if [ -n "$PLUGIN_VERSION" ] && version_gt "$PLUGIN_VERSION" "$installed"; then
        cartog self update --defer --to "$PLUGIN_VERSION" --quiet || true
    fi

    local rc=0
    printf '%s\n' "${PLUGIN_VERSION:-unknown}" > "$APPLY_MARKER_FILE" 2>/dev/null || true
    cartog self update --apply-pending || rc=$?
    rm -f "$APPLY_MARKER_FILE" 2>/dev/null || true
    case "$rc" in
        0)
            echo "Deferred update applied (or nothing pending)."
            ;;
        6)
            # Expected fallback, NOT an error: the intent stays armed and the next
            # boundary retries, so no last-error. The binary's own stderr (already
            # in $SESSION_LOG) names the blocking slot/PID, so keep this line
            # neutral rather than restating a count that could contradict it.
            echo "Deferred update kept: a cartog process still holds the lock (see message above); retries at the next session boundary."
            ;;
        2|5)
            # Transient (network / disk). Intent kept by the binary; retries.
            printf 'cartog self update --apply-pending failed (exit %d, transient); will retry next session. See %s.\n' \
                "$rc" "$SESSION_LOG" > "$LAST_ERROR_FILE"
            ;;
        7)
            # Smoke-test failure — deterministic for this target; the binary
            # cleared the intent and restored the previous binary. Surface an
            # actionable message (it will NOT retry automatically).
            printf 'cartog update failed verification (smoke test) and was rolled back; it will not retry automatically. Run `cartog self update` in a terminal or /cartog-install. See %s.\n' \
                "$SESSION_LOG" > "$LAST_ERROR_FILE"
            ;;
        3)
            # Cargo-installed binary — self update cannot swap it. Give the
            # cohort the command that actually works, not a generic failure.
            printf 'cartog was installed via cargo and cannot be auto-updated; run `cargo install cartog --force` to upgrade. See %s.\n' \
                "$SESSION_LOG" > "$LAST_ERROR_FILE"
            ;;
        *)
            # 4 (checksum) and any other terminal failure — intent cleared by
            # the binary where applicable.
            printf 'cartog self update --apply-pending failed (exit %d). See %s.\n' \
                "$rc" "$SESSION_LOG" > "$LAST_ERROR_FILE"
            ;;
    esac
}

# Pin-exact install.sh upgrade for any binary that can't self-apply a deferred
# update. Preferred over plain `cartog self update`, which fetches the LATEST
# release (no `--to` on < 0.20.0 to constrain it) and carries peer-lock/smoke
# exit codes that don't fit a session-end swap. Non-zero only on install failure.
upgrade_via_installer() {
    # Upgrade only when strictly behind the pin (version_gt avoids downgrading a
    # manually-installed newer binary that a string `!=` would catch).
    if [ -z "$PLUGIN_VERSION" ] || ! version_gt "$PLUGIN_VERSION" "$installed"; then
        return 0
    fi
    if rag_pipeline_running; then
        echo "Skipping installer upgrade: background pipeline still running (lock: $LOCK_DIR)."
        return 0
    fi
    echo "Upgrading cartog $installed → ${PLUGIN_VERSION} via install.sh (binary lacks deferred-update support)..."
    if ! bash "$SCRIPT_DIR/install.sh" "$PLUGIN_VERSION"; then
        echo "install.sh failed."
        return 1
    fi
    return 0
}

run_update() {
    installed="$(cartog --version 2>/dev/null | head -n 1 | sed -E 's/^cartog ([^ ]+).*/\1/')"
    [ -n "$installed" ] || return 0

    # Capability is the only gate: a binary with no `self update` also lacks
    # --apply-pending, so the probe routes both old cohorts to the installer.
    if ! supports_deferred_update; then
        upgrade_via_installer
        return $?
    fi
    apply_pending_update
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
