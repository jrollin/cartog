# Contributing to cartog

## Before you start

- Search [existing issues](https://github.com/jrollin/cartog/issues) and PRs to avoid duplicate work.
- For significant changes, open an issue first to align on the approach before writing code.

## Setup

Requires Rust MSRV 1.77.

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
make check-fixtures        # validate all language fixture codebases (py, ts, go, rs, rb, java, php)
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

End-to-end checklist for landing a new language. PR #35 (PHP) is a good reference;
the per-language wiring is shallow and the bulk of the work is the benchmark fixture.

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
3. Register the module and file extension:
   - Module + `get_extractor()` arm in `crates/cartog-languages/src/lib.rs`
   - Extension mapping in `crates/cartog-core/src/lib.rs` `detect_language()`
4. Add unit tests in the new module (mirror `python.rs` tests).

### 2. Benchmark fixture

5. Create `benchmarks/fixtures/webapp_<lang>/` mirroring the shape of an existing
   fixture (e.g. `webapp_rb` or `webapp_php`). Symbol names must match cross-language
   so the 13 shared bench scenarios apply unchanged.
6. Add `benchmarks/ground_truth/webapp_<lang>.json` — easiest path: index the fixture
   locally with cartog, then transcribe real query output into the 13 scenario entries.
7. Wire the fixture into the bench harness:
   - Add a case to the `--fixture` filter in `benchmarks/run.sh` and `benchmarks/lib/common.sh`
   - Add `run_scenario "webapp_<lang>" ...` lines in every script under `benchmarks/scenarios/`
8. Validate: `make check-fixtures`. If your language ships a syntax checker, add a
   `check-<lang>` target to the `Makefile` via the `check_lang` function (native tool,
   pinned Docker fallback, hard fail), and add it to the `check-fixtures` prerequisites.
9. Run `make bench` and confirm the new fixture appears in the summary with non-zero
   recall on every scenario.

### 3. LSP wiring (optional but recommended)

10. Pick a language server with a stdio LSP entry point. Add one (or two, for primary +
    fallback) `ServerSpec` entries to `SERVERS` in `crates/cartog-lsp/src/servers.rs`.
    Order matters: the first available binary wins.
11. Add a `test_find_servers_<lang>_*` assertion in the same file's test module to pin
    the priority order.
12. Bench-validate the LSP integration: install the server, run `cartog index` with and
    without `--no-lsp` on your fixture, and capture `edges_resolved` vs
    `edges_lsp_resolved` from `--json` output. If the LSP gain is meaningful, add a row
    to the recall table in `README.md` and `site/usage.html`.

### 4. Documentation

Every doc that enumerates supported languages or LSP servers must mention the new one.
At minimum:

- `README.md` — supported-languages section + LSP server list (~line 106)
- `docs/product.md`, `docs/tech.md`, `docs/structure.md` — language counts and lists
- `docs/troubleshooting.md` — LSP server enumeration
- `skills/cartog/SKILL.md` — supported languages + supported servers
- `crates/cartog-languages/README.md`
- `site/index.html` and `site/usage.html` — language cards and tables
- `crates/cartog-mcp/src/lib.rs` — tool description language list (if mentioned)

Grep before committing:

```bash
grep -rn 'Python, TypeScript\|pyright.*typescript-language-server' \
  README.md docs/ site/ skills/ crates/
```

Every match should mention the new language/server.

## Feature flags

| Flag | Default | Description |
|------|---------|-------------|
| `lsp` | on | LSP-based edge resolution. Disable at build time with `--no-default-features` (runtime equivalent: `--no-lsp`) |
| `ollama-embedding` | off | Enables Ollama as an embedding provider for RAG |

## Single maintainer

This is a solo-maintained project. PRs and issues are reviewed on a best-effort basis.

## References

- Architecture and tech decisions: [docs/tech.md](docs/tech.md)
- Product context and goals: [docs/product.md](docs/product.md)
- CLI, MCP, and skill setup: [docs/usage.md](docs/usage.md)
