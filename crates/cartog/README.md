# cartog

Code graph indexer for LLM coding agents. Map your codebase, navigate by graph.

## Overview

Binary crate and library facade. Provides the `cartog` CLI with 25 top-level commands for code graph indexing, querying, and semantic search. Also re-exports workspace crates under the `cartog::` namespace for use by benches and integration tests.

## CLI commands

| Command | Description |
|---------|-------------|
| `init` | Scaffold a `.cartog.toml` config |
| `ide` | Wire `cartog serve` into MCP-compatible editors |
| `index [PATH]` | Build or rebuild the code graph (`--force`, `--no-lsp`) |
| `outline FILE` | Show symbols and structure of a file |
| `callees NAME` | Find what a symbol calls |
| `impact NAME` | Transitive impact analysis (`--depth`, default: 3) |
| `refs NAME` | All references to a symbol (`--kind` filter) |
| `hierarchy NAME` | Inheritance hierarchy for a class |
| `deps FILE` | File-level import dependencies |
| `stats` | Index statistics summary |
| `search QUERY` | Search symbols by name (`--kind`, `--file`, `--limit`) |
| `map` | Token-budget-aware codebase summary (`--tokens`, default: 4000) |
| `changes` | Symbols affected by recent git changes (`--commits`, `--kind`) |
| `watch [PATH]` | Auto-re-index on file changes (`--rag`, `--debounce`) |
| `serve` | Start MCP server over stdio (`--watch`, `--rag`) |
| `push` / `pull` | Sync the index to/from an S3-compatible remote (opt-in) |
| `config` | Print the active resolved configuration |
| `doctor` | Diagnose setup (git, config, DB, models) |
| `completions` / `manpage` | Generate shell completions / man page |
| `rag setup` | Download embedding and reranker models |
| `rag index` | Build embedding index for semantic search |
| `rag search` | Semantic search over code symbols |
| `self update/version/rollback/migrate-db` | Manage the installed binary and migrate a legacy DB |

## How it works

### Config resolution

Database path is resolved with a 4-tier priority:

1. `--db` CLI flag / `CARTOG_DB` environment variable
2. `[database.path]` in `.cartog.toml` (found by walking up to git root)
3. Auto git-root detection — `.cartog/db.sqlite` next to `.git/` (legacy `.cartog.db` is still read with a one-shot warning if only it exists; `cartog self migrate-db` moves it)
4. Fallback — `.cartog/db.sqlite` in the current directory

`.cartog.toml` also configures RAG providers via `[embedding]` (provider, model, dimension) and `[reranker]` (provider) sections. See `crates/cartog-rag/README.md` for details.

### Logging

All log output goes to **stderr** (stdout is reserved for CLI output and MCP protocol). Default level is `warn` for CLI commands, `info` for `serve`, `watch`, and `rag` operations. Override with `RUST_LOG`.

### Library facade

`lib.rs` re-exports all workspace crates for backward-compatible access:

```rust
pub use cartog_db as db;
pub use cartog_indexer as indexer;
pub use cartog_languages as languages;
pub use cartog_rag as rag;
pub use cartog_core as types;
pub use cartog_watch as watch;
pub use cartog_lsp as lsp; // gated on the `lsp` feature (on by default)
pub use cartog_process_lock as process_lock; // hidden from rustdoc
```

This allows benches and integration tests to use `cartog::db::Database`, `cartog::indexer::index_directory`, etc.

## Crate dependencies

All workspace crates: `cartog-core`, `cartog-db`, `cartog-indexer`, `cartog-languages`, `cartog-rag`, `cartog-watch`, `cartog-mcp`, optionally `cartog-lsp`
