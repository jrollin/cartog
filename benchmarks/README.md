# cartog Benchmark Suite

Compares cartog graph queries vs grep/cat approaches for common code navigation tasks.

## Three benchmark surfaces

| Surface | Question it answers | Run with |
|---------|---------------------|----------|
| **Shell suite** (this file) | Is a single cartog query smaller / more complete than grep? | `make bench` |
| **Criterion micro-benchmarks** | How fast is cartog's own CPU work (µs–ms)? | `make bench-criterion` |
| **Agent-task** (`agent/`) | Does giving an agent cartog cut the *end-to-end* token cost of a task? | `make bench-agent` |

The shell suite below is **per-query** (no LLM). The
[agent-task suite](agent/README.md) drives a real agent through whole tasks with
and without cartog — the end-to-end "does it actually help" claim. See
[docs/tech.md → Benchmarks](../docs/tech.md#benchmarks) for the full
test-and-benchmark matrix.

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
| `fixtures/webapp_ts/` | TypeScript | 49 | ~2,500 |
| `fixtures/webapp_go/` | Go | 45 | ~3,300 |
| `fixtures/webapp_rs/` | Rust | 65 | ~3,200 |
| `fixtures/webapp_rb/` | Ruby | 51 | ~2,300 |
| `fixtures/webapp_java/` | Java | 41 | ~1,800 |
| `fixtures/webapp_php/` | PHP | 25 | ~1,500 |
| `fixtures/webapp_dart/` | Dart | 9 | ~200 |
| **Total** | | **354** | **~18,800** |

All fixtures model the same domain (auth service, tokens, routes, middleware, database, cache, events, validators) with controlled, known relationships defined in `ground_truth/`.

The criterion `indexing` bench exercises all 8 fixtures. The shell scenarios and
`ground_truth/` currently cover the first 7 — `webapp_dart` has no scenario
ground truth yet, so it is indexed by the criterion bench but not scored by the
shell suite.

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
| 13 | "Find authentication logic" | `rag search` (FTS5 + vector KNN + reranker) vs grep keywords |

## Index isolation

The fixtures live inside the cartog repo, so a bare `cartog index .` walks up and
writes to the repo-root `.cartog`, where every fixture would clobber the next and
recall would be measured against whichever fixture indexed last. `token_savings.sh`
and the scenarios pin `CARTOG_DB` to a per-fixture file under `benchmarks/.indexes/`
(gitignored) so each fixture stays isolated. Any new `cartog` invocation in a
scenario must do the same — use `fixture_db_path "$fixture_dir"` from `lib/common.sh`.

## Known cartog gaps

Some scenarios deliberately keep ground truth at the *objectively correct* answer
read from source, so cartog scores below 100% where its resolution is incomplete.
These rows exist to track that — they should approach parity as the gaps close:

- **PHP class inheritance (scenario 04)**: `hierarchy BaseService` returns nothing
  even though `AuthService`/`AuthenticationService`/`PaymentProcessor` extend it.
  PHP's *error* tree resolves (`TokenError -> App\AppError`), so this looks
  namespace/`use`-related rather than a total miss.
- **Rust traits / Go interfaces (excluded from scenario 04)**: Rust uses traits and
  Go uses struct embedding, not class inheritance. cartog does not model
  trait-impl or interface-satisfaction as a hierarchy, so "who implements this
  contract?" is not answerable today. Those rows are skipped rather than scored 0.
- **Dart mixins**: `hierarchy AuthResult` (sealed class) resolves, but `refs`
  on a `mixin` (e.g. `TokenCache`) does not surface the `with`-ing classes.

## Usage

```bash
# Run all scenarios (01-13) across all 10 languages
./benchmarks/token_savings.sh

# Run single scenario
./benchmarks/token_savings.sh --scenario 01

# Run only one language fixture
./benchmarks/token_savings.sh --fixture py
./benchmarks/token_savings.sh --fixture ts
./benchmarks/token_savings.sh --fixture go
./benchmarks/token_savings.sh --fixture rs
./benchmarks/token_savings.sh --fixture rb
./benchmarks/token_savings.sh --fixture java
./benchmarks/token_savings.sh --fixture php
```

## Edge-resolution rate

`resolution_rate.sh` measures resolved / total edges per language (a different
axis from token savings: how completely the resolver links call/import/type
edges). Re-run after extractor or resolver changes.

```bash
./benchmarks/resolution_rate.sh                 # heuristic, all langs, save snapshot
./benchmarks/resolution_rate.sh --lsp           # add LSP pass (uses installed servers)
./benchmarks/resolution_rate.sh --fixture rs    # one language
./benchmarks/resolution_rate.sh --baseline      # diff vs last snapshot (no overwrite)
```

## Reproducing the numbers

Both result files now carry provenance so a published number can be traced back
to an exact build:

- `results/resolution_rate{,_lsp}.json` — top-level `cartog_version`, `git_sha`,
  `timestamp`, plus per-language `lsp_source` (`host:<bin>` or `none`).
- `results/latest.jsonl` — first line is a `{"_meta": {...}}` header with the
  same `cartog_version` / `git_sha` / `timestamp` (the summary loop skips it).

To reproduce:

1. Build the pinned commit: `cargo build --release` (the scripts prefer
   `target/release/cartog`; override with `CARTOG=/path/to/cartog`).
2. Run the script. Provenance is captured automatically.

**Host-independent LSP via Docker.** The `--lsp` numbers otherwise depend on
which servers happen to be installed (Go needs a toolchain, Ruby ≥3.2, etc.).
To pin a server, build its image and point `[lsp.<lang>]` at it — see
`lsp-images/dart.Dockerfile` and the "LSP server overrides" section in
[docs/usage.md](../docs/usage.md). Example for Dart:

```bash
docker build -t cartog-lsp-dart:stable -f benchmarks/lsp-images/dart.Dockerfile benchmarks/lsp-images
# then add the [lsp.dart] block from docs/usage.md to a .cartog.toml and run:
CARTOG_DB=/tmp/dart.sqlite cartog index --force benchmarks/fixtures/webapp_dart
```

Only the Dart image ships today. Docker images for the other 9 languages, a
`--docker-lsp` script mode, and a CI job are follow-ups, not yet wired.

## Criterion benchmarks (in-process latency)

Rust-native criterion benchmarks measure cartog's own CPU-bound work against the
`benchmarks/fixtures/` corpora indexed into in-memory SQLite. They are split into
four `[[bench]]` targets so the ONNX boundary is expressed by target membership
(see [docs/tech.md](../docs/tech.md#benchmarks) for the full rationale). Inputs and
results are wrapped in `black_box`, so the µs-scale benches measure real work.

```bash
# Everything ONNX-free (queries + per-language indexing + hybrid search)
make bench-criterion

# Real-model embed/rerank — needs `cartog rag setup`, not run in CI
make bench-onnx
```

### `queries` — query latency (`cartog`)

Microsecond-scale latency for `search`, `refs`, `impact`, `outline`, `callees`,
`hierarchy`, `deps`, `stats` on the Python and Java fixtures. Query latency is
language-agnostic (same SQL regardless of source language), so two fixtures suffice.

```bash
cargo bench -p cartog --bench queries
cargo bench -p cartog --bench queries -- search_token   # one bench (substring match)
```

### `indexing` — per-language indexing (`cartog-indexer`)

Lives in `cartog-indexer`, which has no `cartog-rag`/ONNX dependency, so it builds
and runs without the native ONNX library. Per-language cost lives in the
tree-sitter grammar + extractor, so the full-index scenario is parameterized over
all 8 fixtures.

```bash
cargo bench -p cartog-indexer --bench indexing
cargo bench -p cartog-indexer --bench indexing -- index_full_force/rs   # one language
```

| Benchmark | What it measures |
|-----------|-----------------|
| `index_full_force/<lang>` | Full index of each fixture (force=true) — `py ts go rs rb java php dart` |
| `index_incremental_noop` | Re-index with no changes (all files skipped via hash); Python |
| `index_incremental_one_file` | One file's hash invalidated, triggers Merkle diff + scoped resolution; Python |

### `rag_search` — hybrid search (`cartog`)

`hybrid_search` (FTS5 + vector KNN + RRF merge) over the embedded Python fixture,
using a deterministic stub embedding provider — no ONNX model is loaded, so it runs
in CI.

```bash
cargo bench -p cartog --bench rag_search
```

### `rag_onnx` — real embedding + reranking (`cartog`, opt-in)

Loads the actual fastembed/ONNX models to measure `embed_query`,
`embed_documents`, and cross-encoder `rerank`. **Not run in CI**; requires the
models on disk (`cartog rag setup`) and skips gracefully if they are absent.

```bash
make bench-onnx   # or: cargo bench -p cartog --bench rag_onnx
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

## Fixture integrity checks

Validate that all fixture codebases compile/parse correctly:

```bash
make check-fixtures   # all fixtures (py, ts, go, rs, rb, java, php)
make check-py         # Python: py_compile
make check-ts         # TypeScript: tsc --noEmit (requires npx)
make check-go         # Go: go build ./...
make check-rs         # Rust: cargo check
make check-rb         # Ruby: ruby -c
make check-java       # Java: javac
make check-php        # PHP: php -l
```

Run `make check` to also include Rust project checks (fmt + clippy + test).

## Prerequisites

- `cartog` binary (built automatically by `token_savings.sh` if not in PATH)
- `jq` for ground truth comparison and stats (optional but recommended)
- bash 3+ (macOS/Linux compatible)

## Output

`token_savings.sh` prints a comparison table and saves results to `results/latest.jsonl`.

Each line is a JSON object:
```json
{"scenario":"01_find_callers","lang":"webapp_py","naive_tokens":420,"naive_recall":80.0,"best_tokens":280,"best_recall":80.0,"cartog_tokens":95,"cartog_recall":100.0}
```

`bench-project.sh` prints a summary table to stderr (no file output).
