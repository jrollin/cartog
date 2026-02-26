#!/usr/bin/env bash
# Ground truth comparison helpers.
# Source this file from scenario scripts.

set -euo pipefail

export LC_NUMERIC=C

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Extract expected items from ground truth JSON for a scenario.
# Usage: get_expected_items <fixture_name> <scenario_key> <json_path>
# Example: get_expected_items webapp_py 01_find_callers '.expected[].source'
get_expected_items() {
    local fixture="$1"
    local scenario="$2"
    local jq_path="$3"

    local gt_file="$BENCH_DIR/ground_truth/${fixture}.json"
    if [ ! -f "$gt_file" ]; then
        echo ""
        return
    fi

    jq -r ".\"${scenario}\"${jq_path}" "$gt_file" 2>/dev/null || echo ""
}

# Count expected items for a scenario.
# Usage: get_expected_count <fixture_name> <scenario_key> <json_path>
get_expected_count() {
    local fixture="$1"
    local scenario="$2"
    local jq_path="$3"

    local gt_file="$BENCH_DIR/ground_truth/${fixture}.json"
    if [ ! -f "$gt_file" ]; then
        echo 0
        return
    fi

    jq -r ".\"${scenario}\"${jq_path} | length" "$gt_file" 2>/dev/null || echo 0
}

# Check recall: count how many expected items appear in given output.
# Usage: check_recall <output> <fixture_name> <scenario_key> <jq_items_path>
# Returns: "found/total recall%"
check_recall() {
    local output="$1"
    local fixture="$2"
    local scenario="$3"
    local jq_path="$4"

    local items
    items=$(get_expected_items "$fixture" "$scenario" "$jq_path")

    if [ -z "$items" ]; then
        echo "0 0 0.0"
        return
    fi

    local total=0
    local found=0

    while IFS= read -r item; do
        [ -z "$item" ] && continue
        total=$((total + 1))
        if echo "$output" | grep -q "$item"; then
            found=$((found + 1))
        fi
    done <<< "$items"

    if [ "$total" -eq 0 ]; then
        echo "0 0 100.0"
    else
        local recall
        recall=$(awk "BEGIN { printf \"%.1f\", ($found / $total) * 100 }")
        echo "$found $total $recall"
    fi
}
