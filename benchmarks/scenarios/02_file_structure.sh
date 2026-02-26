#!/usr/bin/env bash
# Scenario 02: Understand file structure
# Question: "What's in auth/service.{py,rs}?"
#
# Tests: outline query vs cat (reading entire file)
# Measures: token output size — outline gives structure without implementation

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="02_file_structure"

run_scenario() {
    local fixture_name="$1"
    local file="$2"

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    echo -e "  ${CYAN}[$fixture_name]${NC} What's in $file?" >&2

    # ── Naive: cat the entire file ──
    run_cat "$fixture_dir" "$file"
    local naive_tok=$CAT_TOKENS
    local naive_out="$CAT_OUTPUT"
    local naive_cmds=1

    # ── Best-effort: grep for def/class/fn/struct/trait lines ──
    run_grep "best" "$fixture_dir" "grep -n 'def \|class \|fn \|struct \|trait \|impl \|pub \|enum ' '$file'"
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog ──
    run_cartog_cmd "$fixture_dir" outline "$file"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall: check if key symbols appear ──
    local naive_recall best_recall cartog_recall
    read -r _ _ naive_recall <<< "$(check_recall "$naive_out" "$fixture_name" "$SCENARIO" '.expected[].name')"
    read -r _ _ best_recall <<< "$(check_recall "$best_out" "$fixture_name" "$SCENARIO" '.expected[].name')"
    read -r _ _ cartog_recall <<< "$(check_recall "$CARTOG_JSON_OUTPUT" "$fixture_name" "$SCENARIO" '.expected[].name')"

    print_comparison "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall"

    emit_result_json "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_recall" \
        "$best_tok" "$best_recall" \
        "$cartog_tok" "$cartog_recall"
}

run_scenario "webapp_py" "auth/service.py"
run_scenario "webapp_rs" "auth/service.rs"
