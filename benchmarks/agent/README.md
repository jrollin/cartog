# Agent-Task Benchmark

Measures the **end-to-end** cost of an AI agent answering architectural
questions in a real codebase, **with and without cartog**. This is the "does it
actually help the agent" claim that per-query token counts cannot make on their
own: a real agent, real tasks, with/without arms, median of N runs.

## How it differs from the other benchmark surfaces

| Surface | What it measures | LLM in the loop? |
|---------|------------------|------------------|
| Shell suite (`../run.sh`) | **Per-query** output size + recall: one cartog command vs one grep | no |
| Criterion benches | **In-process latency** of cartog's own CPU work (µs–ms) | no |
| **Agent-task (this)** | **End-to-end task cost**: tokens, tool calls, USD, and time to answer a question, cartog-on vs cartog-off | yes (agent + judge) |

The shell suite asks "is a cartog query smaller than grep output?". This suite
asks "does giving an agent cartog reduce the *total* cost of finishing a task?"
— a different, more real-world question. See
[docs/tech.md → Benchmarks](../../docs/tech.md#benchmarks) for the full matrix.

## Two target modes

| Mode | Target | Tasks | Speed | Use for |
|------|--------|-------|-------|---------|
| **fixture** (default) | synthetic `../fixtures/webapp_<lang>` | `tasks.yaml` (callers, impact, hierarchy, trace, concept) | fast, no network | quick smoke / wiring check |
| **repo** | real OSS repos cloned at `--depth 1` | `repos.yaml` (one deep "how does X work?" per repo) | slow (clone + index + deep agent runs) | the credible end-to-end story |

The win lives in **task difficulty and repo size**: on a tiny synthetic fixture a
1-hop lookup is cheap for the baseline too, and the MCP tool-schema adds a fixed
prompt cost to the cartog arm — so the fixture mode can show a flat or slightly
negative token delta. The real win shows on the **repo** mode's deep mechanism
questions, where the baseline thrashes through many grep/read round-trips.

## Methodology

- **Two arms per task**
  - `baseline` — empty MCP config; agent has only `Read`/`Grep`/`Glob`.
  - `cartog` — same agent + cartog's MCP server over a prebuilt index, nudged to
    prefer its tools.
  - cartog availability is the **only** variable; both arms run with
    `--strict-mcp-config` (no ambient MCP servers leak in) and a per-run
    `--max-budget-usd` cap.
- **N runs per arm** (default 4) → report the **median** to damp LLM variance.
  Variance is large run-to-run; treat single runs as directional only.
- **Cost (USD) is the headline metric**, not a lump-sum token total. A token
  total sums fresh input, cache-creation, cache-read, and output as if equal —
  but they are priced ~10× apart. cartog's granular MCP tools generate many
  *cheap cache-read* tokens (the agent re-reads the conversation each round-trip),
  which inflates the lump-sum total while *lowering* cost. Reporting cost prices
  the cache correctly; the four token categories are reported separately so the
  effect stays visible rather than hidden in one misleading number.
- **Metrics** (parsed from `stream-json`): cost (USD, primary), tool calls,
  wall-time, total tokens, and cache-read tokens (the category that diverges).
- **Correctness gate** — an LLM judge (shared `scripts/lib/llm_judge.sh`) scores
  each run PASS/FAIL against the task's `expected` rubric. **Only PASS runs feed
  the medians**, so a cheaper-but-wrong answer cannot win.

Each target is indexed (and embedded) once up-front into a temp DB; agents never
re-index during a run, so indexing cost is out of scope here (it has its own
criterion bench).

## Usage

```bash
make bench-agent                              # fixture mode: webapp_py, 4 runs/arm, opus

# Fixture mode
./benchmarks/agent/run.sh --fixture ts        # different language fixture
./benchmarks/agent/run.sh --task callers      # single task
./benchmarks/agent/run.sh --runs 6            # more runs, tighter median

# Real-repo mode (clones into .corpus/, gitignored)
./benchmarks/agent/run.sh --repo django       # one repo by id
./benchmarks/agent/run.sh --repo all          # all repos in repos.yaml
./benchmarks/agent/run.sh --lang py           # all repos tagged that language
./benchmarks/agent/run.sh --model sonnet      # cheaper agent + judge
```

Targets live in `repos.yaml`, one curated repo per language. Select by `id`
(`--repo`) or by `lang` (`--lang`); to swap which repo represents a language,
edit that entry in place. Every field (`id`, `url`, `rev`, `lang`, `prompt`,
`expected`) is required — `parse_repos.py` rejects an incomplete entry.

## Adding a language

A new language is **one new entry in `repos.yaml`** — no code change. Pick a real
repo whose language cartog supports (py/ts/rs/go/rb/java/php/dart):

```yaml
  - id: rails                              # unique slug → corpus dir + --repo selector
    url: https://github.com/rails/rails    # git clone source
    rev: v7.2.2                            # tag/branch to PIN — keeps runs reproducible
    lang: rb                               # cartog language tag → the --lang selector
    prompt: >-
      How does Rails route an incoming request to a controller action?
      Walk through the key classes and the dispatch path.
    expected: >-
      Names the RouteSet, route matching, ActionDispatch, and how the controller
      action is invoked.
```

Run it with `./benchmarks/agent/run.sh --lang rb` (or `--repo rails`).

### Writing the `expected` rubric

Repo-mode tasks are open "how does X work?" questions with no ground-truth file,
so `expected` is a **short prose rubric** of what a correct answer must *explain*
— not a list of exact symbols. The judge passes a run when the answer covers a
clear majority of it.

- **Name the key types/methods** an answer must mention, and the **mechanism
  path** between them ("how a Call proceeds through the interceptors to a
  Response") — not just loose keywords.
- **Specific enough that a vague or wrong answer fails**, but not so exhaustive
  (10 exact internal method names) that a correct-but-differently-worded answer
  fails.
- Avoid both extremes: "explains routing" passes everything (no signal); a full
  call-stack transcript rejects valid paraphrases.

## Prerequisites

- `claude` CLI (Claude Code) — drives both arms and the judge. Uses your
  existing auth; no API key needed.
- `jq`, `python3`, `git` — parsing, medians, and repo-mode cloning.
- `cartog` binary — built by `make bench-agent`, or pass `CARTOG=/path/to/cartog`.

> **Not run in CI.** Each run spends real model tokens (agent + judge across two
> arms × N runs). Run locally when validating the cost/efficiency claim.

## Output

Prints a per-target table (cost-headlined) and a median summary, and writes
`results/agent-latest.jsonl` (gitignored). Each line:

```json
{"target":"gin","runs":4,"baseline":{"median_cost_usd":0.62,"median_tool_calls":13,"median_time_s":118,"median_tokens":847000,"median_cache_read_tokens":210000,"pass":4},"cartog":{"median_cost_usd":0.46,"median_tool_calls":5,"median_time_s":94,"median_tokens":651000,"median_cache_read_tokens":380000,"pass":4},"cost_reduction_pct":"25.8","token_reduction_pct":"23.0"}
```

`median_cost_usd` is the headline (cache priced correctly). `median_tokens` is
the lump sum and `median_cache_read_tokens` is broken out so you can see how much
of the token total is cheap cache reads. `pass` is how many of the N runs the
judge scored correct; only those count toward the medians.
