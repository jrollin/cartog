#!/usr/bin/env bash
set -euo pipefail

# SessionStart hook, run alongside ensure_indexed.sh: a USER-VISIBLE notice when
# the installed binary is older than the plugin pin and the user has to act.
#
# Why a second script: Claude Code parses a hook's stdout as JSON only when the
# whole stdout is one JSON document. ensure_indexed.sh interleaves its own lines
# with `cartog index` output, so its drift line can only reach the model (plain
# stdout goes to the model's context, not the terminal). This script prints
# exactly one JSON object — `systemMessage` is shown to the user,
# `additionalContext` reaches the model — or nothing at all.
#
# Silent (exit 0, no output) unless ALL of these hold:
#   1. CARTOG_NO_UPDATE_CHECK is unset or empty (same kill switch as the
#      background update check and `cartog doctor`).
#   2. plugin.json carries a version and the cartog binary is on PATH.
#   3. The project is cartog-configured — the same three consent signals as
#      ensure_indexed.sh (CARTOG_AUTO_INIT, a .cartog.toml up to the git root,
#      an existing DB file) plus an explicit CARTOG_DB. An unconfigured repo
#      gets no cartog output of any kind.
#   4. installed < pin. Equal or ahead (a deliberate manual install) is left alone.
#   5. No update to exactly the pin is already armed — that case converges on
#      its own at the next session boundary, so there is nothing to ask of the
#      user (ensure_indexed.sh's model-facing line still says so).
#   6. This (installed, pin) pair has not been announced in the last 24 hours,
#      across all windows and projects (marker file in the cartog cache dir).
#      A new plugin pin or a changed binary re-notifies.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd)" || SCRIPT_DIR="."

[ -z "${CARTOG_NO_UPDATE_CHECK:-}" ] || exit 0

PLUGIN_JSON="${CARTOG_PLUGIN_JSON:-${SCRIPT_DIR}/../../../.claude-plugin/plugin.json}"
PLUGIN_VERSION="$( { sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PLUGIN_JSON" 2>/dev/null || true; } | head -n 1)"
[ -n "$PLUGIN_VERSION" ] || exit 0

command -v cartog >/dev/null 2>&1 || exit 0

SESSION_LOG_DIR="${CARTOG_LOG_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cartog}"
if ! mkdir -p "$SESSION_LOG_DIR" 2>/dev/null; then
    SESSION_LOG_DIR="/tmp"
fi

# Semver compare: returns 0 iff $1 < $2 component-wise (pre-release suffix
# stripped). Mirrors version_lt in ensure_indexed.sh.
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

# Walk up from cwd looking for .cartog.toml, stopping at the git root (mirrors
# the binary's local_config_path and ensure_indexed.sh's _find_cartog_toml).
find_cartog_toml() {
    local dir; dir="$(pwd)"
    while :; do
        [ -f "$dir/.cartog.toml" ] && return 0
        [ -e "$dir/.git" ] && return 1
        local parent; parent="$(dirname "$dir")"
        [ "$parent" = "$dir" ] && return 1
        dir="$parent"
    done
}

# Consent gate. A .cartog.toml with a custom `[database] path` still passes on
# the toml alone, so the DB-path resolver is not needed here.
#
# CARTOG_DB grants consent only when the file it names EXISTS: ensure_indexed.sh
# uses the variable to *resolve* the path and then gates on `[ -f "$DB_FILE" ]`.
# Treating the bare variable as consent would notify in every unrelated repo of
# anyone who exports CARTOG_DB from a shell profile, breaking this file's own
# rule that an unconfigured repo gets no cartog output.
project_is_configured() {
    [ -n "${CARTOG_AUTO_INIT:-}" ] && return 0
    [ -n "${CARTOG_DB:-}" ] && [ -f "$CARTOG_DB" ] && return 0
    find_cartog_toml && return 0
    local root
    root="$(git rev-parse --show-toplevel 2>/dev/null)" || root="."
    [ -f "$root/.cartog/db.sqlite" ] || [ -f "$root/.cartog.db" ]
}
project_is_configured || exit 0

info="$(cartog self version --json 2>/dev/null)" || info=""
if [ -n "$info" ]; then
    installed="$(printf '%s' "$info" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    pending="$(printf '%s' "$info" | sed -n 's/.*"target_version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    source="$(printf '%s' "$info" | sed -n 's/.*"install_source"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
else
    # `|| installed=""`: under `pipefail` a broken binary's non-zero status would
    # propagate out of the assignment and `set -e` would kill the hook BEFORE the
    # emptiness guard below. An empty exit-2 hook reads to Claude Code as a
    # blocking error, so a half-swapped binary must degrade to silence instead.
    installed="$(cartog --version 2>/dev/null | head -n 1 | sed -E 's/^cartog ([^ ]+).*/\1/')" \
        || installed=""
    pending=""
    source=""
fi
[ -n "$installed" ] || exit 0
version_lt "$installed" "$PLUGIN_VERSION" || exit 0
[ "$pending" != "$PLUGIN_VERSION" ] || exit 0

# Versions are interpolated into JSON and a filename: keep only semver chars.
safe_installed="$(printf '%s' "$installed" | tr -cd '0-9A-Za-z.+-')"
safe_pin="$(printf '%s' "$PLUGIN_VERSION" | tr -cd '0-9A-Za-z.+-')"
marker="$SESSION_LOG_DIR/drift-notified-${safe_installed}-${safe_pin}"

# Was this pair announced in the last 24h? `find -mmin` is the portable mtime
# test (GNU and BSD alike); an unreadable marker directory just means we notify.
notified_recently() {
    [ -f "$marker" ] && [ -n "$(find "$marker" -mmin -1440 2>/dev/null)" ]
}
notified_recently && exit 0

# Serialise the remaining check-and-write. Windows opened at the same instant
# all pass the check above, and a plain `: > "$marker"` let every one of them
# print (a 6-way race emitted 6 notices). `mkdir` is the portable test-and-set:
# one caller creates the directory, the rest exit quiet. A claim left by a
# killed run is reaped after an hour, so a crash cannot mute the notice forever.
claim="${marker}.claim"
if [ -d "$claim" ] && [ -n "$(find "$claim" -maxdepth 0 -mmin +60 2>/dev/null)" ]; then
    rmdir "$claim" 2>/dev/null || true
fi
mkdir "$claim" 2>/dev/null || exit 0
trap 'rmdir "$claim" 2>/dev/null || true' EXIT

# Re-check under the claim. A process can pass the first check, block while the
# winner writes the marker and releases, then acquire the freed claim: without
# this second look it would print a duplicate notice (2 of 6 did).
notified_recently && exit 0

if [ "$source" = "cargo" ]; then
    # `cartog self update` refuses a cargo-managed binary (exit 3): this cohort
    # can only converge by hand, which is the one case that must reach the user.
    message="cartog ${safe_installed} is older than the cartog plugin (${safe_pin}) and was installed via cargo, so it cannot auto-update. Run: cargo install cartog --force"
    context="${message}. Mention this to the user once; do not run the command for them."
else
    message="cartog ${safe_installed} is older than the cartog plugin (${safe_pin}). It updates automatically when this session ends; to update now, run /cartog-install."
    context="${message} Mention this to the user once; ask before running /cartog-install."
fi

# Remove markers for other pairs so a converged machine leaves no litter.
find "$SESSION_LOG_DIR" -maxdepth 1 -type f -name 'drift-notified-*' ! -name "drift-notified-${safe_installed}-${safe_pin}" -exec rm -f {} + 2>/dev/null || true
: > "$marker" 2>/dev/null || true

printf '{"systemMessage":"%s","hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"%s"}}\n' \
    "$message" "$context"
exit 0
