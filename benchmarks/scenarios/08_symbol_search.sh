#!/usr/bin/env bash
# Scenario 08: Discover symbols matching a partial name
# Question: "Find all token-related symbols"
#
# Tests: search query (prefix + substring match)
# Measures: token output, command count, recall against ground truth
#
# Key differentiator: cartog returns only symbol names + locations (structured).
# grep returns raw source lines with noise — definitions, usages, string literals mixed.

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="08_symbol_search"
QUERY="token"

run_scenario() {
    local fixture_name="$1"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} Find symbols matching '$QUERY'" >&2

    # ── Naive grep: raw string search, returns every occurrence ──
    run_grep "naive" "$fixture_dir" "grep -rn '$QUERY' . --include='*.*'"
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: unique identifiers only (word boundary, no duplicates) ──
    run_grep "best" "$fixture_dir" "grep -roh '\b\w*[Tt]oken\w*\b' . --include='*.*' | sed 's/.*://' | sort -u"
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog ──
    run_cartog_cmd "$fixture_dir" search "$QUERY"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall against ground truth ──
    local naive_recall best_recall cartog_recall
    read -r _ _ naive_recall <<< "$(check_recall "$naive_out" "$fixture_name" "$SCENARIO" '.expected[].name')"
    read -r _ _ best_recall  <<< "$(check_recall "$best_out"  "$fixture_name" "$SCENARIO" '.expected[].name')"
    read -r _ _ cartog_recall <<< "$(check_recall "$cartog_out" "$fixture_name" "$SCENARIO" '.expected[].name')"

    print_comparison "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall"

    emit_result_json "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_recall" \
        "$best_tok" "$best_recall" \
        "$cartog_tok" "$cartog_recall"
}

run_scenario "webapp_py"
run_scenario "webapp_ts"
run_scenario "webapp_go"
run_scenario "webapp_rs"
run_scenario "webapp_rb"
