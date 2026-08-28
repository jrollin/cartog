# cartog — Technology Stack

## Language

**Rust** — Performance, tree-sitter native bindings, single binary distribution.

## Core Dependencies

| Crate | Purpose | Notes |
|-------|---------|-------|
| `tree-sitter` 0.26 | Incremental parsing, CST traversal | Pinned — grammar crates lag by one minor |
| `tree-sitter-{lang}` 0.2–1.0 | Per-language grammars (Python, TS/JS, Rust, Go, Ruby, Java, C, C++, C#, PHP, Dart, Swift, Kotlin, Vue, Svelte, Astro, Markdown) | Each ~1-2 MB of generated C |
| `rusqlite` (bundled) | SQLite storage, zero external deps | `bundled` compiles SQLite from C source — no system `libsqlite3-dev` required. Critical for cross-compilation to 4 release targets |
| `clap` (derive) | CLI argument parsing | `ValueEnum` derive for type-safe `--kind` filters with shell completion |
| `serde` + `serde_json` | JSON serialization for `--json` output | `to_string_pretty` for readability in both terminal and agent contexts |
| `walkdir` | Recursive directory traversal | `follow_links(true)` — enables symlinked monorepo layouts |
| `sha2` | SHA-256 content hashing | Change detection fallback when git is unavailable |
| `notify` + `notify-debouncer-mini` | Filesystem event watching | "mini" debouncer chosen over full — fewer deps, simpler API, sufficient for single-project use |
| `ctrlc` | Graceful Ctrl+C handling | Flushes pending RAG embeddings before exit |
| `anyhow` | Error propagation | `Result<T>` everywhere, no `unwrap()` in library code |
| `rmcp` (server + transport-io) | MCP server over stdio | Server-only — cartog is never an MCP client. stdio transport matches how agents launch subprocesses |
| `tokio` (rt-multi-thread) | Async runtime for MCP server only | Multi-thread for `spawn_blocking` throughput. Runtime created on-demand — sync commands skip it entirely |
| `tracing` + `tracing-subscriber` | Structured logging to stderr | Logs to stderr so stdout stays clean for output and MCP protocol |
| `fastembed` | ONNX Runtime inference for embeddings + re-ranking (local provider) | Optional via `provider-local` feature (default on). `default-features = false` drops image models (CLIP etc.). `rustls-tls` avoids OpenSSL system dependency |
| `reqwest` | HTTP client for self-update + remote embedding providers (Ollama, OpenAI) | Non-optional in the `cartog` binary (self-update). The Ollama (`provider-ollama`) and OpenAI-compatible (`provider-openai`) providers in `cartog-rag` are both enabled by default (`ollama-embedding` / `openai-embedding`). The OpenAI provider reads its API key from an env var (`api_key_env`, default `OPENAI_API_KEY`), never from `.cartog.toml`; an unset key sends no auth header (keyless local `/v1` endpoints). Uses `blocking` + `rustls-tls` |
| `sqlite-vec` | Vector similarity search (KNN) in SQLite | `vec0` virtual table, requires integer rowids (bridged via `symbol_embedding_map`) |
| `criterion` (dev) | Micro-benchmarks | Four `[[bench]]` targets — see [Benchmarks](#benchmarks). Inputs/results are `black_box`-wrapped so µs-scale benches measure real work |
| `rust-s3` 0.37 (`tokio-rustls-tls`) | S3-compatible client for `cartog push` / `cartog pull` | Optional via `remote-s3` feature (default on). Chosen over `aws-sdk-s3` for size (~5 MB vs ~18 MB); supports AWS S3, MinIO, R2, floci |

## Build Profiles

| Setting | Value | Rationale |
|---------|-------|-----------|
| `dev.opt-level` | `1` | Tree-sitter C grammars are machine-generated huge files that compile very slowly at opt-level 0. Level 1 also makes indexing usably fast during development |
| `release.lto` | `"thin"` | Most binary size / performance benefits of full LTO at a fraction of the link time |
| `release.strip` | `"debuginfo"` | Removes DWARF sections (~50% binary size reduction) but keeps function names in panic backtraces for diagnosable crash reports |

## Test & benchmark matrix

Every measurement surface, what it uniquely covers, and where surfaces
deliberately reuse each other. Kept here so new tests land in the right place
instead of duplicating an existing one.

| Surface | Question | Command | Overlap note |
|---------|----------|---------|--------------|
| Unit tests (`#[cfg(test)]`) | Does a single fn/module work? | `cargo test` | finest grain |
| Integration tests (`crates/*/tests/`) | Do subsystems compose (init, remote, ide, locks, self-update, watch)? | `cargo test` | each owns one subsystem |
| RAG relevancy (`rag_relevancy.rs`) | Does hybrid search return the right symbols? | `cargo test --test rag_relevancy` | reused by `bench-rag` |
| Fixture integrity | Do the 10 fixture codebases compile/parse? | `make check-fixtures` | validates inputs, not cartog |
| Skill shell tests | Does `ensure_indexed.sh`/`install.sh` logic hold? | `make check-skill` | bash-level |
| Skill eval | Does the agent pick the **right cartog command first**? (tools off) | `make eval-skill` | LLM judge, tool-selection only |
| Agent eval | Same, for the agent definitions | `make eval-agents` | LLM judge, tool-selection only |
| Criterion benches | How fast is cartog's own CPU work (µs–ms)? | `make bench-criterion` | in-process latency |
| Idle memory guard | Does an idle `cartog serve` hold a model it was never asked to use? | `make bench-memory` | macOS `footprint` / Linux `smaps_rollup`; runs in CI |
| Shell suite | Is one cartog query smaller/more complete than one grep? (**per-query**, no LLM) | `make bench` | token + recall |
| Agent-task | Does giving an agent cartog cut **end-to-end** task cost? (LLM, with/without arms) | `make bench-agent` | task outcome + cost |

The skill/agent **evals** judge *tool-selection* with tools disabled; the
**agent-task benchmark** judges *task outcome and cost* with tools enabled and a
baseline arm — they share `scripts/lib/llm_judge.sh` (one judge invocation) but
test different things. The shell suite and agent-task suite share the
token-savings *theme* but answer different questions (per-query size vs
end-to-end task cost).

## Benchmarks

Three distinct surfaces, all rooted in `benchmarks/fixtures/` (10 code-language webapps + 3 SFC apps: vue/svelte/astro):

- **Shell suite** (`benchmarks/token_savings.sh`, 13 scenarios × fixtures) — per-query
  token efficiency and recall versus grep/cat. Run with `make bench`.
- **Edge-resolution rate** (`benchmarks/resolution_rate.sh`) — resolved / total edges
  per language, heuristic-only by default or `--lsp`; `--baseline` diffs against the
  last snapshot. Run after extractor/resolver changes.
- **Agent-task** (`benchmarks/agent/run.sh`) — end-to-end agent token + turn cost
  with cartog on vs off, median over N runs, LLM-judged for correctness. The
  "does it actually help an agent" claim. Run with `make bench-agent`; not in CI
  (spends model tokens). See [benchmarks/agent/README.md](../benchmarks/agent/README.md).
- **Criterion micro-benchmarks** — in-process latency. The guiding rule: benchmark
  cartog's own CPU-bound work; anything dominated by an external service (the
  Ollama daemon, S3) gets a *correctness* test at the boundary, not a latency
  bench, so numbers never measure infrastructure.

Criterion benches are split into four `[[bench]]` targets so the ONNX boundary is
expressed by target membership — CI runs the three runtime-ONNX-free targets and
simply never names the fourth (criterion's regex filter cannot express exclusion):

| Target | Crate | Scope | Runtime ONNX | CI |
|--------|-------|-------|--------------|-----|
| `queries` | `cartog` | 8 query ops (search/refs/impact/outline/callees/hierarchy/deps/stats), Python + Java | no | ✅ |
| `indexing` | `cartog-indexer` | `index_full_force/<lang>` over all 13 fixtures + 2 incremental scenarios | no (crate has no `cartog-rag` dep) | ✅ |
| `rag_search` | `cartog` | `hybrid_search` (FTS5 + vector KNN + RRF) via a deterministic stub provider | no (stub vectors) | ✅ |
| `rag_onnx` | `cartog` | real fastembed embed + cross-encoder rerank | **yes** | ❌ opt-in (`make bench-onnx`) |

Conventions: every `b.iter` input and result is wrapped in `std::hint::black_box`
so the compiler cannot constant-fold literal inputs or eliminate unused results —
without it the µs-scale query benches would risk measuring nothing. Query latency
is language-agnostic (same SQL regardless of source language), so it is benched on
Python + Java only; per-language cost lives in the tree-sitter grammar/extractor,
so `index_full_force` is parameterized across all 13 fixtures. The shared scenario
bodies live in `cartog_indexer::bench_support` so `queries` and `indexing` cannot
drift. On PRs the CI `bench` job establishes a same-runner baseline at the merge
base and reports a `--baseline` delta (controlling for runner variance); it is
`continue-on-error`, so a noisy result never blocks. Run everything ONNX-free
locally with `make bench-criterion`.

### Token-savings accounting

`cartog stats --savings` / `cartog savings` reports cumulative tokens saved by
serving graph queries instead of grep+read. The accounting is deliberately
coarse and honest:

- **Method**: `savings_breakdown` counts rows in the local `query_log` table and
  multiplies by two flat per-query constants — `TOKENS_PER_QUERY_CARTOG` (~280)
  and `TOKENS_PER_QUERY_GREP` (~1,700), defined in `cartog-db/src/lib.rs`. Saved
  = `count × (1,700 − 280)`, ≈83% of the grep baseline. It is **not** a per-call
  measurement of actual response size — every tool uses the same multiplier, so
  the by-tool breakdown shows *which* navigation patterns are used, not which
  saved the most.
- **Provenance**: the two constants are the aggregate averages from the
  13-scenario × multi-language shell suite (`benchmarks/token_savings.sh`). The
  headline "83% fewer tokens" in [product.md](product.md) is the same figure, so
  the runtime report and the marketing claim stay consistent.
- **Always-on, not opt-in**: logging runs unconditionally because it is
  metadata-only — each row stores the tool name, call surface (`cli`/`mcp`), and
  a unix timestamp. **No query payload is ever recorded and nothing leaves the
  machine** (see the no-payload note in [usage.md](usage.md#cartog-savings)).
  Because there is no privacy surface to gate, there is no `.cartog.toml` opt-in
  switch; read-only MCP attaches skip the write entirely (so multi-server setups
  undercount rather than double-count).

## Architecture Decisions

> Moved to **[explanation/architecture.md](explanation/architecture.md)** — design decisions table, secret redaction, SQLite tuning, MSRV.

## RAG Pipeline Design

> Moved to **[explanation/rag-pipeline.md](explanation/rag-pipeline.md)** — why small models, what gets embedded, FTS5+vector+RRF+reranker pipeline, key constants, provider architecture.

## Further Reading

- [Product Overview](product.md) — purpose, target users, differentiation
- [Project Structure](structure.md) — module layout, conventions
- [CLI Reference](reference/cli.md) — all commands
- [Configuration Reference](reference/config.md) — all `.cartog.toml` keys
- [Architecture Decisions](explanation/architecture.md) — design decisions, secret redaction, SQLite tuning
- [RAG Pipeline](explanation/rag-pipeline.md) — search pipeline, constants, provider architecture
- [Incremental Indexing](explanation/incremental-indexing.md) — three-layer change detection
- [Concurrency](explanation/concurrency.md) — rayon, tokio, std::thread models
