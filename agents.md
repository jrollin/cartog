# Agent Guidelines

## Project

cartog — code graph indexer for LLM coding agents. Single Rust binary, tree-sitter parsing, SQLite storage.

See [docs/product.md](docs/product.md) for product context, [docs/tech.md](docs/tech.md) for architecture decisions, [docs/structure.md](docs/structure.md) for module layout.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build
cargo test               # run all tests (271 tests)
cargo fmt --check        # check formatting
cargo clippy --all-targets -- -D warnings  # lint
```

Always run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` before committing.

### Integrity checks

```bash
make check            # all checks (Rust project + fixtures + skill)
make check-rust       # cargo fmt + clippy + test
make check-fixtures   # validate all 4 fixture codebases (py, go, rs, rb)
make check-skill      # skill tests (ensure_indexed.sh unit tests)
make check-ts         # TypeScript fixtures (requires npx/tsc)
make eval-skill       # LLM-as-judge skill evaluation (requires claude CLI)
make bench            # shell benchmark suite (13 scenarios x 5 languages)
make bench-criterion  # Rust criterion benchmarks (query latency)
make bench-rag        # RAG relevancy benchmarks (in-memory + shell scenario 13)
```

Run `make check` before committing. Run `make eval-skill` after changing skill SKILL.md or search routing.

## Code Conventions

- **Error handling**: `anyhow::Result` everywhere, no `unwrap()` in library code.
- **Output**: human-readable by default, `--json` flag for structured output.
- **Visibility**: all public functions get `///` doc comments.
- **Tests**: unit tests co-located in each module (`#[cfg(test)] mod tests`), integration fixtures in `tests/fixtures/`.

## Architecture

```
main.rs → cli.rs (clap) → command handlers (sync)
                         ↓
              indexer.rs (walk + extract + store + symbol content)
              ├── languages/*.rs (tree-sitter extractors)
              ├── db.rs (SQLite: core schema + RAG schema)
              └── types.rs (shared structs)

         → Rag → rag/setup.rs (model download via fastembed auto-download)
              → rag/embeddings.rs (ONNX Runtime inference via fastembed)
              → rag/indexer.rs (embed symbols → sqlite-vec)
              → rag/search.rs (FTS5 + vector KNN → RRF merge)
              → rag/reranker.rs (cross-encoder re-ranking via fastembed)

         → Watch → watch.rs (file watcher, debounced re-index + deferred RAG)
              ├── notify-debouncer-mini (kqueue/inotify/ReadDirectoryChangesW)
              ├── WatchConfig (debounce, rag, rag_delay)
              └── run_watch() / spawn_watch() (foreground / background)

         → Serve → mcp.rs (MCP server over stdio, async via tokio)
              ├── CartogServer (11 tool handlers: 9 core + 2 RAG)
              ├── Path validation (CWD subtree restriction)
              ├── --watch flag → spawn_watch() background thread
              └── spawn_blocking → db.rs / indexer.rs / rag (sync)
```

Each language extractor implements the `Extractor` trait from `src/languages/mod.rs`:
```rust
fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult>
```

Returns `Vec<Symbol>` + `Vec<Edge>`. After all files are extracted, `db.resolve_edges()` links edges by name (same file > same dir > unique project match).

## Adding a New Language

1. Add `tree-sitter-{lang}` to `Cargo.toml`
2. Create `src/languages/{lang}.rs` implementing `Extractor`
3. Register in `src/languages/mod.rs`: extension mapping + `get_extractor()` match arm
4. Add tests using the same pattern as `python.rs` tests

## CI/CD

- **CI** (`.github/workflows/ci.yml`): runs on push/PR to `main` — check, fmt, clippy, test, coverage (cargo-llvm-cov → Codecov)
- **Release** (`.github/workflows/release.yml`): runs on tag push (`v*`) — builds binaries for 5 targets, creates GitHub Release, publishes to crates.io
- **Targets**: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
- **Secrets required**: `CARGO_REGISTRY_TOKEN` (crates.io), `CODECOV_TOKEN` (Codecov)

### Release Process

```bash
./scripts/release.sh patch   # 0.1.0 → 0.1.1
./scripts/release.sh minor   # 0.1.0 → 0.2.0
./scripts/release.sh major   # 0.1.0 → 1.0.0
./scripts/release.sh 2.3.4   # set exact version
```

The script bumps `Cargo.toml`, commits, tags `vX.Y.Z`, and pushes. The release workflow then builds binaries and publishes to crates.io.

## Current State

- **Working**: Python, TypeScript/JavaScript, Rust, Go, Ruby extractors, SQLite storage, all 9 CLI commands + MCP server (`cartog serve`, 11 tools: 9 core + 2 RAG), incremental indexing (git-based + SHA-256 fallback), `--force` re-index flag, CI/CD pipelines, `EdgeKind::References` extraction (type annotations, decorators, exception types, composite literals, `new` expressions, rescue clause types), symbol search (`cartog search`), RAG semantic search (`cartog rag` subcommand group: setup/index/search), hybrid FTS5+vector search with RRF merge, fastembed ONNX Runtime embeddings (`BAAI/bge-small-en-v1.5`), sqlite-vec vector storage, cross-encoder re-ranking (`BAAI/bge-reranker-base` via fastembed, batch scoring, auto-enabled when model downloaded via `cartog rag setup`), shared model cache (`~/.cache/cartog/models`, XDG-compliant), file watcher (`cartog watch` CLI + `cartog serve --watch` background mode, debounced re-index + deferred RAG embedding)
- **Pending**: Java extractor
