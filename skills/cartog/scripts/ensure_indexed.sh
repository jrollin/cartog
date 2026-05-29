#!/usr/bin/env bash
set -euo pipefail

# SessionStart hook: ensure cartog index is current without blocking the TUI.
#
# Foreground (sub-second when binary is present):
#   F1. Surface any error from the previous session's background pipeline.
#   F2. If binary is MISSING, fork install + first-index + RAG into the
#       background subshell and exit 0 fast. MCP won't have cartog this
#       session — it will pick up next session. /cartog-install repairs
#       on demand.
#   F3. Drift warning if the installed binary doesn't match plugin.json.
#       The actual update is the user's call via /cartog-install (or, for
#       binaries <0.14.0, the transitional SessionEnd hook).
#   F4. `cartog index .` (incremental, typically <1s for unchanged trees).
#
# Background (forked into one subshell, logged to ~/.cache/cartog/session.log):
#   B1. cartog rag setup — download cross-encoder reranker (~100MB, first time).
#   B2. cartog rag index . — embed symbols for vector search.
#
# Failures during the background pipeline (or, separately, the foreground
# index) are written to ~/.cache/cartog/last-error and surfaced on the next
# SessionStart.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd)" || SCRIPT_DIR="."
LOCK_DIR="${CARTOG_LOCK_DIR:-/tmp/cartog-rag-index.lock}"

# GIT_ROOT is needed for both the DB resolver below and the no-toml gate
# in the foreground flow, so resolve it once up front, independent of
# CARTOG_DB. (Setting CARTOG_DB used to bypass the resolver branch and
# leave GIT_ROOT unset, causing the gate to miss a git-root .cartog.toml
# when run from a subdirectory.)
GIT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || true

# Resolve the database path using the same priority as the Rust binary:
#   1. CARTOG_DB env var (explicit override)
#   2. .cartog.toml database.path (local project config)
#   3. Git root: prefer .cartog/db.sqlite, fall back to legacy .cartog.db
#   4. cwd fallback (.cartog/db.sqlite, or legacy .cartog.db if present)
if [ -n "${CARTOG_DB:-}" ]; then
    DB_FILE="$CARTOG_DB"
else
    TOML_DB=""
    for _dir in "." "$GIT_ROOT"; do
        [ -n "$_dir" ] && [ -f "$_dir/.cartog.toml" ] && {
            TOML_DB="$(sed -n '/^\[database\]/,/^\[/{s/^path[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p;}' "$_dir/.cartog.toml" 2>/dev/null)" || true
            [ -n "$TOML_DB" ] && break
        }
    done
    if [ -n "$TOML_DB" ]; then
        case "$TOML_DB" in
            "~/"*) DB_FILE="${HOME}${TOML_DB#\~}" ;;
            *)     DB_FILE="$TOML_DB" ;;
        esac
    else
        _root="${GIT_ROOT:-.}"
        if [ -f "${_root}/.cartog/db.sqlite" ]; then
            DB_FILE="${_root}/.cartog/db.sqlite"
        elif [ -f "${_root}/.cartog.db" ]; then
            DB_FILE="${_root}/.cartog.db"
        else
            DB_FILE="${_root}/.cartog/db.sqlite"
        fi
    fi
fi

# Plugin tag is kept in sync with the binary version at release time.
PLUGIN_JSON="${CARTOG_PLUGIN_JSON:-${SCRIPT_DIR}/../../../.claude-plugin/plugin.json}"
PLUGIN_VERSION="$( { sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PLUGIN_JSON" 2>/dev/null || true; } | head -n 1)"

SESSION_LOG_DIR="${CARTOG_LOG_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cartog}"
if ! mkdir -p "$SESSION_LOG_DIR" 2>/dev/null; then
    SESSION_LOG_DIR="/tmp"
fi
SESSION_LOG="$SESSION_LOG_DIR/session.log"
LAST_ERROR_FILE="$SESSION_LOG_DIR/last-error"
# Breadcrumb written by `cartog self update --apply-pending` on a successful
# boundary swap; surfaced once here to confirm the deferred update landed.
LAST_UPDATE_FILE="$SESSION_LOG_DIR/last-update"

# F1: surface any error from the previous session's background pipeline.
# stdout (not stderr): SessionStart stdout reaches the user via the model's
# context; stderr is discarded when the hook exits 0.
if [ -f "$LAST_ERROR_FILE" ]; then
    echo "Previous cartog background task failed:"
    cat "$LAST_ERROR_FILE" 2>/dev/null || true
    rm -f "$LAST_ERROR_FILE"
fi

# F1b: surface (and clear) a completed deferred update from the last session.
# Guard the cat: under `set -e`, an unreadable file that still passes -f would
# otherwise abort the whole hook before indexing runs.
if [ -f "$LAST_UPDATE_FILE" ]; then
    cat "$LAST_UPDATE_FILE" 2>/dev/null || true
    rm -f "$LAST_UPDATE_FILE"
fi

# Semver compare: returns 0 iff $1 < $2 component-wise (pre-release suffix
# stripped). Mirrors version_gt in update_on_exit.sh.
version_lt() {
    local IFS=.
    local -a a b
    read -ra a <<< "${1%%-*}"
    read -ra b <<< "${2%%-*}"
    local i
    for ((i=0; i<${#a[@]} || i<${#b[@]}; i++)); do
        local ai="${a[i]:-0}" bi="${b[i]:-0}"
        if [ "$ai" -lt "$bi" ] 2>/dev/null; then return 0; fi
        if [ "$ai" -gt "$bi" ] 2>/dev/null; then return 1; fi
    done
    return 1
}

# F3: passive drift warning. Warns only when the installed binary is OLDER than
# the plugin's pinned version — an equal or newer binary (e.g. a deliberate
# manual install ahead of the pin) is left alone. Pending-aware: if a deferred
# update is already armed, it says so instead of nagging to run /cartog-install.
# The actual swap happens at SessionEnd (>=0.14) or via /cartog-install.
#
# Notices go to STDOUT, not stderr: Claude Code injects a SessionStart hook's
# stdout into the model's context (so it surfaces to the user), but discards
# stderr when the hook exits 0. A drift notice on stderr would be invisible.
warn_if_drifted() {
    [ -n "$PLUGIN_VERSION" ] || return 0
    local info installed pending
    info="$(cartog self version --json 2>/dev/null)" || info=""
    if [ -n "$info" ]; then
        installed="$(printf '%s' "$info" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
        pending="$(printf '%s' "$info" | sed -n 's/.*"target_version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    else
        installed="$(cartog --version 2>/dev/null | head -n 1 | sed -E 's/^cartog ([^ ]+).*/\1/')"
        pending=""
    fi
    [ -n "$installed" ] || return 0
    # Only an OLDER binary is "drifted". Equal or ahead → nothing to do.
    version_lt "$installed" "$PLUGIN_VERSION" || return 0
    if [ -n "$pending" ]; then
        if [ "$pending" = "$PLUGIN_VERSION" ]; then
            # Armed for the current pin. The apply runs at SessionEnd, but only
            # once no other cartog session/watch holds the lock — say so, so a
            # user with a second window open isn't left wondering why nothing
            # changed.
            echo "cartog $pending will be applied when this session ends (once any other cartog sessions close)."
        else
            # A deferred update is armed, but the plugin pin has since moved.
            # Tell the user the armed target is stale and how to re-arm.
            echo "cartog has a deferred update to $pending armed, but the plugin now wants $PLUGIN_VERSION — run /cartog-install to re-arm."
        fi
        return 0
    fi
    echo "cartog binary $installed is out of sync with plugin $PLUGIN_VERSION (run /cartog-install to update)."
}

# Background pipeline (steady-state): RAG setup → RAG index.
run_background_pipeline() {
    local pipeline_rc=0
    {
        echo "=== cartog session log $(date '+%Y-%m-%d %H:%M:%S') ==="
        echo "--- B1: rag setup (model download) ---"
        if ! cartog rag setup; then
            pipeline_rc=1
            echo "B1 failed; semantic search will use FTS5 only (no reranker)." >&2
        fi
        echo "--- B2: rag index (vector embedding) ---"
        if ! cartog rag index .; then
            pipeline_rc=1
            echo "B2 failed; vector search unavailable." >&2
        fi
        echo "=== pipeline exit $pipeline_rc ==="
    } >> "$SESSION_LOG" 2>&1

    if [ "$pipeline_rc" -ne 0 ]; then
        printf 'See %s for details (pipeline exit %d).\n' "$SESSION_LOG" "$pipeline_rc" > "$LAST_ERROR_FILE"
    fi
    return "$pipeline_rc"
}

# Background pipeline (first install): install.sh → [index → RAG].
# Runs when the binary was missing at SessionStart, so we cannot rely on
# anything from the user's PATH at the time the hook was invoked.
#
# Args:
#   $1 = "1" to run B1/B2/B3 (index + RAG) after install, "0" to install only.
#        Set to "0" when the no-toml gate would have skipped indexing in the
#        foreground (so we don't auto-index a project the user hasn't opted into).
run_install_pipeline() {
    local do_index="${1:-1}"
    local pipeline_rc=0
    {
        echo "=== cartog install + first-index $(date '+%Y-%m-%d %H:%M:%S') ==="
        echo "--- B0: install.sh ---"
        # Pin to PLUGIN_VERSION so the marketplace pin holds. install.sh
        # accepts an empty arg as "latest"; ${VAR:+"$VAR"} expands to nothing
        # when VAR is empty, which is the right fallback (e.g. test fixtures
        # without plugin.json).
        if ! bash "$SCRIPT_DIR/install.sh" ${PLUGIN_VERSION:+"$PLUGIN_VERSION"}; then
            pipeline_rc=1
            echo "B0: install.sh failed; aborting pipeline." >&2
        fi
        if [ "$pipeline_rc" -eq 0 ]; then
            # install.sh may write to ~/.local/bin or CARGO_HOME/bin — probe.
            for _candidate in \
                "${CARTOG_INSTALL_DIR:-}" \
                "$HOME/.local/bin" \
                "${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"; do
                [ -n "$_candidate" ] || continue
                if [ -x "$_candidate/cartog" ]; then
                    export PATH="$_candidate:$PATH"
                    break
                fi
            done
            hash -r 2>/dev/null || true
            if ! command -v cartog >/dev/null 2>&1; then
                pipeline_rc=1
                echo "B0: install.sh succeeded but cartog is still not on PATH." >&2
            fi
        fi
        if [ "$pipeline_rc" -eq 0 ] && [ "$do_index" = "1" ]; then
            echo "--- B1: cartog index ---"
            if ! cartog index .; then
                pipeline_rc=1
                echo "B1: index failed; skipping RAG steps (they depend on a valid index)." >&2
            else
                echo "--- B2: rag setup (model download) ---"
                if ! cartog rag setup; then
                    pipeline_rc=1
                    echo "B2: rag setup failed; semantic search will use FTS5 only." >&2
                fi
                echo "--- B3: rag index (vector embedding) ---"
                if ! cartog rag index .; then
                    pipeline_rc=1
                    echo "B3: rag index failed; vector search unavailable." >&2
                fi
            fi
        elif [ "$pipeline_rc" -eq 0 ]; then
            echo "--- no-toml gate active: skipping B1/B2/B3 (run \`cartog init\` then re-launch) ---"
        fi
        echo "=== pipeline exit $pipeline_rc ==="
    } >> "$SESSION_LOG" 2>&1

    if [ "$pipeline_rc" -ne 0 ]; then
        printf 'See %s for details (install pipeline exit %d).\n' "$SESSION_LOG" "$pipeline_rc" > "$LAST_ERROR_FILE"
    fi
    return "$pipeline_rc"
}

# Acquire the lock and fork the chosen pipeline.
# $1 = function name to run inside the background subshell.
# Remaining args are forwarded to the function.
fork_background() {
    local pipeline_fn="$1"
    shift
    # Fail loudly if the dispatched function doesn't exist — a typo here would
    # otherwise produce a green "background tasks started" with no actual work
    # and no last-error to surface next session.
    if ! declare -F "$pipeline_fn" >/dev/null; then
        echo "Internal error: pipeline function '$pipeline_fn' is not defined." >&2
        printf 'Internal error: pipeline function %s is not defined (see hook script).\n' \
            "$pipeline_fn" > "$LAST_ERROR_FILE"
        return 2
    fi
    # Stale lock (>1h) is removed automatically — handles crashed processes.
    if [ -d "$LOCK_DIR" ]; then
        local lock_mtime
        lock_mtime="$(stat -c %Y "$LOCK_DIR" 2>/dev/null || stat -f %m "$LOCK_DIR" 2>/dev/null || echo 0)"
        case "$lock_mtime" in
            ''|*[!0-9]*) lock_mtime=0 ;;
        esac
        local lock_age=$(( $(date +%s) - lock_mtime ))
        if [ "$lock_age" -gt 3600 ]; then
            echo "Removing stale cartog background lock (${lock_age}s old)."
            rmdir "$LOCK_DIR" 2>/dev/null || true
        fi
    fi
    if mkdir "$LOCK_DIR" 2>/dev/null; then
        (
            trap 'rmdir "$LOCK_DIR" 2>/dev/null' EXIT
            "$pipeline_fn" "$@"
        ) &
        BG_PID=$!
        disown "$BG_PID" 2>/dev/null || true
        echo "cartog background tasks started (PID $BG_PID, log: $SESSION_LOG)"
        return 0
    else
        echo "cartog background pipeline already running (lock: $LOCK_DIR), skipping."
        return 1
    fi
}

# --- Foreground execution starts here ---

# Evaluate the no-toml gate first so we can decide whether the missing-binary
# background pipeline should also index (it should NOT auto-index a project
# the user hasn't opted into). Check both cwd and the git root — matches
# the DB resolver's search order above.
_toml_root="${GIT_ROOT:-.}"
_has_toml=0
if [ -n "${CARTOG_AUTO_INIT:-}" ] \
   || [ -f "./.cartog.toml" ] \
   || { [ -n "$GIT_ROOT" ] && [ -f "${GIT_ROOT}/.cartog.toml" ]; }; then
    _has_toml=1
fi

# F2: binary missing → background-install. MCP can't start this session;
# /cartog-install is the explicit repair verb.
if ! command -v cartog >/dev/null 2>&1; then
    echo "cartog binary not on PATH. Installing in background — MCP tools available next session."
    echo "Run /cartog-install to install synchronously or to retry on failure."
    # Pass _has_toml so run_install_pipeline skips B1/B2/B3 when the gate
    # would have skipped indexing in the foreground.
    if ! fork_background run_install_pipeline "$_has_toml"; then
        # Lock held by another process AND binary is missing — the user's
        # "MCP tools available next session" promise won't hold. Surface
        # this (stdout → model context → user) so they don't wait forever.
        echo "Another cartog session is already installing or indexing. If this persists, remove $LOCK_DIR and run /cartog-install."
        printf 'SessionStart could not start install: lock %s held by another process. If stale, remove it and run /cartog-install.\n' \
            "$LOCK_DIR" > "$LAST_ERROR_FILE"
    fi
    exit 0
fi

# F2a: config gate. If `.cartog.toml` is missing, defer indexing on
# interactive TTY sessions (so the agent can ask the user before running
# `cartog init`). Non-TTY sessions (CI, piped scripts) exit silently —
# auto-indexing with defaults would write to a location the user didn't
# choose. CARTOG_AUTO_INIT=1 forces indexing regardless.
if [ "$_has_toml" -ne 1 ]; then
    if [ -t 0 ]; then
        echo "No .cartog.toml found at ${_toml_root}."
        echo "Run \`cartog init\` to scaffold one, then re-launch Claude Code."
        echo "Set CARTOG_AUTO_INIT=1 to index with defaults instead."
    fi
    exit 0
fi

# F4: foreground index. Incremental, typically <1s. Failure is recorded
# and surfaced next session; we do NOT abort because the background RAG
# pipeline is independent and reranker + vector search are still useful.
if [ ! -f "$DB_FILE" ]; then
    echo "No cartog index found. Building..."
else
    echo "Updating cartog index..."
fi
index_rc=0
# CARTOG_PROGRESS=1 opts into the non-TTY phase heartbeat so the first (cold)
# index isn't a silent multi-minute wait in this hook. Other callers stay quiet.
CARTOG_PROGRESS=1 cartog index . || index_rc=$?
if [ "$index_rc" -ne 0 ]; then
    echo "cartog index failed (exit $index_rc) — continuing to background pipeline." >&2
    printf 'cartog index . failed (exit %d). See terminal output above.\n' "$index_rc" > "$LAST_ERROR_FILE"
fi

# F3: drift warning (the SessionEnd hook does the actual update).
warn_if_drifted

# Background pipeline.
if fork_background run_background_pipeline; then
    echo "cartog index ready. Reranker + vector search become available once background tasks complete."
else
    echo "cartog index ready."
fi
