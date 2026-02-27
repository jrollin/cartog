#!/usr/bin/env bash
# Scenario 09: Ambiguous symbol resolution
# Question: "Where is 'validate' defined — and which one do callers mean?"
#
# Tests: search + refs for a symbol that exists in 4+ files
# Key differentiator: cartog returns distinct symbols with file locations;
# grep returns a wall of matches across definitions AND usages.

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="09_ambiguous_symbol"

run_scenario() {
    local fixture_name="$1"
    local symbol="$2"

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} Disambiguate '$symbol' (defined in 4+ files)" >&2

    # ── Naive grep: raw search returns everything ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter to definitions only ──
    run_grep "best" "$fixture_dir" "grep -rn 'def $symbol\|fn $symbol\|func $symbol\|function $symbol\|$symbol(' . | head -20"
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog: search returns definitions with file paths ──
    run_cartog_cmd "$fixture_dir" search "$symbol"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall: check expected definition files ──
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

run_scenario "webapp_py" "validate"
run_scenario "webapp_ts" "validate"
run_scenario "webapp_go" "Validate"
run_scenario "webapp_rs" "validate"
run_scenario "webapp_rb" "validate"
