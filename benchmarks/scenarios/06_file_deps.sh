#!/usr/bin/env bash
# Scenario 06: File-level dependencies
# Question: "What does routes/auth.{py,rs} import?"
#
# Tests: deps query vs grep for import/from/use keywords
# Key differentiator: cartog returns resolved import targets

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="06_file_deps"

run_scenario() {
    local fixture_name="$1"
    local file="$2"
    local import_pattern="$3"

    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} Dependencies of $file?" >&2

    # ── Naive grep: search for import keywords ──
    run_grep "naive" "$fixture_dir" "grep -n '$import_pattern' '$file'"
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: same (import lines are fairly self-documenting) ──
    run_grep "best" "$fixture_dir" "grep -n '$import_pattern' '$file'"
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog ──
    run_cartog_cmd "$fixture_dir" deps "$file"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall ──
    local naive_recall best_recall cartog_recall
    read -r _ _ naive_recall <<< "$(check_recall "$naive_out" "$fixture_name" "$SCENARIO" '.expected[]')"
    read -r _ _ best_recall <<< "$(check_recall "$best_out" "$fixture_name" "$SCENARIO" '.expected[]')"
    read -r _ _ cartog_recall <<< "$(check_recall "$CARTOG_JSON_OUTPUT" "$fixture_name" "$SCENARIO" '.expected[]')"

    print_comparison "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_cmds" "$naive_recall" \
        "$best_tok" "$best_cmds" "$best_recall" \
        "$cartog_tok" "$cartog_cmds" "$cartog_recall"

    emit_result_json "$SCENARIO" "$fixture_name" \
        "$naive_tok" "$naive_recall" \
        "$best_tok" "$best_recall" \
        "$cartog_tok" "$cartog_recall"
}

run_scenario "webapp_py" "routes/auth.py" "^from \|^import "
run_scenario "webapp_ts" "src/routes/auth.ts" "^import "
run_scenario "webapp_go" "internal/routes/auth_routes.go" '"webapp_go/'
run_scenario "webapp_rs" "routes/auth.rs" "^use "
run_scenario "webapp_rb" "routes/auth.rb" "^require"
