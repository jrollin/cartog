#!/usr/bin/env bash
# cartog agent-task benchmark: end-to-end agent cost (tokens, tool calls, USD,
# time), cartog on vs off, median over N runs, LLM-judged for correctness.
# Requires claude CLI, jq, python3, git. Methodology + usage: benchmarks/agent/README.md.

set -euo pipefail

AGENT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$(cd "$AGENT_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$BENCH_DIR/.." && pwd)"
RESULTS_DIR="$AGENT_DIR/results"
RESULTS_FILE="$RESULTS_DIR/agent-latest.jsonl"
CORPUS_DIR="${CARTOG_BENCH_CORPUS:-$AGENT_DIR/.corpus}"

source "$AGENT_DIR/lib.sh"
source "$PROJECT_ROOT/scripts/lib/llm_judge.sh"

# ── Args ──
# Target is EITHER a synthetic fixture (fast, default) OR a real cloned repo.

RUNS=4
FIXTURE="py"
REPO_FILTER=""        # repo id, or "all"
LANG_FILTER=""        # selects repos by their `lang:` field
TASK_FILTER=""
MODEL="${CARTOG_BENCH_MODEL:-opus}"
JUDGE_MODEL="${CARTOG_BENCH_JUDGE_MODEL:-}"   # defaults to MODEL if unset (resolved below)
BUDGET_USD="${CARTOG_BENCH_BUDGET:-4}"
# Independent (arm, run) units execute concurrently up to this many at once.
# Bounded to respect API rate limits; runs are otherwise independent.
MAX_PARALLEL="${CARTOG_BENCH_PARALLEL:-5}"
EMBED=1   # 0 = skip `rag index` (semantic search); structural tasks rarely need it
while [[ $# -gt 0 ]]; do
    case $1 in
        --runs)        RUNS="$2"; shift 2 ;;
        --fixture)     FIXTURE="$2"; REPO_FILTER=""; LANG_FILTER=""; shift 2 ;;
        --repo)        REPO_FILTER="$2"; shift 2 ;;
        --lang)        LANG_FILTER="$2"; shift 2 ;;
        --task)        TASK_FILTER="$2"; shift 2 ;;
        --model)       MODEL="$2"; shift 2 ;;
        --judge-model) JUDGE_MODEL="$2"; shift 2 ;;
        --no-embed)    EMBED=0; shift ;;
        -h|--help)
            echo "Usage: $0 [--runs N] [--model M] [--judge-model M] [--no-embed]"
            echo "  Fixture mode (default): --fixture py|ts|go|rs|rb|java|php  [--task ID]"
            echo "  Real-repo mode:         --repo <id>|all   OR   --lang py|rs|java|go"
            echo "  --model       agent model for both arms (default opus)"
            echo "  --judge-model PASS/FAIL judge model (default: same as --model;"
            echo "                a cheaper model like haiku/sonnet cuts cost)"
            echo "  --no-embed    skip the heavy 'rag index' embed step (structural"
            echo "                tasks rarely use semantic search; big speedup on large repos)"
            echo "  Repos and their language tags are defined in repos.yaml."
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# Judge defaults to the agent model unless overridden.
JUDGE_MODEL="${JUDGE_MODEL:-$MODEL}"

# ── Prerequisites ──

echo -e "${BOLD}=== cartog Agent-Task Benchmark ===${NC}"
echo ""

for cmd in claude jq python3 git; do
    if ! command -v "$cmd" &>/dev/null; then
        echo -e "${RED}Missing required tool: $cmd${NC}"
        [ "$cmd" = "claude" ] && echo "  Install: https://docs.anthropic.com/en/docs/claude-code"
        exit 1
    fi
done

if [ -n "${CARTOG:-}" ]; then
    CARTOG_BIN="$CARTOG"
elif command -v cartog &>/dev/null; then
    CARTOG_BIN="$(command -v cartog)"
else
    echo -e "${YELLOW}cartog not in PATH, building release...${NC}"
    (cd "$PROJECT_ROOT" && cargo build --release 2>&1 | tail -1)
    CARTOG_BIN="$PROJECT_ROOT/target/release/cartog"
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# ── Build the work-list of (target_dir, label, prompt, expected) tuples ──
# Fixture mode: one synthetic webapp, many tasks from tasks.yaml.
# Repo mode: one deep mechanism question per cloned repo from repos.yaml.

WORKLIST="$WORK_DIR/worklist.jsonl"
: > "$WORKLIST"

if [ -n "$REPO_FILTER" ] || [ -n "$LANG_FILTER" ]; then
    echo "Mode: real repos    Runs/arm: $RUNS    Model: $MODEL    Judge: $JUDGE_MODEL"
    [ -n "$LANG_FILTER" ] && echo "lang filter: $LANG_FILTER"
    [ -n "$REPO_FILTER" ] && echo "repo filter: $REPO_FILTER"
    echo "cartog:  $CARTOG_BIN"
    echo "corpus:  $CORPUS_DIR"
    echo ""
    mkdir -p "$CORPUS_DIR"

    REPOS_JSON=$(python3 "$AGENT_DIR/parse_repos.py" "$AGENT_DIR/repos.yaml")
    REPO_COUNT=$(echo "$REPOS_JSON" | jq 'length')
    for ((r=0; r<REPO_COUNT; r++)); do
        id=$(echo "$REPOS_JSON" | jq -r ".[$r].id")
        lang=$(echo "$REPOS_JSON" | jq -r ".[$r].lang")
        # Select if it matches the repo id (or "all"), or the language filter.
        if [ -n "$REPO_FILTER" ] && [ "$REPO_FILTER" != "all" ] && [ "$id" != "$REPO_FILTER" ]; then continue; fi
        if [ -n "$LANG_FILTER" ] && [ "$lang" != "$LANG_FILTER" ]; then continue; fi

        url=$(echo "$REPOS_JSON" | jq -r ".[$r].url")
        rev=$(echo "$REPOS_JSON" | jq -r ".[$r].rev")
        prompt=$(echo "$REPOS_JSON" | jq -r ".[$r].prompt")
        expected=$(echo "$REPOS_JSON" | jq -r ".[$r].expected")
        target="$CORPUS_DIR/$id"

        if [ ! -d "$target/.git" ]; then
            echo -e "${BOLD}Cloning $id @ $rev...${NC}"
            git clone --depth 1 --branch "$rev" "$url" "$target" >/dev/null 2>&1 \
                || { echo -e "${YELLOW}clone failed: $id ($rev)${NC}"; continue; }
        fi
        echo -e "${BOLD}Indexing $id...${NC}"
        local_db="$WORK_DIR/$id.sqlite"
        (cd "$target" && CARTOG_DB="$local_db" "$CARTOG_BIN" index . --force >/dev/null 2>&1) \
            || { echo -e "${YELLOW}index failed: $id — skipping${NC}"; continue; }
        [ "$EMBED" -eq 1 ] && (cd "$target" && CARTOG_DB="$local_db" "$CARTOG_BIN" rag index . >/dev/null 2>&1 || true)

        jq -nc --arg t "$target" --arg l "$id" --arg p "$prompt" \
            --arg e "$expected" --arg db "$local_db" \
            '{target:$t,label:$l,prompt:$p,expected:$e,db:$db}' >> "$WORKLIST"
    done
else
    FIXTURE_DIR="$BENCH_DIR/fixtures/webapp_${FIXTURE}"
    [ -d "$FIXTURE_DIR" ] || { echo -e "${RED}No fixture: $FIXTURE_DIR${NC}"; exit 1; }
    echo "Mode: fixture webapp_${FIXTURE}    Runs/arm: $RUNS    Model: $MODEL    Judge: $JUDGE_MODEL"
    echo "cartog:  $CARTOG_BIN"
    echo ""

    echo -e "${BOLD}Indexing fixture...${NC}"
    fixture_db="$WORK_DIR/fixture.sqlite"
    (cd "$FIXTURE_DIR" && CARTOG_DB="$fixture_db" "$CARTOG_BIN" index . --force >/dev/null 2>&1)
    [ "$EMBED" -eq 1 ] && (cd "$FIXTURE_DIR" && CARTOG_DB="$fixture_db" "$CARTOG_BIN" rag index . >/dev/null 2>&1 || true)

    TASKS_JSON=$(python3 "$AGENT_DIR/parse_tasks.py" "$AGENT_DIR/tasks.yaml")
    TASK_COUNT=$(echo "$TASKS_JSON" | jq 'length')
    for ((t=0; t<TASK_COUNT; t++)); do
        id=$(echo "$TASKS_JSON" | jq -r ".[$t].id")
        [ -n "$TASK_FILTER" ] && [ "$id" != "$TASK_FILTER" ] && continue
        prompt=$(echo "$TASKS_JSON" | jq -r ".[$t].prompt")
        expected=$(echo "$TASKS_JSON" | jq -r ".[$t].expected | join(\"\n\")")
        jq -nc --arg t "$FIXTURE_DIR" --arg l "$id" --arg p "$prompt" \
            --arg e "$expected" --arg db "$fixture_db" \
            '{target:$t,label:$l,prompt:$p,expected:$e,db:$db}' >> "$WORKLIST"
    done
fi
echo ""

# Empty MCP config for the baseline arm (cartog availability is the only variable).
echo '{"mcpServers":{}}' > "$WORK_DIR/mcp-empty.json"

# Drive one agent run; prints parse_usage fields + verdict, i.e.
# "<cost> <tool_calls> <time> <total_tokens> <input> <cache_create> <cache_read> <output> <verdict>".
# No system prompt either arm — MCP config is the only variable (cartog server vs.
# empty), so the model selects tools purely from their descriptions. Tools are
# fenced (--allowedTools + --disable-slash-commands) so the ambient .claude config
# can't leak the cartog skill into the baseline. tool_breakdown records the picks.
run_arm() {
    local arm="$1" target="$2" prompt="$3" expected="$4" db="$5" idx="$6"
    local transcript="$WORK_DIR/${arm}_${idx}.jsonl"
    local mcp_config

    # Baseline: built-in exploration tools only. cartog arm: same + the cartog
    # MCP tools (mcp__cartog__*).
    local allowed="Read Grep Glob Bash"
    if [ "$arm" = "cartog" ]; then
        mcp_config="$WORK_DIR/mcp-cartog-${idx}.json"
        write_mcp_config "$CARTOG_BIN" "$db" "$mcp_config"
        allowed="$allowed mcp__cartog"
    else
        mcp_config="$WORK_DIR/mcp-empty.json"
    fi

    # A failed arm (crash, budget cap) is a legitimate outcome the harness
    # reports as 0 tool calls / judge FAIL — not fatal — so `|| true` keeps the
    # multi-repo run alive under `set -e`. stderr goes to a per-arm log (not
    # /dev/null) so a failure is inspectable.
    # shellcheck disable=SC2086  # $allowed is an intentional multi-arg list
    (cd "$target" && claude --print --output-format stream-json --verbose \
        --model "$MODEL" \
        --mcp-config "$mcp_config" --strict-mcp-config \
        --allowedTools $allowed \
        --disable-slash-commands \
        --permission-mode bypassPermissions \
        --max-budget-usd "$BUDGET_USD" \
        --no-session-persistence \
        "$prompt" > "$transcript" 2>"${transcript%.jsonl}.err") || true

    local usage answer verdict
    usage=$(parse_usage "$transcript")
    answer=$(extract_answer "$transcript")
    verdict=$(judge "$answer" "$expected")
    # Tool breakdown is JSON (spaces/braces) so it can't ride the positional line;
    # write it to a sibling file the collector reads for the first PASS run.
    parse_tools "$transcript" > "${transcript%.jsonl}.tools"
    echo "$usage $verdict"
}

# One (arm, run) unit → its own result file, so concurrent units never interleave.
run_unit() {
    local arm="$1" target="$2" prompt="$3" expected="$4" db="$5" idx="$6" out="$7"
    run_arm "$arm" "$target" "$prompt" "$expected" "$db" "$idx" > "$out"
}

# LLM judge via the shared scripts/lib/llm_judge.sh. Prints PASS|FAIL.
judge() {
    local answer="$1" expected="$2"
    local judge_prompt verdict
    judge_prompt="Score the agent answer PASS or FAIL on its first line.
PASS = the answer covers most of what a correct answer must explain (a clear majority).
FAIL = the answer misses most of it or is wrong.

A correct answer must explain / name:
$expected

Agent answer:
$answer"
    verdict=$(judge_verdict "$JUDGE_MODEL" "$judge_prompt")
    if is_pass "$verdict"; then echo "PASS"; else echo "FAIL"; fi
}

# ── Run ──

mkdir -p "$RESULTS_DIR"
: > "$RESULTS_FILE"

# Cost is the headline (it prices cache reads correctly); tokens are broken out.
printf "${BOLD}%-10s | %-28s | %-28s | %s${NC}\n" \
    "Target" "Baseline (no cartog)" "With cartog" "Cost cut"
echo "$(printf '─%.0s' {1..94})"

ITEM_COUNT=$(wc -l < "$WORKLIST" | tr -d ' ')
[ "$ITEM_COUNT" -eq 0 ] && { echo -e "${RED}No work items (check --repo/--task/--fixture).${NC}"; exit 1; }

grand_base_cost=(); grand_cart_cost=()

while IFS= read -r item; do
    target=$(echo "$item" | jq -r '.target')
    label=$(echo "$item" | jq -r '.label')
    prompt=$(echo "$item" | jq -r '.prompt')
    expected=$(echo "$item" | jq -r '.expected')
    db=$(echo "$item" | jq -r '.db')

    b_cost=(); b_calls=(); b_time=(); b_tok=(); b_cr=(); b_pass=0
    c_cost=(); c_calls=(); c_time=(); c_tok=(); c_cr=(); c_pass=0

    # Run all RUNS x 2 arms concurrently in batches of MAX_PARALLEL (bash 3.2 has
    # no `wait -n`, so batch-and-wait). Each unit writes to its own file.
    unit_dir="$WORK_DIR/units_${label}"
    mkdir -p "$unit_dir"
    in_batch=0
    for ((r=0; r<RUNS; r++)); do
        for arm in baseline cartog; do
            tag="${arm:0:1}$r"
            run_unit "$arm" "$target" "$prompt" "$expected" "$db" "$tag" "$unit_dir/$tag" &
            in_batch=$((in_batch + 1))
            [ "$in_batch" -ge "$MAX_PARALLEL" ] && { wait; in_batch=0; }
        done
    done
    wait

    # parse_usage fields: cost calls time total_tok input cache_create cache_read output.
    # Track cost/calls/time/total/cache_read; rest → _. tool_breakdown = first PASS run's map.
    b_tools="{}"; c_tools="{}"
    for ((r=0; r<RUNS; r++)); do
        read -r bcost bc btime btok _ _ bcr _ bv < "$unit_dir/b$r"
        if [ "$bv" = "PASS" ]; then
            b_cost+=("$bcost"); b_calls+=("$bc"); b_time+=("$btime"); b_tok+=("$btok"); b_cr+=("$bcr"); b_pass=$((b_pass+1))
            [ "$b_pass" -eq 1 ] && b_tools=$(cat "$WORK_DIR/baseline_b$r.tools" 2>/dev/null || echo "{}")
        fi

        read -r ccost cc ctime ctok _ _ ccr _ cv < "$unit_dir/c$r"
        if [ "$cv" = "PASS" ]; then
            c_cost+=("$ccost"); c_calls+=("$cc"); c_time+=("$ctime"); c_tok+=("$ctok"); c_cr+=("$ccr"); c_pass=$((c_pass+1))
            [ "$c_pass" -eq 1 ] && c_tools=$(cat "$WORK_DIR/cartog_c$r.tools" 2>/dev/null || echo "{}")
        fi
    done

    bm_cost=$(median_float "${b_cost[@]:-}"); bm_calls=$(median "${b_calls[@]:-}")
    bm_time=$(median "${b_time[@]:-}"); bm_tok=$(median "${b_tok[@]:-}"); bm_cr=$(median "${b_cr[@]:-}")
    cm_cost=$(median_float "${c_cost[@]:-}"); cm_calls=$(median "${c_calls[@]:-}")
    cm_time=$(median "${c_time[@]:-}"); cm_tok=$(median "${c_tok[@]:-}"); cm_cr=$(median "${c_cr[@]:-}")
    # A reduction needs a real measurement in BOTH arms — a 0-PASS arm has no
    # median (cost 0), and reading that as a 100% win would be a lie.
    if [ "$b_pass" -gt 0 ] && [ "$c_pass" -gt 0 ]; then
        cost_cut=$(pct_reduction_float "$bm_cost" "$cm_cost")
        tok_cut=$(pct_reduction "$bm_tok" "$cm_tok")
    else
        cost_cut="n/a"; tok_cut="n/a"
    fi

    awk "BEGIN { exit !($bm_cost > 0) }" && grand_base_cost+=("$bm_cost")
    awk "BEGIN { exit !($cm_cost > 0) }" && grand_cart_cost+=("$cm_cost")

    printf "%-10s | \$%-6s %2s calls %d/%d✓ | \$%-6s %2s calls %d/%d✓ | %6s%%\n" \
        "$label" \
        "$bm_cost" "$bm_calls" "$b_pass" "$RUNS" \
        "$cm_cost" "$cm_calls" "$c_pass" "$RUNS" \
        "$cost_cut"

    jq -nc \
        --arg label "$label" --argjson runs "$RUNS" \
        --arg bcost "$bm_cost" --argjson bc "$bm_calls" --argjson btime "$bm_time" --argjson btok "$bm_tok" --argjson bcr "$bm_cr" --argjson bp "$b_pass" --argjson btools "$b_tools" \
        --arg ccost "$cm_cost" --argjson cc "$cm_calls" --argjson ctime "$cm_time" --argjson ctok "$cm_tok" --argjson ccr "$cm_cr" --argjson cp "$c_pass" --argjson ctools "$c_tools" \
        --arg costcut "$cost_cut" --arg tokcut "$tok_cut" \
        '{target:$label,runs:$runs,
          baseline:{median_cost_usd:($bcost|tonumber),median_tool_calls:$bc,median_time_s:$btime,median_tokens:$btok,median_cache_read_tokens:$bcr,pass:$bp,tool_breakdown:$btools},
          cartog:{median_cost_usd:($ccost|tonumber),median_tool_calls:$cc,median_time_s:$ctime,median_tokens:$ctok,median_cache_read_tokens:$ccr,pass:$cp,tool_breakdown:$ctools},
          cost_reduction_pct:$costcut,token_reduction_pct:$tokcut}' >> "$RESULTS_FILE"
done < "$WORKLIST"

# ── Summary ──

echo ""
total_base=$(median_float "${grand_base_cost[@]:-}")
total_cart=$(median_float "${grand_cart_cost[@]:-}")
echo -e "${BOLD}=== Summary ===${NC}"
echo "  Median cost/task:  baseline=\$$total_base  cartog=\$$total_cart"
echo "  Median cost reduction: $(pct_reduction_float "$total_base" "$total_cart")%"
echo "  (cost prices cache reads correctly; see per-task token breakdown in results)"
echo "  Results: $RESULTS_FILE"
echo ""
echo -e "${BOLD}Done.${NC}"
