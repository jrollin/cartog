# Contributing to cartog

## Before you start

- Search [existing issues](https://github.com/jrollin/cartog/issues) and PRs to avoid duplicate work.
- For significant changes, open an issue first to align on the approach before writing code.

## Setup

Requires Rust MSRV 1.80.

```bash
cargo build                         # default build (includes LSP)
cargo build --no-default-features   # minimal build without LSP
```

## Quality checks

All checks must pass before submitting a PR:

```bash
make check        # cargo fmt + clippy + test + fixture validation + skill tests
```

Individual targets:

```bash
make check-rust            # cargo fmt --check + clippy -D warnings + cargo test
make check-fixtures        # validate all language fixture codebases (py, ts, go, rs, rb, java, php, dart, swift)
make check-fixtures-docker # same, forcing the Docker fallback for every language
make check-skill           # bash unit tests for the agent skill
```

Each `check-fixtures` language target validates with the native toolchain when
present, else a pinned official Docker image (`check-fixtures-docker` forces this
path). No toolchain and no Docker is a hard fail. cartog's own `cargo test` needs
no language toolchains.

## Commit style

This project uses [conventional commits](https://www.conventionalcommits.org/). git-cliff generates the changelog from commit messages automatically — no manual CHANGELOG edits needed.

Format: `type(scope): description`

| Type | When to use |
|------|-------------|
| `feat` | New user-visible feature |
| `fix` | Bug fix |
| `perf` | Performance improvement |
| `refactor` | Internal restructuring |
| `test` | Tests only |
| `docs` | Documentation only |
| `chore` | Tooling, deps, release |

Common scopes: `lang`, `db`, `mcp`, `search`, `index`, `watch`, `skill`, `ci`

Examples:
```
feat(lang): add C++ support
fix(db): resolve connection leak on reindex
perf(search): reduce query latency with covering index
docs(usage): add MCP Zed configuration example
```

## Adding a new language

End-to-end checklist for landing a new language. PR #35 (PHP) and PR #89 (Swift)
are good references; the per-language wiring is shallow and the bulk of the work
is the benchmark fixture.

> **Pick the grammar carefully.** The `tree-sitter-<lang>` crate must export a
> `LANGUAGE: LanguageFn` constant (i.e. depend on `tree-sitter-language`), not a
> legacy `language() -> Language` function — only the former links against the
> `tree-sitter` core version cartog pins. Confirm with a one-line smoke test
> (`parser.set_language(&Language::new(tree_sitter_<lang>::LANGUAGE))` then parse
> a snippet) before writing the extractor. Dumping `node.to_sexp()` for the
> constructs you care about is the fastest way to learn the grammar's node kinds.

### 1. Extractor

1. Add the tree-sitter grammar to `[workspace.dependencies]` in the root `Cargo.toml`
   and to `crates/cartog-languages/Cargo.toml`.
2. Create `crates/cartog-languages/src/<lang>.rs` implementing the `Extractor` trait:
   ```rust
   pub struct MyLangExtractor { parser: tree_sitter::Parser }
   impl Extractor for MyLangExtractor {
       fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> { ... }
   }
   ```
   Keep all per-language helpers private to this module — don't add anything to
   `lib.rs` beyond the registration below. Never panic on malformed input: degrade
   and return partial results (the indexer feeds arbitrary source). Guard recursive
   walkers against pathologically deep input (see `swift.rs`'s depth check).
3. Register the module and file extension:
   - Module declaration + `get_extractor()` arm + a `test_get_extractor` assert +
     the module-doc language list in `crates/cartog-languages/src/lib.rs`
   - Extension mapping + a `detect_language()` test in `crates/cartog-core/src/lib.rs`
4. Add unit tests in the new module (mirror `dart.rs` / `swift.rs` tests — one
   behaviour-named test per construct: symbols, edges, visibility, async, docstrings,
   plus empty-file and syntax-error cases).

### 2. Benchmark fixture

5. Create `benchmarks/fixtures/webapp_<lang>/` mirroring the shape of an existing
   fixture (e.g. `webapp_php` or `webapp_swift`). Symbol names must match cross-language
   so the 13 shared bench scenarios apply unchanged — in particular `validateToken`/
   `validate_token`, `AuthService`, `BaseService`, `handleLogin`→`authenticate`→`login`→
   `generateToken`→`executeQuery`→`getConnection` (deep chain), `getLogger`, `TokenError`,
   `DatabaseConnection`. Exercise the constructs your extractor handles (don't author the
   fixture around its blind spots) so a regression actually shows up.
6. Add `<tag>` to `FIXTURE_LANGS` in `crates/cartog-indexer/src/lib.rs` (`bench_support`)
   so the criterion per-language indexing bench picks it up.
7. Add `benchmarks/ground_truth/webapp_<lang>.json`: index the fixture locally, **derive**
   the 13 scenario entries from real `cartog --json` query output, then **hand-verify each**
   — don't invent counts, and don't let a buggy extractor's output become the expected value.
8. Wire the fixture into the bench harness:
   - Add the `<tag>` to the `--fixture` filter in `benchmarks/lib/common.sh` (the single source of truth both `token_savings.sh` and `resolution_rate.sh` source)
   - Add `run_scenario "webapp_<lang>" ...` lines in every script under `benchmarks/scenarios/`
9. Validate: `make check-fixtures`. If your language ships a compiler/syntax checker, add a
   `check-<lang>` target to the `Makefile` via the `check_lang` function (native tool,
   pinned Docker fallback, hard fail), add it to both `.PHONY` and the `check-fixtures`
   prerequisites, and gitignore any build dir the checker leaves in the fixture.
10. Run `make bench` (or `./benchmarks/token_savings.sh --fixture <tag>`) and confirm the
    fixture appears with non-zero recall on every scenario.

### 3. LSP wiring (optional but recommended)

11. Pick a language server with a stdio LSP entry point. Add one (or two, for primary +
    fallback) `ServerSpec` entries to `SERVERS` in `crates/cartog-lsp/src/servers.rs`
    (set `binary`, `args`, `language_id`, and an `install_hint`). Order matters: the
    first available binary wins.
12. Add a `test_find_servers_<lang>` assertion in the same file's test module to pin
    the priority order.
12b. Add a pinned `benchmarks/lsp-images/<lang>.Dockerfile` (image
    `cartog-lsp-<lang>:stable`) so `resolution_rate.sh --docker-lsp` and
    `make lsp-images` cover the language host-independently. The `ENTRYPOINT`
    must reproduce the `ServerSpec` `binary` + `args` (cartog spawns the
    override argv verbatim and only borrows the spec's `language_id`); the
    generated `docker run` uses `-i` (never `-t`) and mirrors the host path with
    `-v ${ROOT}:${ROOT} -w ${ROOT}`. Copy an existing recipe's header. Confirm
    the containerized server resolves the same `lsp`-tier edges as host (cartog
    sends `processId: null` to override servers so a container's PID namespace
    doesn't trip the LSP parent-liveness check). See `benchmarks/README.md` →
    "Reproducing the numbers".
13. Bench-validate the LSP integration: install the server, run `cartog index` with and
    without `--no-lsp` on your fixture, and capture `edges_resolved` vs
    `edges_lsp_resolved`, plus `edges_marked_unresolvable` (true negatives:
    typo, dyn dispatch, macro) and `edges_marked_external` (LSP located the
    target outside the indexed root: stdlib, deps, node_modules) from `--json`
    output. If the LSP gain is meaningful, add a row to the recall table in
    `README.md` and `site/src/pages/usage.astro`.

### 4. Documentation

Every doc that enumerates supported languages or LSP servers must mention the new one.
Note the two counting conventions and bump both consistently: the **marketing count**
("N languages") counts code languages **plus** Markdown, while the **code-language count**
excludes Markdown — a doc may use either, so check what each sentence means before editing.

Surfaces to update (all confirmed by the Swift PR):

- `README.md` — headline count, comparison table, the supported-languages table, and the
  LSP-server auto-detect list
- `docs/product.md`, `docs/tech.md`, `docs/structure.md` — language counts, grammar list,
  LSP-server list, and the "N language webapps" bench count
- `docs/troubleshooting.md` — the LSP-server enumeration (+ an SDK/install note if the
  server doesn't ship on a common PATH)
- `skills/cartog/SKILL.md` (description + limitations + supported-servers list) and
  `skills/cartog/references/supported_languages.md` (count header + a per-language section)
- `crates/cartog-languages/README.md` — supported-languages line + the per-module API table
- `crates/cartog-mcp/src/lib.rs` — the MCP server "Languages:" instruction string
- `AGENTS.md` (= `CLAUDE.md`) — the architecture comment, language list, and bench count
- `site/src/pages/index.astro` and `site/src/pages/usage.astro` — counts + a language card
  (edit the `.astro` source; the Pages workflow rebuilds `site/dist`, which is gitignored)

Grep before committing — every match must mention the new language/server:

```bash
grep -rn 'Java, PHP, Dart\|jdtls, intelephense' \
  README.md docs/ site/src/ skills/ crates/*/README.md crates/cartog-mcp/ AGENTS.md
```

Every match should mention the new language/server.

## Feature flags

| Flag | Default | Description |
|------|---------|-------------|
| `lsp` | on | LSP-based edge resolution. Disable at build time with `--no-default-features` (runtime equivalent: `--no-lsp`) |
| `ollama-embedding` | off | Enables Ollama as an embedding provider for RAG |

## Regenerating the demo gif

The animated demo on the README and the landing page is produced by
[VHS](https://github.com/charmbracelet/vhs) from `docs/demo.tape`.

```bash
brew install vhs            # or: go install github.com/charmbracelet/vhs@latest
cargo build --release       # so `cartog` resolves to the current build
PATH="$PWD/target/release:$PATH" vhs docs/demo.tape
cp docs/demo.gif site/public/demo.gif   # keep the site mirror in sync
```

Bump the `?v=N` query string on `<img src="demo.gif?v=N">` in
`site/src/pages/index.astro` whenever the gif changes so browsers pick up the
new asset.

## Building the site

The landing page lives in `site/` as an [Astro](https://astro.build) project.
GitHub Pages builds it on tag push via `.github/workflows/pages.yml`.

```bash
cd site
npm ci            # install pinned deps (package-lock.json)
npm run build     # static output → site/dist/
npm run dev       # local preview at http://localhost:4321
```

Page sources are `site/src/pages/{index,usage}.astro`; shared nav/footer/head
live in `site/src/components/` and `site/src/layouts/Base.astro`. Static files
served verbatim (`style.css`, `demo.gif`, `install.sh`, `assets/`) are in
`site/public/`.

## Code of Conduct

This project follows the [Contributor Covenant 2.1](CODE_OF_CONDUCT.md).
Please read it before opening an issue or pull request.

## Single maintainer

This is a solo-maintained project. PRs and issues are reviewed on a best-effort basis.

## References

- Architecture and tech decisions: [docs/tech.md](docs/tech.md)
- Product context and goals: [docs/product.md](docs/product.md)
- CLI, MCP, and skill setup: [docs/usage.md](docs/usage.md)
- Code of Conduct: [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)
