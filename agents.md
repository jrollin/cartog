# Agent Guidelines

## Project

cartog — code graph indexer for LLM coding agents. Single Rust binary, tree-sitter parsing, SQLite storage.

See [docs/product.md](docs/product.md) for product context, [docs/tech.md](docs/tech.md) for architecture decisions, [docs/structure.md](docs/structure.md) for module layout.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build (3.5MB binary)
cargo test               # run all tests (56 unit tests)
```

## Code Conventions

- **Error handling**: `anyhow::Result` everywhere, no `unwrap()` in library code.
- **Output**: human-readable by default, `--json` flag for structured output.
- **Visibility**: all public functions get `///` doc comments.
- **Tests**: unit tests co-located in each module (`#[cfg(test)] mod tests`), integration fixtures in `tests/fixtures/`.

## Architecture

```
main.rs → cli.rs (clap) → command handlers
                         ↓
              indexer.rs (walk + extract + store)
              ├── languages/*.rs (tree-sitter extractors)
              ├── db.rs (SQLite)
              └── types.rs (shared structs)
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
git tag v0.1.0
git push origin v0.1.0
```

## Current State

- **Working**: Python, TypeScript/JavaScript, Rust, Go extractors, SQLite storage, all 9 CLI commands, incremental indexing (git-based + SHA-256 fallback), `--force` re-index flag, CI/CD pipelines
- **Pending**: Java extractor, crates.io publish
