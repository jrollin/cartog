#!/usr/bin/env bash
# Helpers for the agent-task benchmark: parse stream-json usage, compute medians.

set -euo pipefail

export LC_NUMERIC=C

# Colors, used by run.sh which sources this file (shellcheck can't see cross-file).
# shellcheck disable=SC2034
RED='\033[0;31m'
# shellcheck disable=SC2034
YELLOW='\033[0;33m'
# shellcheck disable=SC2034
BOLD='\033[1m'
# shellcheck disable=SC2034
NC='\033[0m'

# MCP config pinning cartog to the prebuilt fixture index (no re-index mid-run).
# Usage: write_mcp_config <cartog_bin> <fixture_db> <out_path>
write_mcp_config() {
    local cartog_bin="$1" fixture_db="$2" out_path="$3"
    CARTOG_BIN="$cartog_bin" FIXTURE_DB="$fixture_db" python3 - "$out_path" <<'PY'
import json, os, sys
config = {
    "mcpServers": {
        "cartog": {
            "command": os.environ["CARTOG_BIN"],
            "args": ["serve"],
            "env": {"CARTOG_DB": os.environ["FIXTURE_DB"]},
        }
    }
}
with open(sys.argv[1], "w") as fh:
    json.dump(config, fh)
PY
}

# Metrics from a stream-json transcript. COST is the primary metric — it prices
# cache reads (~0.1x) vs fresh input correctly, whereas a lump-sum token total
# double-counts cheap cache reads against tool-heavy MCP usage. The token
# categories are reported separately so that effect stays visible.
# Usage: parse_usage <stream_json_file>
#   → "<cost_usd> <tool_calls> <duration_s> <total_tokens> <input> <cache_create> <cache_read> <output>"
parse_usage() {
    python3 - "$1" <<'PY'
import json, sys

tool_calls = 0
result = None

with open(sys.argv[1]) as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "assistant":
            for block in event.get("message", {}).get("content", []) or []:
                if block.get("type") == "tool_use":
                    tool_calls += 1
        elif event.get("type") == "result":
            result = event

u = (result or {}).get("usage", {})
inp = u.get("input_tokens", 0)
cc = u.get("cache_creation_input_tokens", 0)
cr = u.get("cache_read_input_tokens", 0)
out = u.get("output_tokens", 0)
total = inp + cc + cr + out
cost = (result or {}).get("total_cost_usd", 0) or 0
duration = ((result or {}).get("duration_ms", 0) or 0) / 1000
print(f"{cost:.4f} {tool_calls} {duration:.0f} {total} {inp} {cc} {cr} {out}")
PY
}

# The agent's final answer (the `result` event's text), for the judge to score.
# Usage: extract_answer <stream_json_file>
extract_answer() {
    python3 - "$1" <<'PY'
import json, sys

answer = ""
with open(sys.argv[1]) as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "result" and isinstance(event.get("result"), str):
            answer = event["result"]
print(answer)
PY
}

# Median of integer args. Returns 0 for an empty list — including the phantom
# empty-string arg a "${arr[@]:-}" expansion injects when an arm has 0 PASS runs.
# Usage: median <n1> <n2> ...
median() {
    printf '%s\n' "$@" | python3 -c '
import sys
xs = sorted(int(x) for x in sys.stdin.read().split())
n = len(xs)
print(0 if n == 0 else (xs[n // 2] if n % 2 else (xs[n // 2 - 1] + xs[n // 2]) // 2))
'
}

# Median of float args (for cost). Returns 0 for an empty list.
# Usage: median_float <n1> <n2> ...
median_float() {
    printf '%s\n' "$@" | python3 -c '
import sys
xs = sorted(float(x) for x in sys.stdin.read().split())
n = len(xs)
m = 0.0 if n == 0 else (xs[n // 2] if n % 2 else (xs[n // 2 - 1] + xs[n // 2]) / 2)
print(f"{m:.4f}")
'
}

# Percent reduction of `with` vs `without`, integer inputs (positive = with is lower).
# Usage: pct_reduction <without> <with>
pct_reduction() {
    local without="$1" with="$2"
    if [ "$without" -le 0 ] 2>/dev/null; then
        echo "n/a"
    else
        awk "BEGIN { printf \"%.1f\", (1 - $with / $without) * 100 }"
    fi
}

# Percent reduction for float inputs (cost). "n/a" if the baseline is 0.
# Usage: pct_reduction_float <without> <with>
pct_reduction_float() {
    awk -v a="$1" -v b="$2" 'BEGIN { if (a+0 <= 0) print "n/a"; else printf "%.1f", (1 - b / a) * 100 }'
}
