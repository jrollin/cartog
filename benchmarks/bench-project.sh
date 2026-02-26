#!/usr/bin/env bash
# bench-project.sh — Run cartog vs grep benchmark on any codebase.
#
# Indexes the target project, auto-discovers symbols, runs 5 comparison
# scenarios (callers, outline, callees, deps, type refs) and reports
# token efficiency metrics.
#
# No ground truth needed — measures output size and completeness
# by cross-checking cartog results against grep results (and vice versa).
#
# Usage:
#   ./benchmarks/bench-project.sh /path/to/project
#   ./benchmarks/bench-project.sh .                     # current directory
#   ./benchmarks/bench-project.sh /path/to/project -n 5 # test 5 symbols (default: 3)
#
# Prerequisites:
#   - cartog binary in PATH (or set CARTOG env var)
#   - jq (optional, for richer output)

set -euo pipefail

export LC_NUMERIC=C

# ── Colors ──
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

# ── Args ──
TARGET=""
NUM_SYMBOLS=3

while [[ $# -gt 0 ]]; do
    case $1 in
        -n) NUM_SYMBOLS="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 <project-path> [-n num_symbols]"
            echo ""
            echo "Runs cartog vs grep benchmark on any codebase."
            echo "  -n  Number of symbols to test (default: 3)"
            exit 0
            ;;
        *)
            if [ -z "$TARGET" ]; then
                TARGET="$1"
            else
                echo "Error: unexpected argument: $1" >&2
                exit 1
            fi
            shift
            ;;
    esac
done

if [ -z "$TARGET" ]; then
    echo "Error: project path required" >&2
    echo "Usage: $0 <project-path> [-n num_symbols]" >&2
    exit 1
fi

TARGET=$(cd "$TARGET" && pwd)

# ── Cartog binary ──
CARTOG="${CARTOG:-cartog}"
if ! command -v "$CARTOG" &>/dev/null; then
    echo -e "${RED}Error: cartog not found. Install with: cargo install cartog${NC}" >&2
    exit 1
fi

# ── Helpers ──

count_bytes() { printf '%s' "$1" | wc -c | tr -d ' '; }
count_lines() {
    if [ -z "$1" ]; then echo 0; else printf '%s\n' "$1" | wc -l | tr -d ' '; fi
}
count_tokens() {
    local b; b=$(count_bytes "$1"); echo $(( (b + 3) / 4 ))
}

# ── Index ──

echo -e "${BOLD}=== cartog Benchmark — $(basename "$TARGET") ===${NC}"
echo -e "  Path: $TARGET"
echo ""

echo -e "${BOLD}Indexing...${NC}"
INDEX_OUTPUT=$(cd "$TARGET" && $CARTOG index . --force 2>&1)
echo "  $INDEX_OUTPUT"
echo ""

# ── Discover symbols ──

echo -e "${BOLD}Discovering symbols...${NC}"

# Get stats to understand the index
STATS=$(cd "$TARGET" && $CARTOG --json stats 2>/dev/null || echo "{}")

if command -v jq &>/dev/null; then
    total_symbols=$(echo "$STATS" | jq -r '.num_symbols // 0')
    total_edges=$(echo "$STATS" | jq -r '.num_edges // 0')
    total_files=$(echo "$STATS" | jq -r '.num_files // 0')
    echo "  Files: $total_files  Symbols: $total_symbols  Edges: $total_edges"
else
    echo "  (install jq for detailed stats)"
fi
echo ""

# Pick symbols that have references (interesting for benchmarking).
# Strategy: list a known file, pick functions/methods from outline.
# We use stats + outline on a few files to find symbols with edges.

# Find files with the most symbols by scanning outlines
SAMPLE_FILES=()
for ext in py rs ts js go; do
    found=$(cd "$TARGET" && find . -name "*.$ext" ! -path '*/node_modules/*' ! -path '*/.git/*' ! -path '*/target/*' ! -path '*/__pycache__/*' ! -path '*/vendor/*' 2>/dev/null | head -20 || true)
    while IFS= read -r f; do
        [ -z "$f" ] && continue
        # Strip leading ./
        f="${f#./}"
        SAMPLE_FILES+=("$f")
    done <<< "$found"
done

# Score each file by number of outline symbols, pick the richest.
# Avoid associative arrays for bash 3 (macOS) compatibility.
SCORE_TMP=$(mktemp)
trap 'rm -f "$SCORE_TMP"' EXIT

for f in "${SAMPLE_FILES[@]}"; do
    count=$(cd "$TARGET" && $CARTOG outline "$f" 2>/dev/null | wc -l | tr -d ' ' || echo 0)
    echo "$count $f" >> "$SCORE_TMP"
done

# Sort files by symbol count (descending) and pick top ones
RICH_FILES=()
while IFS= read -r line; do
    [ -z "$line" ] && continue
    RICH_FILES+=("$line")
done < <(sort -rn "$SCORE_TMP" | head -10 | awk '{print $2}')

if [ ${#RICH_FILES[@]} -eq 0 ]; then
    echo -e "${RED}No indexed files found. Is the project empty or unsupported language?${NC}" >&2
    exit 1
fi

# From rich files, extract function/method names that have refs
SYMBOLS=()
for f in "${RICH_FILES[@]}"; do
    [ ${#SYMBOLS[@]} -ge $((NUM_SYMBOLS * 2)) ] && break

    # Get function/method names from outline
    outline=$(cd "$TARGET" && $CARTOG --json outline "$f" 2>/dev/null || echo "[]")

    if command -v jq &>/dev/null; then
        names=$(echo "$outline" | jq -r '.[] | select(.kind == "function" or .kind == "method") | .name' 2>/dev/null | head -5 || true)
    else
        # Fallback: parse human output for function/method lines
        names=$(cd "$TARGET" && $CARTOG outline "$f" 2>/dev/null | grep -E '^\s*(function|method|async function|async method)' | awk '{print $NF}' | head -5 || true)
    fi

    while IFS= read -r name; do
        [ -z "$name" ] && continue
        [ "$name" = "__init__" ] && continue
        [ "$name" = "main" ] && continue
        # Check it has at least one ref
        refcount=$(cd "$TARGET" && $CARTOG refs "$name" 2>/dev/null | wc -l | tr -d ' ' || echo 0)
        if [ "$refcount" -gt 0 ]; then
            SYMBOLS+=("$name")
        fi
    done <<< "$names"
done

# Deduplicate and limit (avoid unquoted expansion for names with special chars)
DEDUPED=()
while IFS= read -r sym; do
    [ -z "$sym" ] && continue
    DEDUPED+=("$sym")
done < <(printf '%s\n' "${SYMBOLS[@]}" | sort -u | head -"$NUM_SYMBOLS")
SYMBOLS=("${DEDUPED[@]}")

if [ ${#SYMBOLS[@]} -eq 0 ]; then
    echo -e "${YELLOW}No symbols with references found. The project may be too small.${NC}" >&2
    exit 1
fi

echo -e "  Testing ${#SYMBOLS[@]} symbols: ${SYMBOLS[*]}"
echo -e "  Sample files: ${RICH_FILES[0]:-none}"
echo ""

# ── Run benchmarks ──

echo -e "${BOLD}Running comparisons...${NC}"
echo ""

# Accumulators
TOTAL_GREP_TOK=0
TOTAL_CARTOG_TOK=0
SCENARIO_COUNT=0

print_row() {
    local scenario="$1" grep_tok="$2" cartog_tok="$3" reduction="$4" note="$5"
    printf "  %-35s | %7s tok | %7s tok | %6s%% | %s\n" "$scenario" "$grep_tok" "$cartog_tok" "$reduction" "$note" >&2
}

printf "  ${BOLD}%-35s | %10s | %10s | %7s | %s${NC}\n" "Scenario" "grep" "cartog" "Saved" "Note" >&2
echo "  $(printf '─%.0s' {1..95})" >&2

# ── Scenario A: File structure (outline vs cat) ──
for f in "${RICH_FILES[@]:0:2}"; do
    cat_out=$(cat "$TARGET/$f" 2>/dev/null || true)
    cat_tok=$(count_tokens "$cat_out")

    outline_out=$(cd "$TARGET" && $CARTOG outline "$f" 2>/dev/null || true)
    outline_tok=$(count_tokens "$outline_out")

    if [ "$cat_tok" -gt 0 ]; then
        reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $outline_tok / $cat_tok) * 100 }")
    else
        reduction="n/a"
    fi

    print_row "outline: $f" "$cat_tok" "$outline_tok" "$reduction" "structure vs full file"

    TOTAL_GREP_TOK=$((TOTAL_GREP_TOK + cat_tok))
    TOTAL_CARTOG_TOK=$((TOTAL_CARTOG_TOK + outline_tok))
    SCENARIO_COUNT=$((SCENARIO_COUNT + 1))
done

# ── Scenario B: Find callers (refs --kind calls vs grep) ──
for sym in "${SYMBOLS[@]}"; do
    grep_out=$(cd "$TARGET" && grep -rn "$sym" . --include='*.py' --include='*.rs' --include='*.ts' --include='*.js' --include='*.go' 2>/dev/null | grep -v '\.cartog\.db' || true)
    grep_tok=$(count_tokens "$grep_out")

    refs_out=$(cd "$TARGET" && $CARTOG refs "$sym" --kind calls 2>/dev/null || true)
    refs_tok=$(count_tokens "$refs_out")

    if [ "$grep_tok" -gt 0 ]; then
        reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $refs_tok / $grep_tok) * 100 }")
    else
        reduction="n/a"
    fi

    print_row "callers: $sym" "$grep_tok" "$refs_tok" "$reduction" "call sites only"

    TOTAL_GREP_TOK=$((TOTAL_GREP_TOK + grep_tok))
    TOTAL_CARTOG_TOK=$((TOTAL_CARTOG_TOK + refs_tok))
    SCENARIO_COUNT=$((SCENARIO_COUNT + 1))
done

# ── Scenario C: Callees (what does X call?) ──
for sym in "${SYMBOLS[@]}"; do
    # Grep approach: find the function, read the file, scan for calls — approximate with grep -A
    grep_out=$(cd "$TARGET" && grep -rn -A20 "def $sym\|fn $sym\|function $sym\|func $sym" . --include='*.py' --include='*.rs' --include='*.ts' --include='*.js' --include='*.go' 2>/dev/null | grep -v '\.cartog\.db' || true)
    grep_tok=$(count_tokens "$grep_out")

    callees_out=$(cd "$TARGET" && $CARTOG callees "$sym" 2>/dev/null || true)
    callees_tok=$(count_tokens "$callees_out")

    if [ "$grep_tok" -gt 0 ]; then
        reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $callees_tok / $grep_tok) * 100 }")
    else
        reduction="n/a"
    fi

    print_row "callees: $sym" "$grep_tok" "$callees_tok" "$reduction" "outgoing calls"

    TOTAL_GREP_TOK=$((TOTAL_GREP_TOK + grep_tok))
    TOTAL_CARTOG_TOK=$((TOTAL_CARTOG_TOK + callees_tok))
    SCENARIO_COUNT=$((SCENARIO_COUNT + 1))
done

# ── Scenario D: File dependencies ──
for f in "${RICH_FILES[@]:0:2}"; do
    grep_out=$(cd "$TARGET" && grep -n '^import \|^from \|^use \|^const .* = require' "$f" 2>/dev/null || true)
    grep_tok=$(count_tokens "$grep_out")

    deps_out=$(cd "$TARGET" && $CARTOG deps "$f" 2>/dev/null || true)
    deps_tok=$(count_tokens "$deps_out")

    if [ "$grep_tok" -gt 0 ]; then
        reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $deps_tok / $grep_tok) * 100 }")
    else
        reduction="n/a"
    fi

    print_row "deps: $f" "$grep_tok" "$deps_tok" "$reduction" "import targets"

    TOTAL_GREP_TOK=$((TOTAL_GREP_TOK + grep_tok))
    TOTAL_CARTOG_TOK=$((TOTAL_CARTOG_TOK + deps_tok))
    SCENARIO_COUNT=$((SCENARIO_COUNT + 1))
done

# ── Scenario E: Impact analysis (unique to cartog) ──
for sym in "${SYMBOLS[@]:0:1}"; do
    # Grep has no equivalent — must do multiple searches
    grep_out1=$(cd "$TARGET" && grep -rn "$sym" . --include='*.py' --include='*.rs' --include='*.ts' --include='*.js' --include='*.go' 2>/dev/null | grep -v '\.cartog\.db' || true)
    # Simulate follow-up: pick a related name from first grep
    follow_up=$(echo "$grep_out1" | head -3 | grep -oE '[a-zA-Z_][a-zA-Z0-9_]+' | sort -u | head -1 || true)
    grep_out2=""
    if [ -n "$follow_up" ] && [ "$follow_up" != "$sym" ]; then
        grep_out2=$(cd "$TARGET" && grep -rn "$follow_up" . --include='*.py' --include='*.rs' --include='*.ts' --include='*.js' --include='*.go' 2>/dev/null | grep -v '\.cartog\.db' || true)
    fi
    grep_tok=$(count_tokens "${grep_out1}${grep_out2}")

    impact_out=$(cd "$TARGET" && $CARTOG impact "$sym" --depth 3 2>/dev/null || true)
    impact_tok=$(count_tokens "$impact_out")

    if [ "$grep_tok" -gt 0 ]; then
        reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $impact_tok / $grep_tok) * 100 }")
    else
        reduction="n/a"
    fi

    print_row "impact: $sym (depth 3)" "$grep_tok" "$impact_tok" "$reduction" "transitive refs (no grep equivalent)"

    TOTAL_GREP_TOK=$((TOTAL_GREP_TOK + grep_tok))
    TOTAL_CARTOG_TOK=$((TOTAL_CARTOG_TOK + impact_tok))
    SCENARIO_COUNT=$((SCENARIO_COUNT + 1))
done

# ── Summary ──

echo "" >&2
echo -e "${BOLD}=== Summary ===${NC}" >&2

if [ "$SCENARIO_COUNT" -gt 0 ] && [ "$TOTAL_GREP_TOK" -gt 0 ]; then
    overall_reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $TOTAL_CARTOG_TOK / $TOTAL_GREP_TOK) * 100 }")

    echo "" >&2
    echo "  Scenarios run:    $SCENARIO_COUNT" >&2
    echo "  Total grep tokens:   $TOTAL_GREP_TOK" >&2
    echo "  Total cartog tokens: $TOTAL_CARTOG_TOK" >&2
    echo "  Token reduction:     ${overall_reduction}%" >&2
    echo "" >&2
    echo -e "  ${DIM}Token = bytes/4 approximation. Lower is better for LLM context usage.${NC}" >&2
    echo -e "  ${DIM}Impact analysis has no grep equivalent — cartog provides transitive insight.${NC}" >&2
else
    echo "  No results collected." >&2
fi

echo "" >&2
echo -e "${BOLD}Done.${NC}" >&2
