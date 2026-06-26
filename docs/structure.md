# cartog — Project Structure

cartog is a Cargo workspace with 10 published crates under `crates/`, plus `cartog-loom-models` (test-only, `publish = false` — Loom model-checking harnesses, empty unless built with `--cfg loom`). Each crate has its own `README.md` — this page is the navigation map. For the full tree, run `tree -L 2` from the repo root.

## Top-level layout

```text
cartog/
├── crates/             # 10 published members + cartog-loom-models (test-only; see Dependency Graph below)
├── .claude-plugin/     # Claude Code plugin manifest + hooks
├── skills/             # Agent skills (cartog, cartog-install)
├── agents/             # Autonomous agent definitions (codebase-onboarding, refactoring-scout)
├── benchmarks/         # Benchmark runner, fixtures, ground truth, 13 scenarios
├── specs/tla/          # TLA+ models of the concurrent protocols (`make tla`)
├── tests/fixtures/     # Shared fixtures referenced by crate tests
├── scripts/            # release.sh and other release/dev scripts
├── site/               # Astro project for GitHub Pages (landing page + docs; canonical install.sh in public/, served at www.cartog.dev/install.sh)
├── docs/               # This documentation
└── target/             # Cargo build output (gitignored)
```

**Generated artifacts** (gitignored): `.cartog/` (project-local index — `db.sqlite` + migration backups; legacy `.cartog.db` at the root is read for backwards-compatibility), `target/`, `benchmarks/results/`.

## Dependency graph

```text
cartog-core          (tier 0 — no internal deps)
cartog-process-lock  (tier 0 — no internal deps; libc/windows-sys only)
├── cartog-db        (tier 1)
├── cartog-languages (tier 1)
│
├── cartog-indexer   (tier 2 — db + languages + core)
├── cartog-rag       (tier 2 — db + core)
├── cartog-lsp       (tier 2 — db + core, default feature)
│
├── cartog-watch     (tier 3 — db + indexer + rag + core + process-lock)
├── cartog-mcp       (tier 3 — db + indexer + rag + watch + core + process-lock)
│
└── cartog           (tier 4 — binary, depends on all)
```

The `lsp` feature (on by default) propagates: `cartog` → `cartog-mcp` → `cartog-indexer` → `cartog-lsp`. Build with `--no-default-features` to drop the whole branch.

## Crate map

Each link goes to that crate's `README.md`, which has the detailed responsibilities.

- **[cartog-core](../crates/cartog-core/README.md)** — shared data model (`Symbol`, `Edge`, `SymbolKind`, `EdgeKind`, `Visibility`), stable ID generation, `detect_language()`.
- **[cartog-db](../crates/cartog-db/README.md)** — SQLite connection, core + RAG schema, query methods (search, refs, impact, hierarchy, callees), 6-tier edge resolution, FTS5, sqlite-vec.
- **[cartog-languages](../crates/cartog-languages/README.md)** — `Extractor` trait + 14 code extractors (Python, TypeScript (.ts, .tsx), JavaScript, Rust, Go, Ruby, Java, PHP, Dart, Swift, Kotlin, Vue, Svelte, Astro) + Markdown extractor. SFC extractors (Vue/Svelte/Astro) slice the `<script>` block and delegate to the JS/TS extractor with offset remapping.
- **[cartog-indexer](../crates/cartog-indexer/README.md)** — directory walking, layered change detection (git diff → SHA-256 → Merkle), surgical symbol-level updates. Optionally delegates to `cartog-lsp`. See [explanation/incremental-indexing.md](explanation/incremental-indexing.md).
- **[cartog-rag](../crates/cartog-rag/README.md)** — pluggable embedding providers (local ONNX, Ollama, OpenAI-compatible), hybrid search (FTS5 + vector KNN → RRF merge → cross-encoder reranking), model cache.
- **[cartog-lsp](../crates/cartog-lsp/README.md)** — LSP-based edge resolution (default feature). Spawns language servers, `textDocument/definition`, maps responses to cartog symbol IDs.
- **[cartog-watch](../crates/cartog-watch/README.md)** — debounced file watcher (`notify-debouncer-mini`), incremental re-index, deferred RAG embedding (auto-enabled when the repo already has embeddings).
- **[cartog-mcp](../crates/cartog-mcp/README.md)** — MCP server over stdio (`rmcp`). 16 tool handlers, single-writer election (primary + read-only attach + promotion).
- **[cartog-process-lock](../crates/cartog-process-lock/README.md)** — cross-platform PID-file locks (`<state_dir>/{slot}.pid`). Two-line format with `is_same_process(pid, start_time)` to close the PID-reuse window. Used by `cartog serve`, `cartog watch`, and `cartog self update`.
- **[cartog](../crates/cartog/README.md)** — binary crate: 27 top-level CLI commands via clap (including `cartog trace`/`context` for call paths and task-context bundles, `cartog push`/`pull` for S3 index sync, and `cartog self update/version/rollback/migrate-db`), config resolution, logging, tokio runtime for `cartog serve`, daily background update probe.
- **[cartog-loom-models](../crates/cartog-loom-models/README.md)** — test-only (`publish = false`), empty unless built with `--cfg loom`. Loom harnesses that model-check the in-process concurrency of `cartog-mcp`'s single-writer promoter; complements the design-level TLA+ specs in `specs/tla/`. Run via `make loom`.

## Conventions

- No `unwrap()` in library code — `anyhow::Result` for error propagation.
- Public functions documented with `///`. Crate `lib.rs` has `//!` crate docs.
- CLI output: human-readable by default, `--json` for structured output.
- Tests: unit tests in each crate (`#[cfg(test)]`), integration tests in `crates/cartog/tests/`, shared fixtures in `tests/fixtures/`.
- Benches: criterion `[[bench]]` targets in `crates/cartog/benches/` (`queries`, `rag_search`, `rag_onnx`) and `crates/cartog-indexer/benches/` (`indexing`); see [tech.md](tech.md#benchmarks). Fixtures live in `benchmarks/fixtures/`.
- Lint gate (must pass before commit): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `make check-skill`.
