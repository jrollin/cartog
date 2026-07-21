# cartog — Architecture Decisions

> RAG pipeline design: [rag-pipeline.md](rag-pipeline.md) | Incremental indexing: [incremental-indexing.md](incremental-indexing.md) | Concurrency: [concurrency.md](concurrency.md)

## Language

**Rust** — Performance, tree-sitter native bindings, single binary distribution.

## Architecture Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Parser | tree-sitter (CST + query API) | Incremental, multi-language, structural. JS/TS and Python use declarative tree-sitter queries for call/throw/type-ref extraction; other languages use cursor walks. Handles 90% of what LSP provides without running a language server |
| Storage | SQLite (single `.cartog/db.sqlite`) | Zero infra, ~1 MB, persists across sessions. WAL mode enables concurrent readers (watcher + MCP server). Legacy `.cartog.db` at the project root is still read for backwards-compatibility |
| Packaging | Skill (primary) | Changes agent workflow, not just adds a tool. Works with any LLM that has bash access |
| MCP server | `cartog serve` (stdio) | Skill remains primary; MCP as secondary for zero-context-cost tool access. 1:1 mapping with CLI commands — same `db.*()` code paths |
| Change detection | Layered: git diff → file SHA-256 → symbol Merkle (+ `--force`) | Each layer prunes a different unit (files, then symbols). Full design, layer diagram, hash invariants, and failure modes in [incremental-indexing.md](incremental-indexing.md) |
| Edge resolution | Name-based, scope-aware, multi-pass, scoped | 6-tier priority: same file > import-path > same dir > parent scope > unique global > kind disambiguation (type def > function > method). Two passes so import edges resolved in pass 1 feed pass 2. Incremental mode: invalidates dangling edges after symbol changes, then re-resolves only affected edges. Each resolved edge records the resolving tier/source in `edges.resolution_source` (`EdgeProvenance`), surfaced as `provenance` in `--json`/MCP output; LSP overwrites the heuristic tag, reset paths clear it |
| Symbol ID | `file_path:kind:qualified_name` | Stable across line movements. `kind` = function/class/method/etc. Qualified name encodes parent chain: `auth.py:method:TokenService.validate`. Deterministic, human-readable, no UUIDs |
| Ignore strategy | `.gitignore` + `.cartogignore` (via the `ignore` crate) + a hardcoded floor + `[index] exclude` | The `ignore` crate honors `.gitignore` (incl. nested, no `.git` required) and `.cartogignore`. A hardcoded floor (18 dirs + `starts_with('.')`: node_modules, target, venv, dist, build, vendor, …) and `[index] exclude` globs prune on top, so they win even where `.gitignore` would keep a path. `[index] respect_gitignore = false` disables only the git layer. |
| Content truncation | 2048 bytes per symbol | ~512 tokens at code's ~2-3 chars/token ratio. Captures signature + leading body. Below 50 bytes → excluded (noise) |
| Name normalization | camelCase/snake_case splitting for FTS5 | `validateToken` → `"validate token"`, `get_http_response` → `"get http response"`. Stored in FTS5 alongside original name |
| Serialization (embeddings) | Raw little-endian f32 bytes | Zero-overhead, directly compatible with sqlite-vec `MATCH` operator. 384 dims × 4 bytes = 1536 bytes per symbol |
| Language detection | File extension | Simple, predictable, covers 99%. TSX is a separate key from TS (different grammar for JSX nodes) |
| Async boundary | Manual `tokio::Runtime` for `serve` only | 95% of commands are sync. Avoids async overhead for index/search/refs. `spawn_blocking` offloads sync SQLite calls from the async MCP handler. Full model in [concurrency.md](concurrency.md) |
| DB concurrency | `Arc<Mutex<Database>>` | Single connection, not a pool. MCP serves one agent session — contention is negligible. `std::sync::Mutex` (not tokio) because lock is never held across `.await`. See [concurrency.md](concurrency.md) |
| Path security | Canonical CWD validation | MCP tool parameters come from LLM agents. Rejects paths outside CWD subtree via `canonicalize` + `starts_with`. Defense-in-depth against prompt injection |
| Secret redaction | Default-on, best-effort | Scrubs common secret patterns from stored symbol text and skips sensitive files. See [Secret redaction](#secret-redaction) below |
| Watch mode | Debounced re-index + deferred RAG | 5s debounce, 30s RAG delay. Auto-embeds when the repo already has embeddings (auto-detected; `--rag`/`[embedding] auto_embed`/`CARTOG_WATCH_RAG` override). Edited symbols' stale embeddings are invalidated on re-index so they re-embed. Embedding only fires after editing stops |
| Vector search | sqlite-vec (opt-in) | Embedded in SQLite, no external infra. Models downloaded via `cartog rag setup` |
| Model cache | `~/.cache/cartog/models` | XDG-compliant shared cache avoids downloading ~1.2 GB of models per project. Precedence: `FASTEMBED_CACHE_DIR` > `XDG_CACHE_HOME/cartog/models` > `~/.cache/cartog/models` > `./.fastembed_cache` (CWD last resort, only when neither HOME nor XDG resolves) |
| Output format | Human default + `--json` flag (global) | Readable for humans, parseable for scripts. Both `cartog --json stats` and `cartog stats --json` work |
| Distribution | `cargo install` + pre-built binaries | GitHub Releases for 4 targets (Linux x86/ARM, macOS ARM, Windows x86), crates.io publish, in-place upgrade via `cartog self update` |
| LSP | Auto-detected (default feature) | Index-time refinement for edges unresolved by heuristics. Auto-detects language servers on PATH (rust-analyzer, pyright, typescript-language-server, gopls, ruby-lsp, solargraph, jdtls, csharp-ls, intelephense, dart, sourcekit-lsp, kotlin-language-server, vue-language-server, svelteserver, astro-ls), sends `textDocument/definition`, shuts down after. Silently skips when no server found. Ready-timeout 20s (override via `CARTOG_LSP_READY_TIMEOUT_SECS`). A server that starts but never answers is abandoned after ~30s (three consecutive timed-out request windows), falling back to heuristics for that language. Edges LSP cannot map in-graph are persisted as `resolution_state=2` (truly unresolvable: typo, dyn dispatch, macro) or `=3` (external: stdlib, deps, node_modules); both are skipped on subsequent runs until a matching symbol is added. Disable at runtime with `--no-lsp`; opt out at build time with `cargo install cartog --no-default-features` |
| MCP response cap | 64 KB per tool result | Prevents oversized JSON from evicting agent context. Bounds text block + `structuredContent` together by trimming trailing result items to fit, with an omitted-count notice and narrowing hint per tool. Override via `CARTOG_MCP_MAX_BYTES` |
| RAG tuning | `[rag]` section in `.cartog.toml` | `retrieval_multiplier`, `retrieval_floor`, `rerank_max`, `rerank_min` control FTS5/vector candidate pool size and cross-encoder cost. See [../reference/config.md](../reference/config.md) |
| Workspace | Cargo workspace (10 published crates + `cartog-loom-models`, test-only) | Incremental compilation, explicit dependency boundaries, independent crate reuse. See [../structure.md](../structure.md) for layout and dependency graph |
| Monorepo | Deferred | Index from CWD, user can `cd` into subproject |
| Remote index sync | Opt-in S3-compatible push/pull (default-on feature, inert without config) | `remote-s3` feature ON by default — single distributable binary, no rebuild from source for teams. Inert until `[remote]` is set or `--remote` passed: no network traffic, no impact on air-gapped use. Credentials resolved from the AWS env chain only; `.cartog.toml` rejects credential-shaped keys at parse time. `rust-s3` (~5 MB) over `aws-sdk-s3` (~18 MB) for binary size. Push records git-commit provenance (`x-amz-meta-git-commit` from the index's `last_commit`); pull reports it and cross-checks header vs file, but never blocks on staleness — the read-mostly "CI builds, devs pull" flow. See [../how-to/set-up-s3-sync.md](../how-to/set-up-s3-sync.md) |

## Secret redaction

cartog stores symbol text on disk (the `symbols` table, the `symbol_content`
FTS5 source, and RAG embeddings derived from it). To keep hardcoded secrets out
of that index, redaction is **on by default** (`[security] redact_secrets`).

**Best-effort, not a guarantee.** Detection uses anchored, length-bounded
`regex` patterns for common vendor token shapes (AWS access key IDs, GitHub
PATs, Slack tokens, Stripe keys, JWTs) plus a quoted `key = value` assignment
scan keyed on `password`/`secret`/`token`/`api_key`. Matches are replaced with
`[REDACTED_SECRET]`. It favours precision (not mangling real code) over recall,
so some secrets slip through, notably bare high-entropy strings not behind a
recognised keyword and AWS secret keys outside an `aws_secret_access_key`
assignment. Treat it as mitigation, not a vault.

**Surfaces covered.** Redaction is applied to the extracted value strings only,
never the source buffer (byte offsets back symbol slicing and Merkle hashing):
`symbol_content.content` (feeds FTS5 + embeddings + search results + reranker)
and each symbol's `signature`/`docstring` (returned directly by `cartog search`
/ `cartog outline`). Hashes key off the raw source, so the redaction flag never
perturbs incremental change detection.

**Sensitive files** (`.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`,
`id_rsa`, `id_ed25519`, `credentials.json`, `secrets.yml`, ...) are excluded
from indexing entirely, **always**, independent of `redact_secrets`. Most lack
a code extension and were already skipped; the deny-list is the explicit,
documented guarantee and also catches code-extension'd sensitive names.

**Toggling on an existing index.** Hashes ignore the redaction flag, so enabling
redaction on a populated index would otherwise be a no-op. cartog records the
policy in `metadata` and force-reindexes every file on the next run when it
changes, scrubbing already-stored content, and prints a one-time notice.

## SQLite Tuning

The database is a regenerable index — crash-recovery safety is traded for throughput.

| Pragma | Value | Rationale |
|--------|-------|-----------|
| `journal_mode` | WAL | Write-Ahead Logging enables concurrent readers. Watch thread and MCP server read while indexer writes |
| `busy_timeout` | 5000 (5 s) | Bounded retry on a locked DB. WAL removes reader-vs-writer contention but not writer-vs-writer (one WAL writer at a time) or reader-vs-checkpoint contention. Without it, a connection hitting those locks fails immediately with `SQLITE_BUSY`. WAL and `busy_timeout` are both required: WAL for read concurrency, `busy_timeout` so the rarer write/checkpoint contention waits instead of aborting. Applied to every on-disk connection, including the WAL-checkpoint connection |
| `foreign_keys` | ON | Enforce referential integrity |
| `synchronous` | NORMAL | Reduced fsync frequency. Safe with WAL for a regenerable index — power failure loses at most the last transaction, recoverable via `cartog index --force` |
| `cache_size` | -65536 (64 MB) | Large page cache for repeated queries in MCP sessions |
| `temp_store` | MEMORY | Temp tables in RAM, not disk |
| `mmap_size` | 268435456 (256 MB) | Memory-mapped I/O for large index scans |

### Schema overview

```text
┌──────────────────────────────────────────────────────────┐
│ Core tables                                              │
│                                                          │
│  symbols ──────────── edges ──────── files    metadata   │
│  (id, name, kind,     (source_id,    (path,   (key,     │
│   file_path, lines,    target_name,   hash,    value)    │
│   signature,           target_id,     lang)              │
│   content_hash,        kind, line,                       │
│   subtree_hash, ...)   resolution_state,                 │
│                        resolution_source)                │
├──────────────────────────────────────────────────────────┤
│ RAG tables                                               │
│                                                          │
│  symbol_content ──→ symbol_fts (FTS5 virtual table)      │
│  (symbol_id,         (symbol_name, normalized_name,      │
│   content,            content)                           │
│   header,            Synced via INSERT/DELETE triggers    │
│   normalized_name)                                       │
│                                                          │
│  symbol_embedding_map ──→ symbol_vec (vec0 virtual table)│
│  (id INTEGER ↔            (embedding float[384])         │
│   symbol_id TEXT)         KNN via MATCH operator          │
│  Bridge: vec0 requires integer rowids,                   │
│          symbol IDs are text strings                     │
└──────────────────────────────────────────────────────────┘

Indexes (9): symbols(name, kind, file, parent),
             edges(source, target, target_id, kind),
             embedding_map(symbol_id)
```

## Minimum Supported Rust Version

1.80+ (edition 2021). Declared in `Cargo.toml` as `rust-version = "1.80"`. (1.80
is required for the `[lints] check-cfg` manifest key used to declare the
`cfg(loom)` cfg for the `cartog-loom-models` model-checking crate.)

## Further Reading

- [Product Overview](../product.md) — purpose, target users, differentiation
- [Project Structure](../structure.md) — module layout, conventions
- [CLI Reference](../reference/cli.md) — all commands
- [Configuration Reference](../reference/config.md) — all `.cartog.toml` keys
