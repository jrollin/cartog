#!/usr/bin/env bash
# cartog Benchmark Suite
#
# Compares cartog queries vs grep/cat approaches across 7 scenarios,
# measuring token efficiency, command count, and recall against ground truth.
#
# Usage:
#   ./benchmarks/run.sh                  # Run all scenarios (01–08)
#   ./benchmarks/run.sh --scenario 08    # Run single scenario
#   ./benchmarks/run.sh --fixture py     # Run only Python fixtures
#   ./benchmarks/run.sh --fixture rs     # Run only Rust fixtures
#   ./benchmarks/run.sh --fixture rb     # Run only Ruby fixtures

set -euo pipefail

export LC_NUMERIC=C

BENCH_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$BENCH_DIR/.." && pwd)"
RESULTS_DIR="$BENCH_DIR/results"
RESULTS_FILE="$RESULTS_DIR/latest.jsonl"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# Parse args
SCENARIO_FILTER=""
FIXTURE_FILTER=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --scenario) SCENARIO_FILTER="$2"; shift 2 ;;
        --fixture)  FIXTURE_FILTER="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 [--scenario NN] [--fixture py|rs|rb]"
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# ── Prerequisites ──

echo -e "${BOLD}=== cartog Benchmark Suite ===${NC}"
echo ""

# Check for jq (needed for ground truth comparison)
if ! command -v jq &>/dev/null; then
    echo -e "${YELLOW}Warning: jq not found. Ground truth recall will show 0.0%.${NC}"
    echo "Install jq for accurate recall measurement: brew install jq"
    echo ""
fi

# Check for cartog binary — honor CARTOG env if already set
if [ -n "${CARTOG:-}" ]; then
    echo -e "Using: $CARTOG (from env)"
elif ! command -v cartog &>/dev/null; then
    echo -e "${YELLOW}cartog not in PATH, building from source...${NC}"
    (cd "$PROJECT_ROOT" && cargo build --release 2>&1 | tail -1)
    export CARTOG="$PROJECT_ROOT/target/release/cartog"
    echo -e "${GREEN}Built: $CARTOG${NC}"
else
    export CARTOG="cartog"
    echo -e "Using: $(which cartog)"
fi
echo ""

# ── Index fixtures ──

echo -e "${BOLD}Indexing fixtures...${NC}"
for fixture_dir in "$BENCH_DIR"/fixtures/*/; do
    fixture_name=$(basename "$fixture_dir")

    # Apply fixture filter
    if [ -n "$FIXTURE_FILTER" ]; then
        case "$FIXTURE_FILTER" in
            py) [[ "$fixture_name" != *"_py"* ]] && continue ;;
            rs) [[ "$fixture_name" != *"_rs"* ]] && continue ;;
            rb) [[ "$fixture_name" != *"_rb"* ]] && continue ;;
        esac
    fi

    echo -n "  $fixture_name: "
    (cd "$fixture_dir" && $CARTOG index . --force 2>&1 | head -1)
done
echo ""

# ── Run scenarios ──

# Clear results
mkdir -p "$RESULTS_DIR"
> "$RESULTS_FILE"

echo -e "${BOLD}Running scenarios...${NC}"
printf "  ${BOLD}%-22s | %-27s | %-27s | %-27s | %s${NC}\n" \
    "Scenario" "Naive grep" "Best grep" "cartog" "Reduction"
echo "  $(printf '─%.0s' {1..120})"

for scenario_script in "$BENCH_DIR"/scenarios/*.sh; do
    scenario_num=$(basename "$scenario_script" | cut -d_ -f1)

    # Apply scenario filter
    if [ -n "$SCENARIO_FILTER" ] && [ "$scenario_num" != "$SCENARIO_FILTER" ]; then
        continue
    fi

    # Scenario stdout = JSON results → file; scenario stderr = table rows → terminal
    bash "$scenario_script" >> "$RESULTS_FILE"
done

echo ""

# ── Summary ──

echo -e "${BOLD}=== Summary ===${NC}"

if [ -s "$RESULTS_FILE" ] && command -v jq &>/dev/null; then
    total_naive=0
    total_best=0
    total_cartog=0
    total_naive_recall=0
    total_best_recall=0
    total_cartog_recall=0
    count=0

    while IFS= read -r line; do
        [ -z "$line" ] && continue
        nt=$(echo "$line" | jq -r '.naive_tokens // 0')
        bt=$(echo "$line" | jq -r '.best_tokens // 0')
        ct=$(echo "$line" | jq -r '.cartog_tokens // 0')
        nr=$(echo "$line" | jq -r '.naive_recall // 0')
        br=$(echo "$line" | jq -r '.best_recall // 0')
        cr=$(echo "$line" | jq -r '.cartog_recall // 0')

        total_naive=$((total_naive + nt))
        total_best=$((total_best + bt))
        total_cartog=$((total_cartog + ct))
        total_naive_recall=$(awk "BEGIN { print $total_naive_recall + $nr }")
        total_best_recall=$(awk "BEGIN { print $total_best_recall + $br }")
        total_cartog_recall=$(awk "BEGIN { print $total_cartog_recall + $cr }")
        count=$((count + 1))
    done < "$RESULTS_FILE"

    if [ "$count" -gt 0 ]; then
        avg_naive_recall=$(awk "BEGIN { printf \"%.1f\", $total_naive_recall / $count }")
        avg_best_recall=$(awk "BEGIN { printf \"%.1f\", $total_best_recall / $count }")
        avg_cartog_recall=$(awk "BEGIN { printf \"%.1f\", $total_cartog_recall / $count }")

        naive_reduction="n/a"
        best_reduction="n/a"
        if [ "$total_naive" -gt 0 ]; then
            naive_reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $total_cartog / $total_naive) * 100 }")
        fi
        if [ "$total_best" -gt 0 ]; then
            best_reduction=$(awk "BEGIN { printf \"%.1f\", (1 - $total_cartog / $total_best) * 100 }")
        fi

        echo ""
        echo "  Total tokens:  naive=$total_naive  best=$total_best  cartog=$total_cartog"
        echo "  Token reduction vs naive: ${naive_reduction}%"
        echo "  Token reduction vs best:  ${best_reduction}%"
        echo ""
        echo "  Avg recall:  naive=${avg_naive_recall}%  best=${avg_best_recall}%  cartog=${avg_cartog_recall}%"
        echo ""
        echo "  Results saved to: $RESULTS_FILE"
    fi
else
    echo "  No results collected. Check that cartog is installed and fixtures are indexed."
fi

echo ""
echo -e "${BOLD}Done.${NC}"
