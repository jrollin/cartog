#!/usr/bin/env bash
# Scenario 10: High-fanout utility function
# Question: "Who calls get_logger / getLogger / GetLogger?"
#
# Tests: refs query on a utility called from 25+ files
# Key differentiator: cartog returns precise call sites; grep returns every
# mention including imports, string literals, and comments.

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="10_high_fanout"

run_scenario() {
    local fixture_name="$1"
    local symbol="$2"

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} High-fanout callers of $symbol" >&2

    # ── Naive grep: every occurrence ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter to call sites ──
    run_grep "best" "$fixture_dir" "grep -rn '${symbol}(' . | grep -v 'def ${symbol}\|fn ${symbol}\|func ${symbol}\|function ${symbol}'"
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog: all refs (imports + calls) — high-fanout utility ──
    run_cartog_cmd "$fixture_dir" refs "$symbol"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall: check expected callers ──
    local naive_recall best_recall cartog_recall
    read -r _ _ naive_recall <<< "$(check_recall "$naive_out" "$fixture_name" "$SCENARIO" '.expected[].file')"
    read -r _ _ best_recall <<< "$(check_recall "$best_out" "$fixture_name" "$SCENARIO" '.expected[].file')"
    read -r _ _ cartog_recall <<< "$(check_recall "$CARTOG_JSON_OUTPUT" "$fixture_name" "$SCENARIO" '.expected[].file')"

    print_comparison "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall"

    emit_result_json "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_recall" \
        "$best_tok" "$best_recall" \
        "$cartog_tok" "$cartog_recall"
}

run_scenario "webapp_py" "get_logger"
run_scenario "webapp_ts" "getLogger"
run_scenario "webapp_go" "GetLogger"
run_scenario "webapp_rs" "get_logger"
run_scenario "webapp_rb" "get_logger"
