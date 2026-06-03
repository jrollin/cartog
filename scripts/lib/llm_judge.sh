#!/usr/bin/env bash
# Shared LLM-as-judge invocation for the skill eval, agent eval, and agent-task
# benchmark, so the `claude --print` judge call stays consistent. Callers own
# their own grounding and judge-prompt body.

# Score a fully-formed judge prompt (must instruct a leading PASS/FAIL first line).
# Echoes the verdict's first line, or "ERROR: judge call failed" so callers branch.
# Usage: judge_verdict <model> <judge_prompt>
judge_verdict() {
    local model="$1" judge_prompt="$2"
    local response
    response=$(claude \
        --print \
        --model "$model" \
        --strict-mcp-config \
        --no-session-persistence \
        "$judge_prompt" 2>/dev/null) || {
        echo "ERROR: judge call failed"
        return 0
    }
    # First line via parameter expansion — no `head` pipe to avoid a SIGPIPE
    # on the echo under `set -o pipefail`.
    printf '%s\n' "${response%%$'\n'*}"
}

# True (0) if a verdict line's leading word is PASS. Word-boundary match (no pipe)
# so "PASSABLE"/"Passing" don't count and a leading echo flag can't eat the input.
# Usage: is_pass "<verdict line>"
is_pass() {
    case "$1" in
        [Pp][Aa][Ss][Ss] | [Pp][Aa][Ss][Ss][^A-Za-z]*) return 0 ;;
        *) return 1 ;;
    esac
}
