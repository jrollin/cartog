# Agent Guidelines

## Project

cartog — code graph indexer for LLM coding agents. Cargo workspace (10 published crates + `cartog-loom-models`, a test-only model-checking crate), tree-sitter parsing, SQLite storage.

See [docs/product.md](docs/product.md) for product context, [docs/tech.md](docs/tech.md) for architecture decisions, [docs/structure.md](docs/structure.md) for module layout, [docs/usage.md](docs/usage.md) for CLI commands and MCP/skill setup.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build
cargo test --workspace   # run all tests (~600 tests across the 10 published crates)
cargo fmt --check        # check formatting
cargo clippy --all-targets -- -D warnings  # lint
```

Always run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` before committing.

```bash
cargo check --no-default-features -p cartog-rag     # verify builds without ONNX
cargo check --features provider-ollama -p cartog-rag # verify Ollama feature
cargo check --features provider-openai -p cartog-rag # verify OpenAI feature
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
make bench                 # shell benchmark suite (13 scenarios x 12 languages)
make bench-resolution      # edge-resolution rate (heuristic + host LSP, all languages; saves a provenance snapshot)
make bench-resolution-docker # same, LSP servers via Docker images (all 16, strict — no host fallback); `make lsp-images` builds them
make bench-resolution-scale # synthetic N-vs-2N repo, asserts index time stays near-linear (quadratic-regression guard, cf. #110)
make bench-memory          # guards `cartog serve` idle footprint (macOS `footprint` / Linux `smaps_rollup`; in CI)
make bench-criterion       # ONNX-free criterion benches (queries, per-language indexing, hybrid search)
make bench-onnx            # real-model embed/rerank benches (needs `cartog rag setup`; not in CI)
make bench-rag             # RAG relevancy benchmarks (in-memory + shell scenario 13)
```

Each `check-fixtures` language target uses the native toolchain when present, else
falls back to a pinned official Docker image, else fails. `check-ts` is now part of
`make check`.

```bash
claude plugin validate .   # validate the Claude plugin manifest (.claude-plugin/) — run after every plugin update
```

Run `make check` before committing. Run `make eval-skill` after changing skill SKILL.md or search routing. Run `make eval-agents` after changing agent definitions. Run `claude plugin validate .` every time the Claude plugin is updated.

## Code Conventions

- **Error handling**: `anyhow::Result` everywhere, no `unwrap()`/`expect()` in library code. Add `.context()`/`.with_context()` so failures name what/where/why.
- **Output**: human-readable by default, `--json` flag for structured output.
- **Visibility**: all public functions get `///` doc comments.
- **Dependencies**: vet before adding — a new crate must clear `cargo deny check` (license/ban/source policy in `deny.toml`) and carry no open `cargo audit` advisory; surface any CVE to a human before it lands. Prefer the workspace's existing deps and pin via `[workspace.dependencies]`; both checks gate CI (see **CI/CD**).
- **Tests**: unit tests co-located in each module (`#[cfg(test)] mod tests`), integration fixtures in `tests/fixtures/`. Every new behavior or bug fix ships a test in the same change — a bug fix starts with a failing regression test; assert public behavior, not private internals. Run the touched crate's suite green before committing (`cargo test -p <crate>`, or `--workspace` for cross-crate edges).
- **Docs + site sync (mandatory)**: any change to a CLI command/flag, an MCP tool, a `.cartog.toml` config key, a supported language, or a user-facing count must update `docs/` **and** the marketing site (`site/src/pages/{index,usage}.astro`) in the same change — a new/changed config key needs both its `[section]` summary-table row and an explanation+example block on `usage.astro`. The site is not optional and not a follow-up. See **Documentation Convention** for the full surface + the `init.rs` template.

### Rust standard

Idiomatic-Rust expectations follow the `rust-skills` rubric (179 rules / 14 categories — own/err/mem/api/async/opt/name/type/test/doc/perf/proj/lint/anti). Don't restate it here; apply it. (`rust-skills`, `rust-decompose`, and `rust-hexagonal` are optional skills that teach these in depth — the bullets below bind whether or not you have them.) The points below are the ones that bind *this* codebase — keep them true on every change:

- **Borrow, don't own in signatures**: take `&[T]` not `&Vec<T>`, `&str` not `&String`. Accept `impl Into<String>` for owned-string inputs (see `Symbol::new`, `Edge::new`).
- **No panic on input**: parsing arbitrary source (`cartog-languages`) and indexing whole repos (`cartog-indexer`) must degrade — log + skip the file, never `unwrap()`/`expect()`/index-panic. Reserve `expect()` for true invariants and document why.
- **SQL is always parameterized**: rusqlite `params!` / `?` placeholders. Only ever interpolate placeholder *counts* for `IN (...)` — never a value, never an identifier from untrusted input.
- **Async (`cartog-mcp`)**: do blocking work (DB, embeddings, fs) inside `spawn_blocking`; never hold a `std::sync` lock across `.await`; use `tokio::sync` for locks that must span awaits.
- **RAII for resources**: child processes (`cartog-lsp`) and PID locks (`cartog-process-lock`) release on `Drop`. Any new owned resource gets the same.
- **`unsafe`**: confined to two sites — the libc/Win32 PID-lock calls in `cartog-process-lock`, and the one-time sqlite-vec extension registration in `cartog-db` (`register_sqlite_vec`, an FFI fn-pointer transmute). Every `unsafe` fn/block carries a `# Safety` note; syscall sites check the libc/Win32 return. Don't add a third without the same.
- **`#[must_use]`**: put it on builder methods returning `Self` and on functions whose ignored return is always a bug.
- **Enums over stringly-typed**: prefer a `#[derive(Deserialize)]` enum to a free `String` for closed sets (config provider names, kinds) so typos fail at parse time.
- **Decompose as you write** (not in a later cleanup pass): one file = one concern (a multi-concern slice is a directory module with a logic-free `mod.rs` that only declares + re-exports — see `crates/cartog/src/config/`); a function with named phases is a thin orchestrator + one fn per phase (see `index_directory` over `crates/cartog-indexer/src/{walk,pass}.rs`). The transaction/lock/`&mut` accumulator that spans the op **stays in the orchestrator** — phase fns take `&Database` and write via `*_in_tx` helpers, never own the guard across the call. Tests split by concern under `tests/`. When a split moves an invariant across a fn boundary, add a seam test (accumulator folds across all files; rollback spans every phase). *If you have the `rust-decompose` skill it covers this in depth; the rule binds with or without it.*
- **Comments explain why, not what**: add a comment only when the intent is non-obvious (a rate limit, an ordering invariant, a workaround). Keep it to one terse line; never narrate what the code already says, never leave multi-paragraph prose. Delete dead code — don't comment it out — and no `TODO`/`FIXME` without a linked issue or explicit deferral note.
- **Performance is part of correctness here** (this indexes whole repos): never run a per-row or per-edge query inside a loop — batch it, or push the set into one `IN (SELECT …)` (see `crates/cartog-db/src/store/queries.rs`). Nothing on the index/resolve path may be O(symbols²) or O(edges²); `make bench-resolution-scale` is the near-linear guard (cf. #110). Memory is part of this too: a long-lived `serve` must not hold a model it hasn't been asked to use — `make bench-memory` guards the idle footprint (macOS + Linux, and a CI job), since several open projects each paying for an eager ONNX load dominated a machine's committed memory. The in-crate `reranker_is_loaded` assertions pin the same invariant on every platform `cargo test` runs on. Borrow over clone in hot paths (`&str`/`&[T]`, no per-iteration allocation). When you change an indexing or query hot path, add or re-run the matching criterion bench (`make bench-criterion`, `crates/*/benches/`) so a regression shows up.
- **Measure before and after for any performance change**: capture the baseline on the *unchanged* code first (criterion bench, `make bench-resolution-scale`, or a timed `cartog index` on a real repo), then the same measurement after, and report the before→after delta as the evidence the change worked — no "should be faster" without numbers. A perf claim with no measured comparison is not done.
- **Before committing**: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass (clippy is currently warning-clean — keep it that way).

## Architecture

See [docs/structure.md](docs/structure.md) for full directory tree and module responsibilities.

```
crates/cartog/         (binary — CLI dispatch, config, self-update)
├── cartog-core        (Symbol, Edge, SymbolKind, detect_language)
├── cartog-db          (SQLite: core + RAG schema, edge resolution)
├── cartog-languages   (tree-sitter extractors, 18 languages + SFC/JSX)
├── cartog-indexer     (walk + extract + store, Merkle hashing)
├── cartog-rag         (embeddings, hybrid search, reranker)
├── cartog-lsp         (LSP-based edge resolution — default feature)
├── cartog-watch       (debounced re-index + deferred RAG)
├── cartog-mcp         (MCP server over stdio, 16 tools)
└── cartog-process-lock (PID-file locks for serve/watch peers)

cartog-loom-models     (test-only, not published — Loom model-checking
                        harnesses for the in-process concurrency in
                        cartog-mcp; see `make loom` and specs/tla/)
```

Each language extractor implements the `Extractor` trait from `crates/cartog-languages/src/lib.rs`:
```rust
fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult>
```

Returns `Vec<Symbol>` + `Vec<Edge>`. After all files are extracted, `db.resolve_edges()` links edges by name using 6-tier priority (same file > import-path > same dir > parent scope > unique global > kind disambiguation). Runs two passes so import edges resolved in pass 1 feed import-path resolution in pass 2. Each resolved edge records which tier/source resolved it (`edges.resolution_source`, `EdgeProvenance`); LSP-resolved edges overwrite the heuristic tag. Surfaced as `provenance` in `--json`/MCP output.

## Adding a New Language

Grammar crates should export `LANGUAGE: LanguageFn` (depend on `tree-sitter-language`)
to link against the pinned `tree-sitter` core. A legacy `language()` fn also works
*iff* the crate pins the same core (e.g. `tree-sitter-vue-updated` returns a `Language`
from core 0.26 directly — call `language()`, don't wrap in `Language::new`); an old
core yields a different `tree_sitter::Language` type that won't typecheck. Verify with
a one-line parse smoke test against the workspace core before writing the extractor.

**Core extractor:**

1. Add `tree-sitter-{lang}` to `[workspace.dependencies]` in root `Cargo.toml` and to `crates/cartog-languages/Cargo.toml`
2. Create `crates/cartog-languages/src/{lang}.rs` implementing `Extractor` (keep helpers private to the module; don't fatten `lib.rs`)
3. Register in `crates/cartog-languages/src/lib.rs`: module declaration + `get_extractor()` match arm + `test_get_extractor` assert + module-doc language list
4. Add extension mapping + a `detect_language()` test in `crates/cartog-core/src/lib.rs`
5. Add co-located tests using the same pattern as `python.rs` / `dart.rs` tests

**Edge resolution + agent integration:**

6. Add a `ServerSpec` (+ `test_find_servers_{lang}`) to `crates/cartog-lsp/src/servers.rs` for the language's LSP server, and a matching pinned `benchmarks/lsp-images/{lang}.Dockerfile` (its `ENTRYPOINT` must reproduce the `ServerSpec` args; `docker run` uses `-i`, never `-t`) so `resolution_rate.sh --docker-lsp` covers it
   - **Compiler-only exception:** `benchmarks/lsp-images/kotlinc.Dockerfile` is NOT an LSP-server image and does NOT follow the `ServerSpec` convention — it ships the Kotlin *compiler* (`kotlinc`) for the `check-kt` fixture build, has **no `ENTRYPOINT`** (the `check-kt` make target invokes `kotlinc` explicitly), and is excluded from the `lsp-images` build glob. `check-kt`'s `docker run` does **not** pass `-i` (a one-shot compile reads source files, not stdio, so no stdin attach is needed) and, like the LSP images, never `-t`.
7. Add the language to the MCP "Languages:" instruction string in `crates/cartog-mcp/src/lib.rs`

**Benchmarks (parity with the other languages):**

8. Add `"{tag}"` to `FIXTURE_LANGS` in `crates/cartog-indexer/src/lib.rs` (`bench_support`)
9. Create `benchmarks/fixtures/webapp_{lang}/` mirroring the other fixtures' domain shape; add a `check-{lang}` Makefile target (native + Docker fallback) and add it to `.PHONY` + `check-fixtures`; gitignore any build dir
10. Author `benchmarks/ground_truth/webapp_{lang}.json` (derive expected values from real `cartog` output, then hand-verify) and wire `run_scenario "webapp_{lang}" ...` into all 13 `benchmarks/scenarios/NN_*.sh`; add the tag to `should_skip_fixture` in `benchmarks/lib/common.sh` and the `token_savings.sh` usage text

**Docs & counts** (search the repo for the previous count and bump consistently — there are two conventions: marketing "N languages" = code+Markdown, and "N code languages"):

11. README, `docs/{product,structure,tech,usage}.md`, this file (AGENTS.md / CLAUDE.md), `skills/cartog/SKILL.md` + `skills/cartog/references/supported_languages.md`, and the site (`site/src/pages/index.astro` + `usage.astro`): add a `lang-tag` whose icon is an `<img>` pointing at a new `site/public/assets/{lang}.svg` brand mark (24×24, white/brand fill legible on dark), matching the existing language chips. Edit the `.astro` source only; the Pages workflow rebuilds `site/dist`. See the site-sync note under **Documentation Convention**.

## CI/CD

- **CI** (`.github/workflows/ci.yml`): runs on push/PR to `main` — check, fmt, clippy, test, coverage (cargo-llvm-cov → Codecov). CI runs **raw `cargo`, not `make check`**, and gates the **default and `--no-default-features` builds separately** for both clippy (`cargo clippy --all-targets [--no-default-features] -- -D warnings`) and test (`cargo test [--no-default-features]`). `make check-rust` only runs the default-feature pass, so before pushing also run `cargo clippy --all-targets --no-default-features -- -D warnings` and `cargo test --no-default-features`, or CI fails on a gate you didn't see locally.
- **Supply-chain gates** (CI, on every PR): `cargo audit` (RUSTSEC advisories), `cargo deny check` (license/ban/source policy in `deny.toml`), and a `cargo cyclonedx` SBOM. Adding or bumping a dependency means running `cargo audit` and `cargo deny check` locally first — CI is red without them, and a new CVE must be surfaced before the dep lands, not after. A newly-published advisory against an existing transitive dep can turn CI red on an unrelated PR (and on `main`); when no upstream fix exists, ignore the specific RUSTSEC ID in **both** `deny.toml [advisories].ignore` **and** `.cargo/audit.toml` (the two ignore lists must stay in sync) with a dated rationale + a removal condition — never blanket-disable the gate. Current ignores: `RUSTSEC-2026-0194`/`-0195` (quick-xml DoS, transitive via self_update/rust-s3, no ≥0.41 upgrade path yet).
- **Perf gate** (CI, on PRs): the `queries` and `rag_search` criterion benches run with `--baseline base` against the PR's merge base; a regression there fails CI. The `cartog-indexer/indexing` bench is report-only (artifact, not gated). So a hot-path change to symbol queries or hybrid search must hold its bench — re-run `cargo bench -p cartog --bench queries`/`--bench rag_search` locally to check before pushing.
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

All documentation lives in `docs/`. Organized by the [Diataxis](https://diataxis.fr) framework — do not add a new file when an existing one fits. `docs/README.md` is the index; keep it in sync when adding or moving a doc.

**Tutorials** (learning-oriented, follow-along):

| File | Scope |
|------|-------|
| `docs/tutorials/quickstart.md` | Install → init → index → first search |

**How-to guides** (task-oriented, assumes prior knowledge):

| File | Scope |
|------|-------|
| `docs/how-to/configure-lsp-servers.md` | LSP server overrides and concurrency cap |
| `docs/how-to/switch-embedding-provider.md` | Local ONNX, Ollama, OpenAI-compatible |
| `docs/how-to/set-up-s3-sync.md` | Remote push/pull via S3-compatible storage |
| `docs/how-to/update-cartog.md` | Upgrade, rollback, deferred in-session updates |
| `docs/how-to/wire-editors.md` | Index page linking `mcp-setup.md` + `editor-integration.md` |
| `docs/mcp-setup.md` | Per-client MCP config snippets (Cursor, VS Code, Claude Desktop, …) |
| `docs/editor-integration.md` | Non-MCP editor CLI recipes (Neovim, Emacs, …) |

**Reference** (information-oriented, complete and accurate):

| File | Scope |
|------|-------|
| `docs/reference/cli.md` | All 27 CLI commands and global flags |
| `docs/reference/config.md` | All `.cartog.toml` keys and environment variables |
| `docs/reference/mcp-tools.md` | 16 MCP tools, progress notifications, cancellation, logging |
| `docs/reference/exit-codes.md` | `cartog self` exit codes, state file format, env vars |
| `docs/updates.md` | Full `cartog self update` reference (superseded by `how-to/update-cartog.md` for procedures) |
| `docs/structure.md` | Directory layout, module responsibilities, conventions |

**Explanation** (understanding-oriented, why and how):

| File | Scope |
|------|-------|
| `docs/explanation/architecture.md` | Design decisions table, secret redaction, SQLite tuning, MSRV |
| `docs/explanation/rag-pipeline.md` | Hybrid search pipeline: FTS5 + vector + RRF + reranker |
| `docs/explanation/incremental-indexing.md` | Three-layer change detection, Merkle invariants |
| `docs/explanation/concurrency.md` | rayon / tokio / std::thread concurrency models |
| `docs/tech.md` | Core dependencies, build profiles, test/benchmark matrix |

**Navigation and overview**:

| File | Scope |
|------|-------|
| `docs/product.md` | Product context, target users, differentiation |
| `docs/usage.md` | Hub: semantic search commands, plugin, agent skill, MCP server, agents |
| `docs/troubleshooting.md` | Common errors and fixes (the single home for these) |
| `docs/architecture/*.md` | Redirect stubs — canonical content is in `docs/explanation/` |

The MCP config JSON has one canonical copy in `docs/mcp-setup.md`; other docs link to it rather than re-embedding it. Release runbooks (e.g. `scripts/release-smoke.md`) live with the release scripts, not in `docs/`.

New subsystem explainers (cross-cutting, spanning features) go in `docs/explanation/` — not `docs/architecture/` (redirect stubs only) and not `docs/tech.md`. A new `.cartog.toml` config section belongs in both `docs/reference/config.md` and the `init.rs` template comment.

**The marketing site mirrors the docs and ships the same facts to users.** When you add or change a feature, command, MCP tool, language, client, config key, or count, update the site alongside the docs — it is not optional:

- `site/src/pages/index.astro` — landing page (feature cards, language/agent grids, counts, comparison tables, slogan).
- `site/src/pages/usage.astro` — the docs page that mirrors `docs/reference/cli.md` + `docs/reference/config.md` + `docs/reference/mcp-tools.md` (**every config section must have BOTH a row in the `[section]` summary table AND an explanation + example block**, e.g. embedding, remote S3, secret redaction, `[index]`). A new `.cartog.toml` key also goes in the `init.rs` template comment.

Edit the `.astro` source (never `site/dist/`, which is gitignored and rebuilt by the Pages workflow). New brand marks go in `site/public/assets/*.svg` as `<img>`-referenced files (24×24 viewBox, white/brand fill legible on the dark theme), not inline SVG. Run `npm run build` in `site/` to verify before committing.

### Per-crate docs.rs documentation

Every published crate (`cartog` + the 9 `cartog-*`) renders its `README.md` as the docs.rs landing page. The wiring, per crate:

- **`src/lib.rs`**: keep the curated `//!` header, then append the README via `#![doc = ""]` + `#![doc = include_str!("../README.md")]`. Feature-bearing crates (`cartog`, `-indexer`, `-mcp`, `-rag`) also carry `#![cfg_attr(docsrs, feature(doc_cfg))]` as the first attr, and gate visible feature badges with `#[cfg_attr(docsrs, doc(cfg(feature = "...")))]` on the relevant public item.
- **`Cargo.toml`**: `[package.metadata.docs.rs]` with `all-features = true` (renders feature-gated modules) + `rustdoc-args = ["--cfg", "docsrs"]` (activates the badges). `docsrs` is set only by docs.rs (nightly); on stable CI the `cfg_attr` is inert, so this is stable-safe and has zero effect on `cargo build`/`publish`.

Consequences to respect on every change:

- **READMEs are now published API docs** — keep them code-accurate (correct fn/type names, signatures, counts, public exports). Stale README = stale docs.rs.
- **Code fences become doctests.** A bare ` ``` ` fence or a ` ```rust ` fence in a wired README is compiled by `cargo test --doc`. Tag non-Rust examples ` ```text `/` ```toml `/` ```scheme `; tag illustrative-but-non-compiling Rust ` ```rust,ignore `; use ` ```rust,no_run ` for real, type-checked snippets that shouldn't execute. Run `cargo test --doc --workspace` (default **and** `--no-default-features`) after touching any README.
- **Intra-doc links to private items** (`[`name`]` where `name` is private/out-of-scope) warn under `cargo doc`. Drop the brackets to plain `` `code` `` when the target is internal. `cargo doc --no-deps --workspace` must stay warning-clean.

## Current State

- **Languages**: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, C, C++, C#, PHP, Dart, Swift, Kotlin, Vue, Svelte, Astro, Markdown
- **Frameworks**: React, Vue, Svelte, Astro. SFCs (`.vue/.svelte/.astro`) slice the `<script>`/frontmatter block, delegate to the JS/TS extractor, and remap byte/line offsets back to the full file (`crates/cartog-languages/src/sfc.rs`). JSX component usage (`<Counter/>`) emits a `Calls` edge in `.jsx/.tsx` and inside SFC scripts (`is_jsx_component` filter in `js_shared.rs` — uppercase/dotted tag = component, lowercase = intrinsic, skipped)
- **CLI**: 27 top-level commands (`init`, `ide`, `install`, `index`, `search`, `outline`, `refs`, `callees`, `impact`, `trace`, `context`, `hierarchy`, `deps`, `stats`, `savings`, `map`, `changes`, `config`, `doctor`, `watch`, `serve`, `push`, `pull`, `completions`, `manpage`, plus `rag` with 3 subcommands and `self` with 4 subcommands; `self update` has `--check`/`--defer`[`--to <version>`]/`--apply-pending` modes) + MCP server (16 tools)
- **Indexing**: incremental (git-based + SHA-256 + Merkle-tree symbol diffing), `--force` re-index. Stable symbol IDs (`file:kind:qualified_name`) survive line movements. Scoped edge resolution for changed files only. Live progress: climbing per-phase counters (`parsing M/N`, `storing M/N`, `resolving M/N edges with LSP`) on the CLI spinner and via MCP `notifications/progress`. LSP `definition` requests are pipelined in windows of 64 (~33% faster index on large repos). On the indexer's owned-manager path, per-language LSP servers resolve **concurrently** (one server process per language, capped by `[lsp] max_concurrent_servers` / `CARTOG_LSP_MAX_SERVERS`, default `min(langs, 4)`); DB-free worker threads drain into `resolve::LangOutcomes` and a single applier writes them in sorted-language order (byte-identical to serial). The warm MCP pass (`shared_manager=Some`) and `cap<=1` stay serial. `cartog index` is Ctrl-C cancellable (cooperative probe through the LSP phase; the whole pass rolls back, so a re-run redoes it). `cartog rag index` is likewise Ctrl-C cancellable (flushed embedding batches persist, so a plain re-run resumes); MCP `cartog_index`/`cartog_rag_index` honor `notifications/cancelled`. When a live `cartog serve` holds the same DB's lock, `cartog index` defers its LSP pass to that peer's warm servers (serve slot only — never watch-only peers; never on a first index; `--force` runs locally; stderr `note:` + `lsp_deferred_to_peer` in `--json`). The MCP `cartog_index` reopens state-4 (heuristic-exhausted) seals before its warm pass — re-sealing if no server starts — and catch-up-resolves a sealed backlog even on no-op (0 dirty files) calls, latching off per session when no LSP server is available
- **Search**: symbol search (`cartog search`), hybrid FTS5+vector RAG search with RRF merge and cross-encoder re-ranking
- **Watch**: `cartog watch` CLI + `cartog serve --watch` background mode, debounced re-index + deferred RAG embedding. Auto-embed is **on when the repo already has embeddings** (`db.embedding_count() > 0`); `--rag` / `[embedding] auto_embed` / `CARTOG_WATCH_RAG` override (resolved inside the watcher so failover re-checks live data). Editing a symbol's body invalidates its old embedding on re-index (`clear_embeddings_for_symbols_in_tx` for `diff.modified`), so it re-embeds. `cartog ide`/`install` now wires `serve --watch` for **all** clients (single-writer election makes concurrent watchers safe)
- **MCP single-writer**: `cartog serve` instances on the same DB use atomic O_EXCL election. First is primary, subsequent attach read-only (14 of 16 tools — only the 2 DB-write tools are gated); promoter on the secondary takes over within ~10s if the primary dies. Kill switch: `CARTOG_SINGLE_WRITER=0`.
- **Deferred self-update**: inside a Claude Code session the MCP server holds the serve lock, so `cartog self update` would refuse (exit 6). `cartog self update --defer` (or the `cartog_update` MCP tool) arms a pending update without swapping; the SessionEnd hook runs `--apply-pending` once the peer exits. See [docs/updates.md](docs/updates.md).
- **CI/CD**: fmt, clippy, test, coverage, release to crates.io + GitHub Releases
- **Centrality**: in-degree ranking — search results prefer highly-referenced symbols
- **Codebase map**: `cartog map --tokens N` produces budget-aware file tree + top symbols
- **Token budget**: `--tokens N` global flag for context-window-aware output truncation (human output only). `--compact` global flag strips heavy fields (bodies, docstrings, cache hashes) from `--json` to save agent tokens (keeps ids/names/kinds/locations/signatures/scores; no-op without `--json`). MCP is **compact by default** (symbol noise trimmed; `cartog_rag_search`/`cartog_trace` bodies bounded to a snippet; `cartog_context` keeps budgeted bodies) — set `CARTOG_MCP_COMPACT=0` to restore full bodies
- **Recent changes**: `cartog changes` shows symbols affected by recent git commits
- **Call-path trace**: `cartog trace <from> <to>` / `cartog_trace` returns the shortest `calls` path between two symbols with each hop's body inline (forward BFS, static call edges only)
- **Task-context bundle**: `cartog context <task>` / `cartog_context` fuses hybrid search seeds + 1-hop neighbors + seed-file centrality into a token-budgeted bundle
- **Staleness banners**: when `cartog serve --watch` has pending changes/embeddings (including symbols whose body was edited and not yet re-embedded — now counted since the drift fix invalidates their embeddings), affected MCP read-tool responses are prefixed with a `⚠️` banner; gated on a live watcher (no banner for read-only peers or watcherless serve)
- **AST-aware embeddings**: significant body lines (skip blanks/comments/braces) for better vector search recall
- **Embedding format versioning**: auto-detects embedding strategy changes, triggers re-embed on next `rag index`
- **Schema versioning**: metadata-based migration system for DB schema evolution
- **Pluggable embedding providers**: local ONNX (default), Ollama, and a generic OpenAI-compatible `/v1/embeddings` provider (OpenAI, Mistral, Voyage, Jina, OVHcloud, or local `/v1` servers — switch vendors via `base_url`; API key from an env var named by `[embedding.openai] api_key_env`, never in TOML; Azure's deployment-path shape is out of scope), configured via `.cartog.toml`
- **Secret redaction**: default-on, best-effort. Scrubs common secret patterns (AWS/GitHub/Slack/Stripe/JWT + quoted key=value assignments) from `symbol_content`, `signature`, `docstring`, and embeddings; always excludes sensitive files (`.env`, `*.pem`, `id_rsa`, ...). Toggling `[security] redact_secrets` force-reindexes. See [docs/tech.md](docs/tech.md#secret-redaction)
- **Feature flags**: binary `cartog` default = `lsp` + `remote-s3` + `ollama-embedding` + `openai-embedding` (all on); advanced users strip via `--no-default-features`. Runtime embedding default stays local ONNX (`provider = "local"`); Ollama and OpenAI are opt-in via `.cartog.toml`. Crate `cartog-rag` — `provider-local` (default), `provider-ollama`, `provider-openai`
- **Walk filtering** (`.gitignore` + floor + exclude): the indexer walks via the `ignore` crate (`WalkBuilder`, single-threaded `.build()`), honoring `.gitignore` (incl. nested; `require_git(false)` applies it without a `.git` dir; `parents(false)` + `git_global(false)` keep it to ignore files inside the indexed tree, never an ancestor or `$HOME/.gitignore`) and `.cartogignore`. A hardcoded floor (`is_ignored_dirname`: node_modules/target/vendor/… + dotdirs, `var`/`builds` at depth 1) and `[index] exclude` globs run as a `filter_entry` on top, staying authoritative (prune even where `.gitignore` keeps a path; apply in non-git trees). `[index] respect_gitignore = false` disables only the git layer. The 3 layers + the toggle are bundled in `cartog_indexer::WalkFilter { exclude: ExcludeGlobs, respect_gitignore, jobs }` (built by `config::to_walk_filter`), threaded into `index_directory` (9th arg), `WatchConfig`, and `CartogServer`. `jobs` caps the parse-phase rayon pool (`--jobs N` flag > `CARTOG_JOBS` > `[index] jobs` > auto; `clamp_jobs` resolves 0→`available_parallelism`, clamps 1..=64). The parse phase runs inside a dedicated pool sized to `jobs` (`parse_pool`, cached per size in a `OnceLock<Mutex<HashMap>>` so worker threads + their `THREAD_EXTRACTORS` cache survive re-indexes), so the cap applies on **every** index, including under serve/watch — unlike the global pool, it's unaffected by another subsystem (e.g. tokenizers) initializing rayon first. `is_ignored`/`is_excluded_path` take primitives (name/is_dir/depth, rel/is_dir) so they're walker-agnostic. `[index] exclude` globs use `globset` with `literal_separator` (gitignore-like `*`); a matched dir is pruned via a synthetic-child probe (`dir/**` doesn't match the bare dir). Watcher event filter (`is_relevant_path`) applies the floor + `exclude` but delegates `.gitignore` to the indexer's re-index pass (PR2 boundary). See [docs/usage.md](docs/usage.md)
- **LSP command override**: `[lsp.<lang>] command = [...]` runs a custom (e.g. Dockerized) LSP server instead of the PATH-resolved `ServerSpec`. `${ROOT}` in any argv element expands to the host-absolute project root; path mirroring (`-v ${ROOT}:${ROOT} -w ${ROOT}`) is mandatory because cartog exchanges host-path `file://` URIs. `LspManager::with_overrides`; threaded via `config::to_lsp_overrides` → `index_directory`/`run_server` (watch passes none — it never runs LSP). See [docs/usage.md](docs/usage.md). All 16 LSP languages have a pinned `benchmarks/lsp-images/<lang>.Dockerfile`; `resolution_rate.sh --docker-lsp` runs each via its `cartog-lsp-<lang>:stable` image (strict, no host fallback) and all resolve identically to host. A command-override server gets `processId: null` (its host PID is absent from a container's PID namespace; without this, pyright/typescript-language-server honor the LSP parent-liveness check and exit at startup).
- **Frontend SFCs + JSX** (`crates/cartog-languages/src/sfc.rs`): Vue/Svelte/Astro extractors parse the envelope grammar (`tree-sitter-vue-updated`/`-svelte-ng`/`-astro-next`), locate each `<script>`/frontmatter region (Vue/Astro may have several), slice its content, delegate to the JS or TS extractor (`lang="ts"` → TS, else JS), then remap byte/line offsets back so the indexer's full-file Merkle re-slice stays exact. JSX component usage emits `Calls` edges via a `jsx_query` in `js_shared.rs` (covers `.jsx/.tsx` and SFC scripts). LSP edge resolution uses dedicated SFC servers (`vue-language-server`/`svelteserver`/`astro-ls`) — the resolver groups edges by `detect_language` and queries the real `.vue/.svelte/.astro` file at the remapped full-file positions, so SFC servers resolve them natively. Each SFC file also emits a whole-file `component` symbol (stem-named, PascalCase-normalized) so component imports and JSX usages resolve to it; component symbols carry no stored content/embedding
- **Index-creation consent gate**: cartog never creates a `.cartog/` for a config-less, un-indexed project on its own. Consent = a present `.cartog.toml` — **`Loaded` or `Rejected`**: the file existing is the opt-in, its contents parsing is a separate question (deriving consent from parse success made one typo report `no .cartog.toml in this project` at a user looking straight at one) — **or** an existing main DB file at the resolved path (Branch 1; stray `-wal`/`-shm` alone doesn't count) **or** `CARTOG_AUTO_INIT` non-empty (indexes with defaults, writes **no** config). The pure predicate `config::allow_index_creation(db_path, consent: IndexConsent)` (an enum, not a bool, so neither state can be passed inverted) is evaluated once in `main.rs` and threaded as `allow_create: bool` into `run_server` → `WatchConfig`/`CartogServer` (mirrors `walk_filter`/`redact`; lower crates never import a config helper). `cartog index`/`rag index`/`watch` refuse before dispatch (`is_gated_write_command`, excludes `Serve`); read commands fall back to `Database::open_memory()` via `commands::shared::open_db` (write commands use `open_db_create`). A **rejected** config grants consent but still refuses to create a *fresh* index: its unreadable `[database] path` might point elsewhere, so materializing the default location would land `.cartog/` somewhere the user configured away from — `--db`/`CARTOG_DB` or an existing DB settles it and indexing proceeds. `cartog-db` gained `Database::open_existing` (no `create_dir_all`; `DbError::NotFound` when absent). `serve` starts **degraded** when consent is absent + no DB: empty in-memory DB, no `.cartog/`, `refuse_if_degraded` gates the 2 write tools, `cartog_stats` carries `degraded` (serialized only when true) + a banner. The `serve --watch` watcher stays degraded (`watcher_consents` re-checks live each pass) and **pre-builds** the index when `.cartog.toml` appears / a DB shows up / `CARTOG_AUTO_INIT` is set (`wait_for_consent`, root-watched + 1s poll); the running server stays degraded until relaunch (no live swap — Seam 2). `.cartog/` is excluded from the watch set by the existing dotdir floor. The SessionStart hook (`ensure_indexed.sh`) mirrors the rule: gate = `_has_toml OR db-file-exists OR CARTOG_AUTO_INIT`. See [docs/reference/config.md](docs/reference/config.md#index-creation-consent-gate)
