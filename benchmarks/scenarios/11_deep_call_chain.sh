#!/usr/bin/env bash
# Scenario 11: Deep call chain (5+ hops)
# Question: "Trace handle_login → authenticate → login → generate_token → execute_query → get_connection"
#
# Tests: sequential callees queries for a 5-hop call chain
# Key differentiator: cartog can follow 5 hops precisely; grep requires 5+ rounds
# with increasing noise and false positives at each step.

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="11_deep_call_chain"

run_scenario() {
    local fixture_name="$1"
    shift
    local -a chain=("$@")  # 6 function names forming the chain

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} Deep call chain (${#chain[@]} hops) from ${chain[0]}" >&2

    # ── Naive grep: search for each function name ──
    local naive_out=""
    local naive_cmds=${#chain[@]}
    for fn in "${chain[@]}"; do
        local out
        out=$(cd "$fixture_dir" && grep -rn "$fn" . 2>/dev/null || true)
        naive_out="${naive_out}${out}"
    done
    local naive_tok=$(count_tokens "$naive_out")

    # ── Best-effort grep: search for call sites only ──
    local best_out=""
    local best_cmds=${#chain[@]}
    for fn in "${chain[@]}"; do
        local out
        out=$(cd "$fixture_dir" && grep -rn "${fn}(" . 2>/dev/null | grep -v "def \|fn \|func \|function " || true)
        best_out="${best_out}${out}"
    done
    local best_tok=$(count_tokens "$best_out")

    # ── Cartog: sequential callees for each hop ──
    local cartog_out=""
    local cartog_json_out=""
    local cartog_cmds=${#chain[@]}
    for fn in "${chain[@]}"; do
        local c cj
        c=$(cd "$fixture_dir" && $CARTOG callees "$fn" 2>/dev/null || true)
        cj=$(cd "$fixture_dir" && $CARTOG --json callees "$fn" 2>/dev/null || true)
        cartog_out="${cartog_out}${c}"
        cartog_json_out="${cartog_json_out}${cj}"
    done
    local cartog_tok=$(count_tokens "$cartog_out")

    # ── Recall: check expected chain links ──
    local naive_recall best_recall cartog_recall
    read -r _ _ naive_recall <<< "$(check_recall "$naive_out" "$fixture_name" "$SCENARIO" '.expected_chain[].callee')"
    read -r _ _ best_recall <<< "$(check_recall "$best_out" "$fixture_name" "$SCENARIO" '.expected_chain[].callee')"
    read -r _ _ cartog_recall <<< "$(check_recall "$cartog_json_out" "$fixture_name" "$SCENARIO" '.expected_chain[].callee')"

    print_comparison "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall"

    emit_result_json "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_recall" \
        "$best_tok" "$best_recall" \
        "$cartog_tok" "$cartog_recall"
}

run_scenario "webapp_py" "handle_login" "authenticate" "login" "generate_token" "execute_query" "get_connection"
run_scenario "webapp_ts" "handleLogin" "authenticate" "login" "generateToken" "executeQuery" "getConnection"
run_scenario "webapp_go" "HandleLogin" "Authenticate" "Login" "GenerateToken" "ExecuteQuery" "GetConnection"
run_scenario "webapp_rs" "handle_login" "authenticate" "login" "generate_token" "execute_query" "get_connection"
run_scenario "webapp_rb" "handle_login" "authenticate" "login" "generate_token" "execute_query" "get_connection"
