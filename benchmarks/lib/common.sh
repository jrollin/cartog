#!/usr/bin/env bash
# Shared functions for benchmark scenarios.
# Source this file from scenario scripts.

set -euo pipefail

export LC_NUMERIC=C

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARTOG="${CARTOG:-cartog}"
RESULTS=()

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# ── Fixture filter ──

# Check if a fixture should be skipped based on FIXTURE_FILTER env var.
# Usage: should_skip_fixture <fixture_name>
# Returns 0 (true = skip) if fixture doesn't match filter, 1 (false = run) otherwise.
should_skip_fixture() {
    local fixture_name="$1"
    if [ -z "${FIXTURE_FILTER:-}" ]; then
        return 1
    fi
    case "$FIXTURE_FILTER" in
        py) [[ "$fixture_name" != *"_py"* ]] && return 0 || return 1 ;;
        ts) [[ "$fixture_name" != *"_ts"* ]] && return 0 || return 1 ;;
        go) [[ "$fixture_name" != *"_go"* ]] && return 0 || return 1 ;;
        rs) [[ "$fixture_name" != *"_rs"* ]] && return 0 || return 1 ;;
        rb) [[ "$fixture_name" != *"_rb"* ]] && return 0 || return 1 ;;
        *)  return 1 ;;
    esac
}

# ── Measurement helpers ──

count_bytes() {
    printf '%s' "$1" | wc -c | tr -d ' '
}

count_lines() {
    if [ -z "$1" ]; then
        echo 0
    else
        printf '%s\n' "$1" | wc -l | tr -d ' '
    fi
}

# Approximate token count (bytes / 4 is a common heuristic for English/code)
count_tokens() {
    local bytes
    bytes=$(count_bytes "$1")
    echo $(( (bytes + 3) / 4 ))
}

# ── Run approaches ──

# Run a grep command and capture output + metrics.
# Usage: run_grep <label> <fixture_dir> <command...>
# Sets: GREP_OUTPUT, GREP_TOKENS, GREP_LINES
run_grep() {
    local label="$1"
    local fixture_dir="$2"
    shift 2

    GREP_OUTPUT=$(cd "$fixture_dir" && eval "$@" 2>/dev/null | grep -v '\.cartog\.db' || true)
    GREP_TOKENS=$(count_tokens "$GREP_OUTPUT")
    GREP_LINES=$(count_lines "$GREP_OUTPUT")
}

# Run a cartog command (human-readable) and capture output + metrics.
# Usage: run_cartog_cmd <fixture_dir> <cartog_args...>
# Sets: CARTOG_OUTPUT, CARTOG_TOKENS, CARTOG_LINES
#        CARTOG_JSON_OUTPUT (JSON variant for recall checking)
run_cartog_cmd() {
    local fixture_dir="$1"
    shift

    # Human-readable output for token comparison (what agent actually processes)
    CARTOG_OUTPUT=$(cd "$fixture_dir" && $CARTOG "$@" 2>/dev/null || true)
    CARTOG_TOKENS=$(count_tokens "$CARTOG_OUTPUT")
    CARTOG_LINES=$(count_lines "$CARTOG_OUTPUT")

    # JSON output for recall checking (names are more reliably extractable)
    CARTOG_JSON_OUTPUT=$(cd "$fixture_dir" && $CARTOG --json "$@" 2>/dev/null || true)
}

# Run a cat command (simulating "read entire file") and capture metrics.
# Usage: run_cat <fixture_dir> <file>
# Sets: CAT_OUTPUT, CAT_TOKENS, CAT_LINES
run_cat() {
    local fixture_dir="$1"
    local file="$2"

    CAT_OUTPUT=$(cat "$fixture_dir/$file" 2>/dev/null || true)
    CAT_TOKENS=$(count_tokens "$CAT_OUTPUT")
    CAT_LINES=$(count_lines "$CAT_OUTPUT")
}

# ── Recall computation ──

# Count how many expected items appear in output.
# Usage: count_matches <output> <item1> <item2> ...
# Returns: count of found items
count_matches() {
    local output="$1"
    shift
    local found=0
    for item in "$@"; do
        if echo "$output" | grep -c "$item" >/dev/null 2>&1; then
            found=$((found + 1))
        fi
    done
    echo $found
}

# Compute recall as percentage.
# Usage: compute_recall <found> <total>
compute_recall() {
    local found="$1"
    local total="$2"
    if [ "$total" -eq 0 ]; then
        echo "100.0"
    else
        awk "BEGIN { printf \"%.1f\", ($found / $total) * 100 }"
    fi
}

# ── Result collection ──

# Append a result row.
# Usage: add_result <scenario> <lang> <approach> <tokens> <commands> <recall>
add_result() {
    local scenario="$1"
    local lang="$2"
    local approach="$3"
    local tokens="$4"
    local commands="$5"
    local recall="$6"

    RESULTS+=("${scenario}|${lang}|${approach}|${tokens}|${commands}|${recall}")
}

# Print a single scenario comparison for one language.
# Usage: print_comparison <scenario> <lang> <naive_tok> <naive_cmds> <naive_recall> \
#                          <best_tok> <best_cmds> <best_recall> \
#                          <cartog_tok> <cartog_cmds> <cartog_recall>
print_comparison() {
    local scenario="$1" lang="$2"
    local naive_tok="$3" naive_cmds="$4" naive_recall="$5"
    local best_tok="$6" best_cmds="$7" best_recall="$8"
    local cartog_tok="$9" cartog_cmds="${10}" cartog_recall="${11}"

    local reduction="n/a"
    if [ "$naive_tok" -gt 0 ] 2>/dev/null; then
        reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $cartog_tok / $naive_tok) * 100 }")
    fi

    printf "  %-22s | %6s tok (%s cmd) %5s%% | %6s tok (%s cmd) %5s%% | %6s tok (%s cmd) %5s%% | %5s%%\n" \
        "$scenario" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall" \
        "$reduction" >&2
}

# ── JSON output for results file ──

emit_result_json() {
    local scenario="$1" lang="$2"
    local naive_tok="$3" naive_recall="$4"
    local best_tok="$5" best_recall="$6"
    local cartog_tok="$7" cartog_recall="$8"

    cat <<EOF
{"scenario":"${scenario}","lang":"${lang}","naive_tokens":${naive_tok},"naive_recall":${naive_recall},"best_tokens":${best_tok},"best_recall":${best_recall},"cartog_tokens":${cartog_tok},"cartog_recall":${cartog_recall}}
EOF
}
