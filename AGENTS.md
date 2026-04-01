# Agent Guidelines

## Project

cartog — code graph indexer for LLM coding agents. Cargo workspace (9 crates), tree-sitter parsing, SQLite storage.

See [docs/product.md](docs/product.md) for product context, [docs/tech.md](docs/tech.md) for architecture decisions, [docs/structure.md](docs/structure.md) for module layout, [docs/usage.md](docs/usage.md) for CLI commands and MCP/skill setup.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build
cargo test --workspace   # run all tests (380 tests across 9 crates)
cargo fmt --check        # check formatting
cargo clippy --all-targets -- -D warnings  # lint
```

Always run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` before committing.

```bash
cargo check --no-default-features -p cartog-rag     # verify builds without ONNX
cargo check --features provider-ollama -p cartog-rag # verify Ollama feature
```

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

See [docs/structure.md](docs/structure.md) for full directory tree and module responsibilities.

```
crates/cartog/         (binary — CLI dispatch, config)
├── cartog-core        (Symbol, Edge, SymbolKind, detect_language)
├── cartog-db          (SQLite: core + RAG schema, edge resolution)
├── cartog-languages   (tree-sitter extractors, 8 languages)
├── cartog-indexer     (walk + extract + store, Merkle hashing)
├── cartog-rag         (embeddings, hybrid search, reranker)
├── cartog-lsp         (optional LSP-based edge resolution)
├── cartog-watch       (debounced re-index + deferred RAG)
└── cartog-mcp         (MCP server over stdio, 12 tools)
```

Each language extractor implements the `Extractor` trait from `crates/cartog-languages/src/lib.rs`:
```rust
fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult>
```

Returns `Vec<Symbol>` + `Vec<Edge>`. After all files are extracted, `db.resolve_edges()` links edges by name using 6-tier priority (same file > import-path > same dir > parent scope > unique global > kind disambiguation). Runs two passes so import edges resolved in pass 1 feed import-path resolution in pass 2.

## Adding a New Language

1. Add `tree-sitter-{lang}` to `[workspace.dependencies]` in root `Cargo.toml` and to `crates/cartog-languages/Cargo.toml`
2. Create `crates/cartog-languages/src/{lang}.rs` implementing `Extractor`
3. Register in `crates/cartog-languages/src/lib.rs`: module declaration + `get_extractor()` match arm
4. Add extension mapping in `crates/cartog-core/src/lib.rs` `detect_language()`
5. Add tests using the same pattern as `python.rs` tests

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

## Documentation Convention

All documentation lives in `docs/`. No per-tool or per-client separate doc files — consolidate into the canonical files below.

| File | Scope |
|------|-------|
| `docs/product.md` | Product context, target users, differentiation |
| `docs/tech.md` | Technology stack, architecture decisions |
| `docs/structure.md` | Directory layout, module responsibilities, conventions |
| `docs/usage.md` | CLI commands, agent skill setup, MCP server setup (all clients) |
| `docs/spec-*.md` | Feature specs (design records, kept after implementation) |

### Feature Specs (`docs/spec-*.md`)

Create a spec for new features with 3+ functional requirements or cross-cutting concerns. Follow the pattern in `docs/spec-watch.md`: overview, architecture, FRs, NFRs, acceptance criteria, implementation checklist.

Do **not** create specs for bug fixes, small enhancements, or docs-only changes.

After implementation, mark checklist items complete — the spec stays as a design record.

## Current State

- **Languages**: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java
- **CLI**: 16 commands (13 top-level + 3 RAG subcommands) + MCP server (12 tools)
- **Indexing**: incremental (git-based + SHA-256 + Merkle-tree symbol diffing), `--force` re-index. Stable symbol IDs (`file:kind:qualified_name`) survive line movements. Scoped edge resolution for changed files only
- **Search**: symbol search (`cartog search`), hybrid FTS5+vector RAG search with RRF merge and cross-encoder re-ranking
- **Watch**: `cartog watch` CLI + `cartog serve --watch` background mode, debounced re-index + deferred RAG embedding
- **CI/CD**: fmt, clippy, test, coverage, release to crates.io + GitHub Releases
- **Centrality**: in-degree ranking — search results prefer highly-referenced symbols
- **Codebase map**: `cartog map --tokens N` produces budget-aware file tree + top symbols
- **Token budget**: `--tokens N` global flag for context-window-aware output truncation
- **Recent changes**: `cartog changes` shows symbols affected by recent git commits
- **AST-aware embeddings**: significant body lines (skip blanks/comments/braces) for better vector search recall
- **Embedding format versioning**: auto-detects embedding strategy changes, triggers re-embed on next `rag index`
- **Schema versioning**: metadata-based migration system for DB schema evolution
- **Pluggable embedding providers**: local ONNX (default) and Ollama, configured via `.cartog.toml`
- **Feature flags**: `provider-local` (default), `provider-ollama`
- **Pending**: Java extractor improvements
