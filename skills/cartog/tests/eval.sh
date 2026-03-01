#!/usr/bin/env bash
set -euo pipefail

# LLM-as-judge evaluation for cartog skill behavioral tests.
#
# Uses the `claude` CLI (Claude Code) for both agent simulation and judging.
# No API key needed — uses your existing claude auth.
#
# Requirements:
#   - claude CLI (Claude Code): https://docs.anthropic.com/en/docs/claude-code
#   - python3 + pyyaml: pip3 install pyyaml
#   - jq: brew install jq
#
# Usage:
#   bash skills/cartog/tests/eval.sh                    # run all scenarios
#   bash skills/cartog/tests/eval.sh --id search_natural_language  # run one
#   bash skills/cartog/tests/eval.sh --tag routing       # run by tag
#   bash skills/cartog/tests/eval.sh --dry-run           # show prompts only
#   bash skills/cartog/tests/eval.sh --model sonnet      # use a different model
#
# Cost: ~$0.01-0.03 per scenario (depending on model)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SKILL_MD="$SKILL_DIR/SKILL.md"
GOLDEN="$SCRIPT_DIR/golden_examples.yaml"

MODEL="${CARTOG_EVAL_MODEL:-sonnet}"

FILTER_ID=""
FILTER_TAG=""
DRY_RUN=false
PASS=0
FAIL=0
SKIP=0

# --- arg parsing ---

while [[ $# -gt 0 ]]; do
    case "$1" in
        --id) FILTER_ID="$2"; shift 2 ;;
        --tag) FILTER_TAG="$2"; shift 2 ;;
        --dry-run) DRY_RUN=true; shift ;;
        --model) MODEL="$2"; shift 2 ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

# --- dependency checks ---

for cmd in claude python3 jq; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "Error: $cmd is required."
        [ "$cmd" = "claude" ] && echo "  Install: https://docs.anthropic.com/en/docs/claude-code"
        [ "$cmd" = "python3" ] && echo "  Also needs: pip3 install pyyaml"
        [ "$cmd" = "jq" ] && echo "  Install: brew install jq"
        exit 1
    fi
done

# --- YAML to JSON via python3 ---

yaml_to_json() {
    python3 -c "
import sys, json
try:
    import yaml
    data = yaml.safe_load(open(sys.argv[1]))
except ImportError:
    print('Error: pip3 install pyyaml', file=sys.stderr)
    sys.exit(1)
print(json.dumps(data))
" "$1"
}

GOLDEN_JSON=$(yaml_to_json "$GOLDEN")
SCENARIO_COUNT=$(echo "$GOLDEN_JSON" | jq 'length')

# --- load skill ---

SKILL_CONTENT=$(cat "$SKILL_MD")

# --- evaluate one scenario ---

evaluate_scenario() {
    local idx="$1"

    local id query expected_calls anti_patterns reasoning description context tags
    id=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].id")
    description=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].description")
    query=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].user_query")
    context=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].context // \"\"")
    expected_calls=$(echo "$GOLDEN_JSON" | jq -r "(.[$idx].expected.tool_calls // [])[]" 2>/dev/null || echo "")
    anti_patterns=$(echo "$GOLDEN_JSON" | jq -r "(.[$idx].anti_patterns // [])[]" 2>/dev/null || echo "")
    reasoning=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].expected.reasoning")
    tags=$(echo "$GOLDEN_JSON" | jq -r "(.[$idx].tags // [])[]" 2>/dev/null || echo "")

    # Check filters
    if [ -n "$FILTER_ID" ] && [ "$id" != "$FILTER_ID" ]; then
        return
    fi
    if [ -n "$FILTER_TAG" ]; then
        if ! echo "$tags" | grep -qF "$FILTER_TAG"; then
            return
        fi
    fi

    echo "--- Scenario: $id ---"
    echo "  $description"
    echo "  Query: $query"

    # Build prompts
    local agent_system agent_user
    agent_system="You are a coding assistant. You have the following skill loaded:

$SKILL_CONTENT

Based on this skill, respond to the user's query by describing which cartog commands you would run and in what order. List each command on its own line prefixed with '> '. Do not explain — just list the commands."

    agent_user="$query"
    if [ -n "$context" ]; then
        agent_user="$context

$query"
    fi

    if [ "$DRY_RUN" = true ]; then
        echo "  [DRY RUN] Would send to $MODEL"
        echo "  System: (skill loaded, ${#SKILL_CONTENT} chars)"
        echo "  User: $agent_user"
        echo ""
        SKIP=$((SKIP + 1))
        return
    fi

    # Step 1: Agent call — ask the LLM what commands it would run
    local agent_response
    agent_response=$(claude \
        --print \
        --model "$MODEL" \
        --system-prompt "$agent_system" \
        --tools "" \
        --no-session-persistence \
        "$agent_user" 2>/dev/null)

    # Step 2: Judge call — evaluate the agent's response
    local judge_prompt verdict judge_response
    judge_prompt="You are an evaluator. Score the agent response as PASS or FAIL.

Judging rules:
- The FIRST command the agent lists is what matters most for scoring.
- Follow-up or conditional commands (e.g., 'if no results, then...') are acceptable and should NOT cause a FAIL.
- Anti-patterns apply to the FIRST action only, not to hypothetical follow-ups the agent mentions.
- PASS = the agent's FIRST command matches the expected behavior and avoids anti-patterns.
- FAIL = the agent's FIRST command is wrong or matches an anti-pattern.

Respond with exactly one line: PASS or FAIL, followed by a colon and a brief reason.
Example: PASS: agent correctly used rag search as first command

---

Agent response:
$agent_response

Expected first command(s):
$expected_calls

Anti-patterns (must NOT be the FIRST action):
$anti_patterns

Reasoning:
$reasoning"

    judge_response=$(claude \
        --print \
        --model "$MODEL" \
        --tools "" \
        --no-session-persistence \
        "$judge_prompt" 2>/dev/null)

    verdict=$(echo "$judge_response" | head -1)

    if echo "$verdict" | grep -qi "^PASS"; then
        echo "  PASS: $verdict"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $verdict"
        echo "  Agent said:"
        echo "$agent_response" | sed 's/^/    /'
        FAIL=$((FAIL + 1))
    fi
    echo ""
}

# --- main ---

echo "=== cartog skill evaluation ==="
echo "Model: $MODEL"
echo "Scenarios: $SCENARIO_COUNT"
echo ""

for ((i=0; i<SCENARIO_COUNT; i++)); do
    evaluate_scenario "$i"
done

echo "=== Results: $PASS passed, $FAIL failed, $SKIP skipped ==="

[ "$FAIL" -eq 0 ] || exit 1
