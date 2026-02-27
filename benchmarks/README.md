# cartog Benchmark Suite

Compares cartog graph queries vs grep/cat approaches for common code navigation tasks.

## What it measures

| Dimension | Metric | Method |
|-----------|--------|--------|
| Token efficiency | Output bytes / approx tokens | Compare output size of cartog vs grep/cat for identical questions |
| Accuracy | Recall against ground truth | Count expected items found by each approach |
| Command count | Tool calls needed | How many commands each approach requires |
| Query latency | Microseconds per query | Criterion benchmarks on indexed fixture (Rust-native) |

## Fixtures

| Fixture | Language | Files | LOC |
|---------|----------|-------|-----|
| `fixtures/webapp_py/` | Python | 69 | ~4,000 |
| `fixtures/webapp_ts/` | TypeScript | 48 | ~2,500 |
| `fixtures/webapp_go/` | Go | 45 | ~3,300 |
| `fixtures/webapp_rs/` | Rust | 65 | ~3,200 |
| `fixtures/webapp_rb/` | Ruby | 51 | ~2,300 |
| **Total** | | **278** | **~15,300** |

All fixtures model the same domain (auth service, tokens, routes, middleware, database, cache, events, validators) with controlled, known relationships defined in `ground_truth/`.

## Scenarios

| # | Question | Key differentiator |
|---|----------|--------------------|
| 01 | "Who calls validate_token?" | `refs --kind calls` vs grep string match |
| 02 | "What's in auth/service?" | `outline` (structure only) vs `cat` (full file) |
| 03 | "Impact of renaming AuthService?" | `impact` (transitive BFS) vs grep (flat) |
| 04 | "Class hierarchy of BaseService?" | `hierarchy` (structured pairs) vs grep for class keywords |
| 05 | "Trace login -> DB call chain" | Sequential `callees` vs multiple grep rounds |
| 06 | "File dependencies?" | `deps` (resolved targets) vs grep for import keywords |
| 07 | "Type references to X?" | `refs --kind references` vs context-unaware grep |
| 08 | "Find all token-related symbols" | `search token` (ranked, deduplicated) vs grep (raw lines) |
| 09 | "Disambiguate 'validate' (4+ defs)" | `search validate` (distinct files) vs grep (wall of matches) |
| 10 | "High-fanout: who uses get_logger?" | `refs` (precise call sites) vs grep (every mention) |
| 11 | "Deep call chain (5+ hops)" | Sequential `callees` x6 vs 6 grep rounds with noise |
| 12 | "Deep impact at depth 5" | `impact --depth 5` (transitive BFS) vs flat grep |

## Usage

```bash
# Run all scenarios (01-12) across all 5 languages
./benchmarks/run.sh

# Run single scenario
./benchmarks/run.sh --scenario 01

# Run only one language fixture
./benchmarks/run.sh --fixture py
./benchmarks/run.sh --fixture ts
./benchmarks/run.sh --fixture go
./benchmarks/run.sh --fixture rs
./benchmarks/run.sh --fixture rb
```

## Criterion benchmarks (query latency)

Measures query latency in microseconds using Rust-native criterion benchmarks on the Python fixture indexed into an in-memory SQLite database.

```bash
# Run all query benchmarks
cargo bench --bench queries

# Run specific benchmark
cargo bench --bench queries -- search_token

# Quick run (fewer iterations)
cargo bench --bench queries -- --quick
```

Benchmarked operations: `search`, `refs`, `impact`, `outline`, `callees`, `hierarchy`, `deps`, `stats`.

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

## Fixture integrity checks

Validate that all fixture codebases compile/parse correctly:

```bash
make check-fixtures   # all fixtures (py, go, rs, rb)
make check-py         # Python: py_compile
make check-ts         # TypeScript: tsc --noEmit (requires npx)
make check-go         # Go: go build ./...
make check-rs         # Rust: cargo check
make check-rb         # Ruby: ruby -c
```

Run `make check` to also include Rust project checks (fmt + clippy + test).

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
