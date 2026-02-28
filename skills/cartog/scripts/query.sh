#!/usr/bin/env bash
set -euo pipefail

# Thin wrapper for cartog queries with JSON output.
# Usage: query.sh <command> [args...]
# Examples:
#   query.sh outline src/auth/tokens.py
#   query.sh refs validate_token --kind calls
#   query.sh impact SessionManager --depth 3

if [ $# -lt 1 ]; then
    echo "Usage: query.sh <command> [args...]"
    echo "Commands: search, outline, callees, impact, refs, hierarchy, deps, stats, rag"
    exit 1
fi

exec cartog --json "$@"
