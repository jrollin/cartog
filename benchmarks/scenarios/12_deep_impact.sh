#!/usr/bin/env bash
# Scenario 12: Deep transitive impact analysis (depth 5)
# Question: "What breaks if DatabaseConnection changes?"
#
# Tests: impact query at depth 5 — finds transitive dependents
# Key differentiator: cartog BFS walks the graph transitively;
# grep can only find direct string matches.

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="12_deep_impact"

run_scenario() {
    local fixture_name="$1"
    local symbol="$2"

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} Deep impact of $symbol (depth 5)" >&2

    # ── Naive grep: search for the name ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: search for symbol + known dependents ──
    run_grep "best" "$fixture_dir" "grep -rn '$symbol\|execute_query\|executeQuery\|ExecuteQuery' ."
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=2

    # ── Cartog: deep transitive impact ──
    run_cartog_cmd "$fixture_dir" impact "$symbol" --depth 5
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall: check expected transitive dependents ──
    local naive_recall best_recall cartog_recall
    read -r _ _ naive_recall <<< "$(check_recall "$naive_out" "$fixture_name" "$SCENARIO" '.expected_refs[]')"
    read -r _ _ best_recall <<< "$(check_recall "$best_out" "$fixture_name" "$SCENARIO" '.expected_refs[]')"
    read -r _ _ cartog_recall <<< "$(check_recall "$CARTOG_JSON_OUTPUT" "$fixture_name" "$SCENARIO" '.expected_refs[]')"

    print_comparison "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall"

    emit_result_json "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_recall" \
        "$best_tok" "$best_recall" \
        "$cartog_tok" "$cartog_recall"
}

run_scenario "webapp_py" "DatabaseConnection"
run_scenario "webapp_ts" "DatabaseConnection"
run_scenario "webapp_go" "DatabaseConnection"
run_scenario "webapp_rs" "DatabaseConnection"
run_scenario "webapp_rb" "DatabaseConnection"
