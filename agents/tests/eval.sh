#!/usr/bin/env bash
set -euo pipefail

# LLM-as-judge evaluation for cartog agent behavioral tests.
#
# Runs each agent end-to-end on a real codebase and evaluates the output:
#   - must_contain / must_not_contain: deterministic grep (hard pass/fail)
#   - quality: LLM-as-judge (soft evaluation for nuanced criteria)
#
# Requirements:
#   - claude CLI (Claude Code): https://docs.anthropic.com/en/docs/claude-code
#   - python3 + pyyaml: pip3 install pyyaml
#   - jq: brew install jq
#   - cartog index built in the test cwd
#
# Usage:
#   bash agents/tests/eval.sh                                  # run all
#   bash agents/tests/eval.sh --id onboarding_rust_cli         # run one
#   bash agents/tests/eval.sh --tag refactoring                # run by tag
#   bash agents/tests/eval.sh --dry-run                        # show what would run
#   bash agents/tests/eval.sh --model opus                     # judge with different model
#
# Cost: ~$0.05-0.20 per scenario (agent run + judge call)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENTS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$AGENTS_DIR/.." && pwd)"
GOLDEN="$SCRIPT_DIR/golden_examples.yaml"

source "$REPO_ROOT/scripts/lib/llm_judge.sh"

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

# --- deterministic checks ---

# Returns 0 if all must_contain phrases are found (case-insensitive)
check_must_contain() {
    local output="$1"
    shift
    for phrase in "$@"; do
        if ! echo "$output" | grep -qi "$phrase"; then
            echo "$phrase"
            return 1
        fi
    done
    return 0
}

# Returns 0 if no must_not_contain phrases are found (case-insensitive)
check_must_not_contain() {
    local output="$1"
    shift
    for phrase in "$@"; do
        [ -z "$phrase" ] && continue
        if echo "$output" | grep -qi "$phrase"; then
            echo "$phrase"
            return 1
        fi
    done
    return 0
}

# --- evaluate one scenario ---

evaluate_scenario() {
    local idx="$1"

    local id agent description cwd prompt tags
    id=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].id")
    agent=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].agent")
    description=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].description")
    cwd=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].cwd // \".\"")
    prompt=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].prompt")
    tags=$(echo "$GOLDEN_JSON" | jq -r "(.[$idx].tags // [])[]" 2>/dev/null || echo "")

    local quality
    quality=$(echo "$GOLDEN_JSON" | jq -r ".[$idx].criteria.quality")

    # Read must_contain into array
    local -a must_contain_arr=()
    while IFS= read -r line; do
        [ -n "$line" ] && must_contain_arr+=("$line")
    done < <(echo "$GOLDEN_JSON" | jq -r "(.[$idx].criteria.must_contain // [])[]" 2>/dev/null)

    local -a must_not_contain_arr=()
    while IFS= read -r line; do
        [ -n "$line" ] && must_not_contain_arr+=("$line")
    done < <(echo "$GOLDEN_JSON" | jq -r "(.[$idx].criteria.must_not_contain // [])[]" 2>/dev/null)

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
    echo "  Agent: $agent"

    # Resolve paths
    local agent_path work_dir
    agent_path="$AGENTS_DIR/$agent"
    if [ "$cwd" = "." ]; then
        work_dir="$REPO_ROOT"
    else
        work_dir="$cwd"
    fi

    if [ ! -f "$agent_path" ]; then
        echo "  SKIP: agent file not found: $agent_path"
        SKIP=$((SKIP + 1))
        echo ""
        return
    fi

    if [ "$DRY_RUN" = true ]; then
        echo "  [DRY RUN] Would run: claude --agent $agent_path --print -p \"$prompt\""
        echo "  Working dir: $work_dir"
        echo ""
        SKIP=$((SKIP + 1))
        return
    fi

    # Step 1: Run the agent
    echo "  Running agent..."
    local agent_output
    agent_output=$(cd "$work_dir" && claude \
        --agent "$agent_path" \
        --print \
        --no-session-persistence \
        -p "$prompt" 2>/dev/null) || {
        echo "  FAIL: agent exited with error"
        FAIL=$((FAIL + 1))
        echo ""
        return
    }

    local output_lines
    output_lines=$(echo "$agent_output" | wc -l | tr -d ' ')
    echo "  Agent produced $output_lines lines"

    # Step 2: Deterministic checks (hard pass/fail)
    local missing_phrase found_phrase

    if [ ${#must_contain_arr[@]} -gt 0 ]; then
        if missing_phrase=$(check_must_contain "$agent_output" "${must_contain_arr[@]}"); then
            echo "  Grep checks: all must_contain found"
        else
            echo "  FAIL (grep): missing required phrase: \"$missing_phrase\""
            echo "  Agent output (first 20 lines):"
            echo "$agent_output" | head -20 | sed 's/^/    /'
            FAIL=$((FAIL + 1))
            echo ""
            return
        fi
    fi

    if [ ${#must_not_contain_arr[@]} -gt 0 ]; then
        if found_phrase=$(check_must_not_contain "$agent_output" "${must_not_contain_arr[@]}"); then
            echo "  Grep checks: no must_not_contain found"
        else
            echo "  FAIL (grep): found forbidden phrase: \"$found_phrase\""
            echo "  Agent output (first 20 lines):"
            echo "$agent_output" | head -20 | sed 's/^/    /'
            FAIL=$((FAIL + 1))
            echo ""
            return
        fi
    fi

    # Step 3: LLM-as-judge for quality (soft evaluation)
    local judge_prompt
    judge_prompt="You are an evaluator for an AI agent's output. Score it as PASS or FAIL.

The output has already passed deterministic checks (required phrases present,
forbidden phrases absent). Your job is to evaluate the QUALITY criteria only.

Respond with exactly one line: PASS or FAIL, followed by a colon and a brief reason.
Example: PASS: report correctly identifies Rust workspace with tier architecture

---

Agent output:
$agent_output

---

Quality criteria:
$quality"

    echo "  Judging quality..."
    local verdict
    verdict=$(judge_verdict "$MODEL" "$judge_prompt")

    if echo "$verdict" | grep -q "^ERROR"; then
        echo "  SKIP: judge call failed"
        SKIP=$((SKIP + 1))
        echo ""
        return
    fi

    if is_pass "$verdict"; then
        echo "  $verdict"
        PASS=$((PASS + 1))
    else
        echo "  $verdict"
        echo "  Agent output (first 30 lines):"
        echo "$agent_output" | head -30 | sed 's/^/    /'
        FAIL=$((FAIL + 1))
    fi
    echo ""
}

# --- main ---

echo "=== cartog agent evaluation ==="
echo "Model (judge): $MODEL"
echo "Scenarios: $SCENARIO_COUNT"
echo ""

for ((i=0; i<SCENARIO_COUNT; i++)); do
    evaluate_scenario "$i"
done

echo "=== Results: $PASS passed, $FAIL failed, $SKIP skipped ==="

[ "$FAIL" -eq 0 ] || exit 1
