#!/usr/bin/env bash
# Scenario 07: Type references
# Question (Python): "Who references TokenError?" (inherits, raises, except)
# Question (Rust):   "Who references User in type annotations?" >&2
#
# Tests: refs query with --kind references/inherits vs grep
# Key differentiator: cartog distinguishes reference KIND (inherits vs raises vs type annotation)
# grep finds the string but can't tell if it's a type annotation, exception, or just a comment

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="07_type_references"

run_scenario_py() {
    local fixture_name="webapp_py"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"
    local symbol="TokenError"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} References to $symbol (inherits, raises, except)?" >&2

    # ── Naive grep ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter by context ──
    run_grep "best" "$fixture_dir" "grep -rn 'class.*($symbol)\|raise $symbol\|except $symbol\|except.*$symbol' ."
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog: all refs to TokenError ──
    run_cartog_cmd "$fixture_dir" refs "$symbol"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall ──
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

run_scenario_rs() {
    local fixture_name="webapp_rs"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"
    local symbol="User"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} Type annotation references to $symbol?" >&2

    # ── Naive grep ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter to type contexts ──
    run_grep "best" "$fixture_dir" "grep -rn ': .*$symbol\|-> .*$symbol\|<$symbol>\|&$symbol' ."
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog: type references only ──
    run_cartog_cmd "$fixture_dir" refs "$symbol" --kind references
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall ──
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

run_scenario_rb() {
    local fixture_name="webapp_rb"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"
    local symbol="TokenError"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} References to $symbol (inherits, raises, rescue)?" >&2

    # ── Naive grep ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter by context ──
    run_grep "best" "$fixture_dir" "grep -rn 'class.*<.*$symbol\|raise $symbol\|rescue $symbol\|rescue.*$symbol' ."
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog: all refs to TokenError ──
    run_cartog_cmd "$fixture_dir" refs "$symbol"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall ──
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

run_scenario_ts() {
    local fixture_name="webapp_ts"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"
    local symbol="TokenError"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} References to $symbol (inherits, throws, catch)?" >&2

    # ── Naive grep ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter by context ──
    run_grep "best" "$fixture_dir" "grep -rn 'class.*extends.*$symbol\|throw new $symbol\|catch.*$symbol\|instanceof $symbol' ."
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog: all refs to TokenError ──
    run_cartog_cmd "$fixture_dir" refs "$symbol"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall ──
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

run_scenario_go() {
    local fixture_name="webapp_go"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"
    local symbol="TokenError"

    should_skip_fixture "$fixture_name" && return 0

    echo -e "  ${CYAN}[$fixture_name]${NC} References to $symbol (type assertions, error checks)?" >&2

    # ── Naive grep ──
    run_grep "naive" "$fixture_dir" "grep -rn '$symbol' ."
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: filter by context ──
    run_grep "best" "$fixture_dir" "grep -rn '&$symbol\|$symbol{\|\.($symbol)\|errors\.As.*$symbol' ."
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=1

    # ── Cartog: all refs to TokenError ──
    run_cartog_cmd "$fixture_dir" refs "$symbol"
    local cartog_tok=$CARTOG_TOKENS
    local cartog_out="$CARTOG_OUTPUT"
    local cartog_cmds=1

    # ── Recall ──
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

run_scenario_py
run_scenario_ts
run_scenario_go
run_scenario_rs
run_scenario_rb
