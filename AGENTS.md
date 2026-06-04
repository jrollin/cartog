# Agent Guidelines

## Project

cartog — code graph indexer for LLM coding agents. Cargo workspace (10 crates), tree-sitter parsing, SQLite storage.

See [docs/product.md](docs/product.md) for product context, [docs/tech.md](docs/tech.md) for architecture decisions, [docs/structure.md](docs/structure.md) for module layout, [docs/usage.md](docs/usage.md) for CLI commands and MCP/skill setup.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build
cargo test --workspace   # run all tests (~600 tests across 10 crates)
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
make check                 # all checks (Rust project + fixtures + skill)
make check-rust            # cargo fmt + clippy + test
make check-fixtures        # validate all fixture codebases (py, ts, go, rs, rb, java, php, dart, swift, kt)
make check-fixtures-docker # same, forcing the Docker fallback for every language
make check-skill           # skill tests (ensure_indexed.sh unit tests)
make eval-skill            # LLM-as-judge skill evaluation (requires claude CLI)
make eval-agents           # LLM-as-judge agent evaluation (requires claude CLI)
make bench                 # shell benchmark suite (13 scenarios x 10 languages)
make bench-criterion       # ONNX-free criterion benches (queries, per-language indexing, hybrid search)
make bench-onnx            # real-model embed/rerank benches (needs `cartog rag setup`; not in CI)
make bench-rag             # RAG relevancy benchmarks (in-memory + shell scenario 13)
```

Each `check-fixtures` language target uses the native toolchain when present, else
falls back to a pinned official Docker image, else fails. `check-ts` is now part of
`make check`.

Run `make check` before committing. Run `make eval-skill` after changing skill SKILL.md or search routing. Run `make eval-agents` after changing agent definitions.

## Code Conventions

- **Error handling**: `anyhow::Result` everywhere, no `unwrap()`/`expect()` in library code. Add `.context()`/`.with_context()` so failures name what/where/why.
- **Output**: human-readable by default, `--json` flag for structured output.
- **Visibility**: all public functions get `///` doc comments.
- **Tests**: unit tests co-located in each module (`#[cfg(test)] mod tests`), integration fixtures in `tests/fixtures/`.

### Rust standard

Idiomatic-Rust expectations follow the `rust-skills` rubric (179 rules / 14 categories — own/err/mem/api/async/opt/name/type/test/doc/perf/proj/lint/anti). Don't restate it here; apply it. The points below are the ones that bind *this* codebase — keep them true on every change:

- **Borrow, don't own in signatures**: take `&[T]` not `&Vec<T>`, `&str` not `&String`. Accept `impl Into<String>` for owned-string inputs (see `Symbol::new`, `Edge::new`).
- **No panic on input**: parsing arbitrary source (`cartog-languages`) and indexing whole repos (`cartog-indexer`) must degrade — log + skip the file, never `unwrap()`/`expect()`/index-panic. Reserve `expect()` for true invariants and document why.
- **SQL is always parameterized**: rusqlite `params!` / `?` placeholders. Only ever interpolate placeholder *counts* for `IN (...)` — never a value, never an identifier from untrusted input.
- **Async (`cartog-mcp`)**: do blocking work (DB, embeddings, fs) inside `spawn_blocking`; never hold a `std::sync` lock across `.await`; use `tokio::sync` for locks that must span awaits.
- **RAII for resources**: child processes (`cartog-lsp`) and PID locks (`cartog-process-lock`) release on `Drop`. Any new owned resource gets the same.
- **`unsafe`**: only in `cartog-process-lock`; every `unsafe` fn/block carries a `# Safety` note and checks the libc/Win32 return.
- **`#[must_use]`**: put it on builder methods returning `Self` and on functions whose ignored return is always a bug.
- **Enums over stringly-typed**: prefer a `#[derive(Deserialize)]` enum to a free `String` for closed sets (config provider names, kinds) so typos fail at parse time.
- **Before committing**: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass (clippy is currently warning-clean — keep it that way).

## Architecture

See [docs/structure.md](docs/structure.md) for full directory tree and module responsibilities.

```
crates/cartog/         (binary — CLI dispatch, config, self-update)
├── cartog-core        (Symbol, Edge, SymbolKind, detect_language)
├── cartog-db          (SQLite: core + RAG schema, edge resolution)
├── cartog-languages   (tree-sitter extractors, 12 languages)
├── cartog-indexer     (walk + extract + store, Merkle hashing)
├── cartog-rag         (embeddings, hybrid search, reranker)
├── cartog-lsp         (LSP-based edge resolution — default feature)
├── cartog-watch       (debounced re-index + deferred RAG)
├── cartog-mcp         (MCP server over stdio, 16 tools)
└── cartog-process-lock (PID-file locks for serve/watch peers)
```

Each language extractor implements the `Extractor` trait from `crates/cartog-languages/src/lib.rs`:
```rust
fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult>
```

Returns `Vec<Symbol>` + `Vec<Edge>`. After all files are extracted, `db.resolve_edges()` links edges by name using 6-tier priority (same file > import-path > same dir > parent scope > unique global > kind disambiguation). Runs two passes so import edges resolved in pass 1 feed import-path resolution in pass 2. Each resolved edge records which tier/source resolved it (`edges.resolution_source`, `EdgeProvenance`); LSP-resolved edges overwrite the heuristic tag. Surfaced as `provenance` in `--json`/MCP output.

## Adding a New Language

Grammar crates must export `LANGUAGE: LanguageFn` (depend on `tree-sitter-language`),
not a legacy `language()` fn, to link against the pinned `tree-sitter` core — verify
with a one-line parse smoke test before writing the extractor.

**Core extractor:**

1. Add `tree-sitter-{lang}` to `[workspace.dependencies]` in root `Cargo.toml` and to `crates/cartog-languages/Cargo.toml`
2. Create `crates/cartog-languages/src/{lang}.rs` implementing `Extractor` (keep helpers private to the module; don't fatten `lib.rs`)
3. Register in `crates/cartog-languages/src/lib.rs`: module declaration + `get_extractor()` match arm + `test_get_extractor` assert + module-doc language list
4. Add extension mapping + a `detect_language()` test in `crates/cartog-core/src/lib.rs`
5. Add co-located tests using the same pattern as `python.rs` / `dart.rs` tests

**Edge resolution + agent integration:**

6. Add a `ServerSpec` (+ `test_find_servers_{lang}`) to `crates/cartog-lsp/src/servers.rs` for the language's LSP server
7. Add the language to the MCP "Languages:" instruction string in `crates/cartog-mcp/src/lib.rs`

**Benchmarks (parity with the other languages):**

8. Add `"{tag}"` to `FIXTURE_LANGS` in `crates/cartog-indexer/src/lib.rs` (`bench_support`)
9. Create `benchmarks/fixtures/webapp_{lang}/` mirroring the other fixtures' domain shape; add a `check-{lang}` Makefile target (native + Docker fallback) and add it to `.PHONY` + `check-fixtures`; gitignore any build dir
10. Author `benchmarks/ground_truth/webapp_{lang}.json` (derive expected values from real `cartog` output, then hand-verify) and wire `run_scenario "webapp_{lang}" ...` into all 13 `benchmarks/scenarios/NN_*.sh`; add the tag to `should_skip_fixture` in `benchmarks/lib/common.sh` and the `run.sh` usage text

**Docs & counts** (search the repo for the previous count and bump consistently — there are two conventions: marketing "N languages" = code+Markdown, and "N code languages"):

11. README, `docs/{product,structure,tech,usage}.md`, this file (AGENTS.md / CLAUDE.md), `skills/cartog/SKILL.md` + `skills/cartog/references/supported_languages.md`, and the site (`site/src/pages/index.astro` + `usage.astro`): add a `lang-tag` whose icon is an `<img>` pointing at a new `site/public/assets/{lang}.svg` brand mark (24×24, white/brand fill legible on dark), matching the existing language chips. Edit the `.astro` source only; the Pages workflow rebuilds `site/dist`. See the site-sync note under **Documentation Convention**.

## CI/CD

- **CI** (`.github/workflows/ci.yml`): runs on push/PR to `main` — check, fmt, clippy, test, coverage (cargo-llvm-cov → Codecov)
- **Release** (`.github/workflows/release.yml`): runs on tag push (`v*`) — builds binaries for 4 targets, creates GitHub Release, publishes to crates.io
- **Targets** (4): `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
- **Secrets required**: `CARGO_REGISTRY_TOKEN` (crates.io), `CODECOV_TOKEN` (Codecov)

### Release Process

```bash
./scripts/release.sh patch            # 0.1.0 → 0.1.1
./scripts/release.sh minor            # 0.1.0 → 0.2.0
./scripts/release.sh major            # 0.1.0 → 1.0.0
./scripts/release.sh 2.3.4            # set exact version
./scripts/release.sh --dry-run minor  # preview next version + changelog, change nothing
```

The script bumps `Cargo.toml`, commits, tags `vX.Y.Z`, and pushes. The release workflow then builds binaries and publishes to crates.io.

`--dry-run` prints the computed next version and the unreleased changelog (via `git-cliff`), then exits before touching the working tree, tags, or remote. It skips the main-branch and clean-tree gates since it only reads git history.

## Documentation Convention

All documentation lives in `docs/`. Consolidate into the canonical files below — do not add a new file when an existing one fits. `docs/README.md` is the index; keep it in sync when adding or moving a doc.

| File | Scope |
|------|-------|
| `docs/product.md` | Product context, target users, differentiation |
| `docs/tech.md` | Technology stack, architecture decisions, RAG design |
| `docs/structure.md` | Directory layout, module responsibilities, conventions |
| `docs/usage.md` | CLI commands, agent skill setup, configuration |
| `docs/mcp-setup.md` | Per-client MCP wiring (Cursor, VS Code, Claude Desktop, …) |
| `docs/editor-integration.md` | Non-MCP editor CLI recipes (Neovim, Emacs, …) |
| `docs/updates.md` | `cartog self update`: exit codes, env vars, state file |
| `docs/troubleshooting.md` | Common errors and fixes (the single home for these) |
| `docs/architecture/*.md` | Cross-cutting subsystem deep-dives (e.g. incremental indexing) |

The MCP config JSON has one canonical copy in `docs/mcp-setup.md`; other docs link to it rather than re-embedding it. Release runbooks (e.g. `scripts/release-smoke.md`) live with the release scripts, not in `docs/`.

`docs/architecture/*.md` is a **subsystem** explainer that spans features (the indexing pipeline, edge resolution). Use when the topic is too detailed for `tech.md` but not tied to one feature.

**The marketing site mirrors the docs and ships the same facts to users.** When you add or change a feature, command, MCP tool, language, client, config key, or count, update the site alongside the docs — it is not optional:

- `site/src/pages/index.astro` — landing page (feature cards, language/agent grids, counts, comparison tables, slogan).
- `site/src/pages/usage.astro` — the docs page that mirrors `docs/usage.md` (CLI reference, MCP tools, **every config section must have an explanation + example**, e.g. embedding, remote S3, secret redaction).

Edit the `.astro` source (never `site/dist/`, which is gitignored and rebuilt by the Pages workflow). New brand marks go in `site/public/assets/*.svg` as `<img>`-referenced files (24×24 viewBox, white/brand fill legible on the dark theme), not inline SVG. Run `npm run build` in `site/` to verify before committing.

## Current State

- **Languages**: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, PHP, Dart, Swift, Kotlin, Markdown
- **CLI**: 26 top-level commands (`init`, `ide`, `index`, `search`, `outline`, `refs`, `callees`, `impact`, `trace`, `context`, `hierarchy`, `deps`, `stats`, `savings`, `map`, `changes`, `config`, `doctor`, `watch`, `serve`, `push`, `pull`, `completions`, `manpage`, plus `rag` with 3 subcommands and `self` with 4 subcommands; `self update` has `--check`/`--defer`[`--to <version>`]/`--apply-pending` modes) + MCP server (16 tools)
- **Indexing**: incremental (git-based + SHA-256 + Merkle-tree symbol diffing), `--force` re-index. Stable symbol IDs (`file:kind:qualified_name`) survive line movements. Scoped edge resolution for changed files only
- **Search**: symbol search (`cartog search`), hybrid FTS5+vector RAG search with RRF merge and cross-encoder re-ranking
- **Watch**: `cartog watch` CLI + `cartog serve --watch` background mode, debounced re-index + deferred RAG embedding
- **MCP single-writer**: `cartog serve` instances on the same DB use atomic O_EXCL election. First is primary, subsequent attach read-only (14 of 16 tools — only the 2 DB-write tools are gated); promoter on the secondary takes over within ~10s if the primary dies. Kill switch: `CARTOG_SINGLE_WRITER=0`.
- **Deferred self-update**: inside a Claude Code session the MCP server holds the serve lock, so `cartog self update` would refuse (exit 6). `cartog self update --defer` (or the `cartog_update` MCP tool) arms a pending update without swapping; the SessionEnd hook runs `--apply-pending` once the peer exits. See [docs/updates.md](docs/updates.md).
- **CI/CD**: fmt, clippy, test, coverage, release to crates.io + GitHub Releases
- **Centrality**: in-degree ranking — search results prefer highly-referenced symbols
- **Codebase map**: `cartog map --tokens N` produces budget-aware file tree + top symbols
- **Token budget**: `--tokens N` global flag for context-window-aware output truncation
- **Recent changes**: `cartog changes` shows symbols affected by recent git commits
- **Call-path trace**: `cartog trace <from> <to>` / `cartog_trace` returns the shortest `calls` path between two symbols with each hop's body inline (forward BFS, static call edges only)
- **Task-context bundle**: `cartog context <task>` / `cartog_context` fuses hybrid search seeds + 1-hop neighbors + seed-file centrality into a token-budgeted bundle
- **Staleness banners**: when `cartog serve --watch` has pending changes/embeddings, affected MCP read-tool responses are prefixed with a `⚠️` banner; gated on a live watcher (no banner for read-only peers or watcherless serve)
- **AST-aware embeddings**: significant body lines (skip blanks/comments/braces) for better vector search recall
- **Embedding format versioning**: auto-detects embedding strategy changes, triggers re-embed on next `rag index`
- **Schema versioning**: metadata-based migration system for DB schema evolution
- **Pluggable embedding providers**: local ONNX (default) and Ollama, configured via `.cartog.toml`
- **Secret redaction**: default-on, best-effort. Scrubs common secret patterns (AWS/GitHub/Slack/Stripe/JWT + quoted key=value assignments) from `symbol_content`, `signature`, `docstring`, and embeddings; always excludes sensitive files (`.env`, `*.pem`, `id_rsa`, ...). Toggling `[security] redact_secrets` force-reindexes. See [docs/tech.md](docs/tech.md#secret-redaction)
- **Feature flags**: binary `cartog` default = `lsp` + `remote-s3` + `ollama-embedding` (all on); advanced users strip via `--no-default-features`. Runtime embedding default stays local ONNX (`provider = "local"`); Ollama is opt-in via `.cartog.toml`. Crate `cartog-rag` — `provider-local` (default), `provider-ollama`
- **Pending**: next language TBD; Java extractor improvements
