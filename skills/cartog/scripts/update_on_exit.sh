#!/usr/bin/env bash
set -euo pipefail

# SessionEnd hook: apply deferred updates at the safe boundary.
#
# Two cohorts:
#   >= 0.14.0 — applies any deferred update armed in-session (via
#               /cartog-install or the cartog_update MCP tool) by running
#               `cartog self update --apply-pending`. The serve process is
#               exiting as this fires, so the binary is no longer pinned by a
#               live inode. Drift detection / arming itself lives in the
#               SessionStart hook and /cartog-install, not here.
#   <  0.14.0 — TRANSITIONAL: those binaries lack `cartog self update`, so this
#               hook is their only no-reinstall upgrade path (via install.sh).
#               Sunset: scheduled for removal a release after /cartog-install
#               shipped; long-dormant <0.14.0 users will then reinstall manually.
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

# Apply a deferred update on a modern (>= 0.14.0) binary. Runs
# `cartog self update --apply-pending`, which no-ops when nothing is armed.
# Owns its own last-error breadcrumb so the caller can treat its return value
# as "did something need surfacing" rather than a raw exit code.
apply_pending_update() {
    if rag_pipeline_running; then
        echo "Skipping deferred-update apply: background pipeline still running (lock: $LOCK_DIR)."
        return 0
    fi

    local rc=0
    cartog self update --apply-pending || rc=$?
    case "$rc" in
        0)
            echo "Deferred update applied (or nothing pending)."
            ;;
        6)
            # Peer still running at session end — expected fallback, NOT an
            # error. The intent stays armed; the next session retries. The
            # SessionStart drift line tells the user the apply is waiting on
            # other sessions, so we deliberately do NOT cry wolf via last-error.
            echo "Another cartog process still running; deferred update kept for next session."
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
        *)
            # 3 (cargo) / 4 (checksum) — terminal for this target; intent
            # cleared by the binary.
            printf 'cartog self update --apply-pending failed (exit %d). See %s.\n' \
                "$rc" "$SESSION_LOG" > "$LAST_ERROR_FILE"
            ;;
    esac
}

# Upgrade a pre-self-update (< 0.14.0) binary via install.sh — the transitional
# path for the cohort that lacks `cartog self update`. Returns non-zero only on
# a genuine install failure (the caller writes last-error).
upgrade_legacy() {
    # Only upgrade when the installed binary is strictly OLDER than the pin —
    # a string `!=` would also "upgrade" (downgrade) a manually-installed newer
    # legacy binary. version_gt PLUGIN > installed ⇔ installed < PLUGIN.
    if [ -z "$PLUGIN_VERSION" ] || ! version_gt "$PLUGIN_VERSION" "$installed"; then
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

run_update() {
    installed="$(cartog --version 2>/dev/null | head -n 1 | sed -E 's/^cartog ([^ ]+).*/\1/')"
    [ -n "$installed" ] || return 0

    # < 0.14.0: transitional install.sh path. >= 0.14.0: apply any armed
    # deferred update.
    if version_gt "0.14.0" "$installed"; then
        upgrade_legacy
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
