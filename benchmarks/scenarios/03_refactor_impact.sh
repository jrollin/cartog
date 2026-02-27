#!/usr/bin/env bash
# Scenario 03: Assess refactoring impact
# Question: "Is it safe to rename AuthService / AuthProvider?"
#
# Tests: impact query (transitive BFS) vs grep (flat string match)
# Key differentiator: cartog finds TRANSITIVE dependencies grep cannot

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="03_refactor_impact"

run_scenario() {
    local fixture_name="$1"
    local symbol="$2"

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} Impact of changing $symbol?" >&2

    # ── Naive grep: just search for the name ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort: search for symbol + known subclasses/implementors ──
    run_grep "best" "$fixture_dir" "grep -rn '$symbol\|AdminService\|DefaultAuth' ."
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=2

    # ── Cartog ──
    run_cartog_cmd "$fixture_dir" impact "$symbol" --depth 3
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall: check transitive refs ──
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

run_scenario "webapp_py" "AuthService"
run_scenario "webapp_ts" "AuthService"
run_scenario "webapp_go" "AuthService"
run_scenario "webapp_rs" "AuthProvider"
run_scenario "webapp_rb" "AuthService"
