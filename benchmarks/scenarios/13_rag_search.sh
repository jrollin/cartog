#!/usr/bin/env bash
# Scenario 13: Semantic search — find code by concept/behavior
# Question: "Find code related to token validation"
#
# Tests: rag search (FTS5 hybrid) vs grep for conceptual queries
# Measures: token output, command count, recall against ground truth
#
# Key differentiator: cartog rag search matches by content (FTS5 + vector),
# returning structured results. grep returns raw lines, mixing definitions,
# usages, string literals, and comments.

source "$(dirname "$0")/../lib/common.sh"
source "$(dirname "$0")/../lib/compare.sh"

SCENARIO="13_rag_search"
QUERY="validate token"

# Ensure embedding model is downloaded (one-time, ~133MB)
ensure_rag_model() {
    if ! $CARTOG rag setup 2>/dev/null | grep -q "already"; then
        echo -e "  ${YELLOW}Downloading embedding model...${NC}" >&2
        $CARTOG rag setup >/dev/null 2>&1
    fi
}

ensure_rag_model

run_scenario() {
    local fixture_name="$1"
    local fixture_dir="$BENCH_DIR/fixtures/$fixture_name"

    should_skip_fixture "$fixture_name" && return 0

    # RAG requires symbol content (populated during index) + embeddings.
    # Re-index to ensure symbol_content is populated, then build RAG index.
    (cd "$fixture_dir" && $CARTOG index . --force >/dev/null 2>&1) || true
    (cd "$fixture_dir" && $CARTOG rag index >/dev/null 2>&1) || true

    echo -e "  ${CYAN}[$fixture_name]${NC} RAG search: '$QUERY'" >&2

    # ── Naive grep: raw string search for both terms ──
    run_grep "naive" "$fixture_dir" "grep -rn 'validate\|token' . --include='*.*'"
    local naive_tok=$GREP_TOKENS
    local naive_out="$GREP_OUTPUT"
    local naive_cmds=1

    # ── Best-effort grep: both terms on same line, unique files ──
    run_grep "best" "$fixture_dir" "grep -rln 'validate' . --include='*.*' | xargs grep -l 'token' 2>/dev/null | head -20"
    local best_tok=$GREP_TOKENS
    local best_out="$GREP_OUTPUT"
    local best_cmds=2

    # ── Cartog rag search ──
    run_cartog_cmd "$fixture_dir" rag search "$QUERY"
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
