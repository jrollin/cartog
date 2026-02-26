# cartog Benchmark Suite

Compares cartog graph queries vs grep/cat approaches for common code navigation tasks.

## What it measures

| Dimension | Metric | Method |
|-----------|--------|--------|
| Token efficiency | Output bytes / approx tokens | Compare output size of cartog vs grep/cat for identical questions |
| Accuracy | Recall against ground truth | Count expected items found by each approach |
| Command count | Tool calls needed | How many commands each approach requires |

## Fixtures

- `fixtures/webapp_py/` — 15-file Python web app (auth service, tokens, models, routes, middleware)
- `fixtures/webapp_rs/` — 12-file Rust web app (same domain: traits, impls, modules)

Both fixtures have controlled, known relationships defined in `ground_truth/`.

## Scenarios

| # | Question | Key differentiator |
|---|----------|--------------------|
| 01 | "Who calls validate_token?" | `refs --kind calls` vs grep string match |
| 02 | "What's in auth/service?" | `outline` (structure only) vs `cat` (full file) |
| 03 | "Impact of renaming AuthService?" | `impact` (transitive BFS) vs grep (flat) |
| 04 | "Class hierarchy of BaseService?" | `hierarchy` (structured pairs) vs grep for class keywords |
| 05 | "Trace login → DB call chain" | Sequential `callees` vs multiple grep rounds |
| 06 | "File dependencies?" | `deps` (resolved targets) vs grep for import keywords |
| 07 | "Type references to X?" | `refs --kind references` vs context-unaware grep |

## Usage

```bash
# Run all scenarios
./benchmarks/run.sh

# Run single scenario
./benchmarks/run.sh --scenario 01

# Run only Python fixtures
./benchmarks/run.sh --fixture py

# Run only Rust fixtures
./benchmarks/run.sh --fixture rs
```

## Benchmark any project

`bench-project.sh` runs cartog vs grep on **any codebase** — no ground truth needed.

It auto-discovers symbols, runs 5 comparison scenarios (outline, callers, callees, deps, impact), and reports token efficiency.

```bash
# Benchmark a project
./benchmarks/bench-project.sh /path/to/project

# Test more symbols (default: 3)
./benchmarks/bench-project.sh /path/to/project -n 5

# Benchmark current directory
./benchmarks/bench-project.sh .
```

Example output on a Rust project:
```
Scenario                            |       grep |     cartog |   Saved | Note
outline: src/languages/go.rs        |    8153 tok |    1102 tok |   86.5% | structure vs full file
callers: extract                    |    6068 tok |     678 tok |   88.8% | call sites only
callees: extract                    |   23410 tok |     579 tok |   97.5% | outgoing calls
impact: default (depth 3)           |   64840 tok |       8 tok |  100.0% | transitive refs

Total: 95.6% token reduction
```

## Prerequisites

- `cartog` binary (built automatically by `run.sh` if not in PATH)
- `jq` for ground truth comparison and stats (optional but recommended)
- bash 3+ (macOS/Linux compatible)

## Output

`run.sh` prints a comparison table and saves results to `results/latest.jsonl`.

Each line is a JSON object:
```json
{"scenario":"01_find_callers","lang":"webapp_py","naive_tokens":420,"naive_recall":80.0,"best_tokens":280,"best_recall":80.0,"cartog_tokens":95,"cartog_recall":100.0}
```

`bench-project.sh` prints a summary table to stderr (no file output).
