#!/usr/bin/env bash
# Scenario 01: Find callers of validate_token
# Question: "Who calls validate_token?"
#
# Tests: refs query with --kind calls filter
# Measures: token output, command count, recall against ground truth

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="01_find_callers"
SYMBOL="validate_token"

run_scenario() {
    local fixture_name="$1"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"
    local ext="$2"

    echo -e "  ${CYAN}[$fixture_name]${NC} Who calls $SYMBOL?" >&2

    # ── Naive grep: simple string search with context ──
    run_grep "naive" "$fixture_dir" "grep -rn -B3 '$SYMBOL' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter to call sites with context ──
    run_grep "best" "$fixture_dir" "grep -rn -B3 '${SYMBOL}(' . | grep -v 'def ${SYMBOL}\|fn ${SYMBOL}\|pub fn ${SYMBOL}'"
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog ──
    run_cartog_cmd "$fixture_dir" refs "$SYMBOL" --kind calls
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall against ground truth ──
    local naive_recall best_recall cartog_recall
    read -r _ _ naive_recall <<< "$(check_recall "$naive_out" "$fixture_name" "$SCENARIO" '.expected[].source')"
    read -r _ _ best_recall <<< "$(check_recall "$best_out" "$fixture_name" "$SCENARIO" '.expected[].source')"
    read -r _ _ cartog_recall <<< "$(check_recall "$CARTOG_JSON_OUTPUT" "$fixture_name" "$SCENARIO" '.expected[].source')"

    print_comparison "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall"

    emit_result_json "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_recall" \
        "$best_tok" "$best_recall" \
        "$cartog_tok" "$cartog_recall"
}

run_scenario "webapp_py" "py"
run_scenario "webapp_rs" "rs"
run_scenario "webapp_rb" "rb"
