#!/usr/bin/env bash
set -euo pipefail

# Thin wrapper for cartog queries with JSON output.
# Usage: query.sh <command> [args...]
# Examples:
#   query.sh outline src/auth/tokens.py
#   query.sh callers validate_token
#   query.sh impact SessionManager --depth 3

if [ $# -lt 1 ]; then
    echo "Usage: query.sh <command> [args...]"
    echo "Commands: outline, callers, callees, impact, refs, hierarchy, deps, stats"
    exit 1
fi

exec cartog --json "$@"
