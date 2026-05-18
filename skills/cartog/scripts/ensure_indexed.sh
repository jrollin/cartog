#!/usr/bin/env bash
set -euo pipefail

# Ensure the cartog index exists and is up to date.
# Run this at the start of a coding session.
#
# Foreground (must finish before Claude responds):
#   F1. Install cartog if the binary is missing (MCP server can't start without it).
#   F2. Code graph index (fast, incremental — usually <1s for unchanged codebases).
#   F3. Passive drift warning if the binary is out of sync with plugin.json.
#       Update itself runs from the SessionEnd hook (update_on_exit.sh) — the
#       MCP `cartog serve` peer is still alive at SessionStart, which would
#       make `cartog self update` fail with PEER_RUNNING (exit 6).
#
# Background (forked into one subshell, logged to ~/.cache/cartog/session.log):
#   B1. Model download (`cartog rag setup`) — enables cross-encoder reranker.
#   B2. RAG embedding (`cartog rag index`) — enables vector/semantic search.
#
# Failures during the background pipeline (or the previous session-end update)
# are written to the log file and surfaced via ~/.cache/cartog/last-error.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd)" || SCRIPT_DIR="."
LOCK_DIR="${CARTOG_LOCK_DIR:-/tmp/cartog-rag-index.lock}"

# Resolve the database path using the same priority as the Rust binary:
#   1. CARTOG_DB env var (explicit override)
#   2. .cartog.toml database.path (local project config)
#   3. Git root: prefer .cartog/db.sqlite, fall back to legacy .cartog.db
#   4. cwd fallback (.cartog/db.sqlite, or legacy .cartog.db if present)
if [ -n "${CARTOG_DB:-}" ]; then
    DB_FILE="$CARTOG_DB"
else
    TOML_DB=""
    GIT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || true
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
# Reading it locally avoids any network call for the version check.
PLUGIN_JSON="${CARTOG_PLUGIN_JSON:-${SCRIPT_DIR}/../../../.claude-plugin/plugin.json}"
PLUGIN_VERSION="$( { sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PLUGIN_JSON" 2>/dev/null || true; } | head -n 1)"

# Background log directory. Falls back to /tmp if ~/.cache isn't writable.
SESSION_LOG_DIR="${CARTOG_LOG_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cartog}"
if ! mkdir -p "$SESSION_LOG_DIR" 2>/dev/null; then
    SESSION_LOG_DIR="/tmp"
fi
SESSION_LOG="$SESSION_LOG_DIR/session.log"
LAST_ERROR_FILE="$SESSION_LOG_DIR/last-error"

# Surface any error from the previous session's background pipeline.
if [ -f "$LAST_ERROR_FILE" ]; then
    echo "Previous cartog background task failed:" >&2
    cat "$LAST_ERROR_FILE" >&2
    rm -f "$LAST_ERROR_FILE"
fi

# F1: install cartog only when the binary is missing. Outdated-but-present
# binaries are upgraded by the SessionEnd hook (update_on_exit.sh), so this
# function does NOT touch the network when cartog is already on PATH.
ensure_cartog_installed() {
    if command -v cartog >/dev/null 2>&1; then
        return 0
    fi
    echo "cartog binary not found on PATH. Installing via $SCRIPT_DIR/install.sh..." >&2
    # Pin to the version this skill was tested against to avoid drift on the next session.
    if ! bash "$SCRIPT_DIR/install.sh" ${PLUGIN_VERSION:+"$PLUGIN_VERSION"} >&2; then
        echo "cartog install failed. See output above." >&2
        exit 1
    fi
    # Probe the same candidates install.sh writes to, in case the chosen dir
    # isn't on PATH yet.
    if ! command -v cartog >/dev/null 2>&1; then
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
    fi
    if ! command -v cartog >/dev/null 2>&1; then
        echo "cartog still not on PATH after install. Add the install directory (printed above) to PATH and retry." >&2
        exit 1
    fi
}

# F3: passive drift warning. Compares the installed binary version against
# the plugin's pinned version and prints a one-line notice. The actual update
# happens in the SessionEnd hook (update_on_exit.sh) where MCP is shutting
# down — `cartog self update` refuses to run while a peer is alive.
warn_if_drifted() {
    [ -n "$PLUGIN_VERSION" ] || return 0
    local installed
    installed="$(cartog --version 2>/dev/null | head -n 1 | sed -E 's/^cartog ([^ ]+).*/\1/')"
    [ -n "$installed" ] || return 0
    [ "$installed" = "$PLUGIN_VERSION" ] && return 0
    echo "cartog binary $installed is out of sync with plugin $PLUGIN_VERSION (will sync on session exit)." >&2
}

# Background pipeline: model download → RAG embedding.
# Single subshell guarded by LOCK_DIR; failures recorded to LAST_ERROR_FILE
# so the next session surfaces them.
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

# --- Foreground execution starts here ---

ensure_cartog_installed

# F2a: config gate. The new bootstrap flow is `cartog init` (writes
# .cartog.toml) BEFORE `cartog index` so the index lands at the configured
# location. On interactive TTY sessions, defer the index when no
# .cartog.toml is present so the agent can ask the user before proceeding.
#
# Bypass cases (proceed with default-config index, no prompt):
#   - non-TTY stdin (CI / piped scripts / unattended setups)
#   - CARTOG_AUTO_INIT=1 (explicit opt-in for users who want defaults)
#
# Note: in MCP sessions, `cartog serve` may have already auto-created an
# empty `.cartog/db.sqlite` at startup. That's harmless — queries against
# an empty index return zero results until the user runs init+index.
_toml_root="${GIT_ROOT:-.}"
if [ ! -f "${_toml_root}/.cartog.toml" ] \
   && [ -z "${CARTOG_AUTO_INIT:-}" ] \
   && [ -t 0 ]; then
    echo "No .cartog.toml found at ${_toml_root}."
    echo "Run \`cartog init\` to scaffold one (writes a commented template, never"
    echo "overwrites). Then \`cartog index\` will build the graph at the configured"
    echo "location. Set CARTOG_AUTO_INIT=1 to skip this check and index with defaults."
    exit 0
fi

# F2b: Code graph index — kept foreground because cartog MCP queries depend on it
# and it's typically <1s for incremental updates.
if [ ! -f "$DB_FILE" ]; then
    echo "No cartog index found. Building..."
else
    echo "Updating cartog index..."
fi
cartog index .

# F3: drift warning (the SessionEnd hook does the actual update).
warn_if_drifted

# Background pipeline: model download + RAG embedding.
# Stale lock (>1h) is removed automatically — handles crashed processes where trap didn't fire.
if [ -d "$LOCK_DIR" ]; then
    # GNU stat (Linux) uses -c %Y; BSD stat (macOS) uses -f %m. Try GNU first
    # because BSD `stat -f %m` would *succeed* on Linux (printing filesystem
    # stats instead of mtime), which would corrupt the arithmetic below.
    lock_mtime="$(stat -c %Y "$LOCK_DIR" 2>/dev/null || stat -f %m "$LOCK_DIR" 2>/dev/null || echo 0)"
    case "$lock_mtime" in
        ''|*[!0-9]*) lock_mtime=0 ;;
    esac
    lock_age=$(( $(date +%s) - lock_mtime ))
    if [ "$lock_age" -gt 3600 ]; then
        echo "Removing stale cartog background lock (${lock_age}s old)."
        rmdir "$LOCK_DIR" 2>/dev/null || true
    fi
fi
if mkdir "$LOCK_DIR" 2>/dev/null; then
    (
        trap 'rmdir "$LOCK_DIR" 2>/dev/null' EXIT
        run_background_pipeline
    ) &
    BG_PID=$!
    disown "$BG_PID" 2>/dev/null || true
    echo "cartog background tasks started (PID $BG_PID, log: $SESSION_LOG)"
    echo "cartog index ready. Reranker + vector search become available once background tasks complete."
else
    echo "cartog background pipeline already running (lock: $LOCK_DIR), skipping."
    echo "cartog index ready."
fi
