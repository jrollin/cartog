#!/usr/bin/env bash
# Scenario 05: Trace a call chain
# Question: "Trace login_route/login_handler → ... → DB"
#
# Tests: sequential callees queries vs multiple grep rounds
# Key differentiator: cartog needs 2-3 targeted calls; grep needs 3-5+ and may miss links

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="05_trace_call_chain"

run_scenario() {
    local fixture_name="$1"
    local entry_fn="$2"
    local mid_fn="$3"
    local leaf_fn="$4"

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    echo -e "  ${CYAN}[$fixture_name]${NC} Trace call chain from $entry_fn" >&2

    # ── Naive grep: search for each function name in sequence ──
    local out1 out2 out3
    out1=$(cd "$fixture_dir" && grep -rn "$entry_fn" . 2>/dev/null || true)
    out2=$(cd "$fixture_dir" && grep -rn "$mid_fn" . 2>/dev/null || true)
    out3=$(cd "$fixture_dir" && grep -rn "$leaf_fn" . 2>/dev/null || true)
    local naive_out="${out1}${out2}${out3}"
    local naive_tok=$(count_tokens "$naive_out")
    local naive_cmds=3

    # ── Best-effort grep: targeted search for call sites ──
    out1=$(cd "$fixture_dir" && grep -rn "${entry_fn}\|${mid_fn}(" . 2>/dev/null | grep -v "def \|fn " || true)
    out2=$(cd "$fixture_dir" && grep -rn "${mid_fn}\|${leaf_fn}(" . 2>/dev/null | grep -v "def \|fn " || true)
    out3=$(cd "$fixture_dir" && grep -rn "${leaf_fn}(" . 2>/dev/null | grep -v "def \|fn " || true)
    local best_out="${out1}${out2}${out3}"
    local best_tok=$(count_tokens "$best_out")
    local best_cmds=3

    # ── Cartog: sequential callees (human-readable for tokens) ──
    local c1 c2 c3 cj1 cj2 cj3
    c1=$(cd "$fixture_dir" && $CARTOG callees "$entry_fn" 2>/dev/null || true)
    c2=$(cd "$fixture_dir" && $CARTOG callees "$mid_fn" 2>/dev/null || true)
    c3=$(cd "$fixture_dir" && $CARTOG callees "$leaf_fn" 2>/dev/null || true)
    local cartog_out="${c1}${c2}${c3}"
    local cartog_tok=$(count_tokens "$cartog_out")
    local cartog_cmds=3

    # JSON for recall checking
    cj1=$(cd "$fixture_dir" && $CARTOG --json callees "$entry_fn" 2>/dev/null || true)
    cj2=$(cd "$fixture_dir" && $CARTOG --json callees "$mid_fn" 2>/dev/null || true)
    cj3=$(cd "$fixture_dir" && $CARTOG --json callees "$leaf_fn" 2>/dev/null || true)
    local cartog_json_out="${cj1}${cj2}${cj3}"

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

run_scenario "webapp_py" "login_route" "login" "generate_token"
run_scenario "webapp_rs" "login_handler" "login" "generate_token"
run_scenario "webapp_rb" "login_route" "login" "generate_token"
