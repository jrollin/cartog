# cartog — Technology Stack

## Language

**Rust** — Performance, tree-sitter native bindings, single binary distribution.

## Core Dependencies

| Crate | Purpose | Notes |
|-------|---------|-------|
| `tree-sitter` 0.26 | Incremental parsing, CST traversal | Pinned — grammar crates lag by one minor |
| `tree-sitter-{lang}` 0.2–0.25 | Per-language grammars (Python, TS/JS, Rust, Go, Ruby, Java, PHP, Dart, Markdown) | Each ~1-2 MB of generated C |
| `rusqlite` (bundled) | SQLite storage, zero external deps | `bundled` compiles SQLite from C source — no system `libsqlite3-dev` required. Critical for cross-compilation to 4 release targets |
| `clap` (derive) | CLI argument parsing | `ValueEnum` derive for type-safe `--kind` filters with shell completion |
| `serde` + `serde_json` | JSON serialization for `--json` output | `to_string_pretty` for readability in both terminal and agent contexts |
| `walkdir` | Recursive directory traversal | `follow_links(true)` — enables symlinked monorepo layouts |
| `sha2` | SHA-256 content hashing | Change detection fallback when git is unavailable |
| `notify` + `notify-debouncer-mini` | Filesystem event watching | "mini" debouncer chosen over full — fewer deps, simpler API, sufficient for single-project use |
| `ctrlc` | Graceful Ctrl+C handling | Flushes pending RAG embeddings before exit |
| `anyhow` | Error propagation | `Result<T>` everywhere, no `unwrap()` in library code |
| `rmcp` (server + transport-io) | MCP server over stdio | Server-only — cartog is never an MCP client. stdio transport matches how agents launch subprocesses |
| `tokio` (rt-multi-thread) | Async runtime for MCP server only | Multi-thread for `spawn_blocking` throughput. Runtime created on-demand — sync commands skip it entirely |
| `tracing` + `tracing-subscriber` | Structured logging to stderr | Logs to stderr so stdout stays clean for output and MCP protocol |
| `fastembed` | ONNX Runtime inference for embeddings + re-ranking (local provider) | Optional via `provider-local` feature (default on). `default-features = false` drops image models (CLIP etc.). `rustls-tls` avoids OpenSSL system dependency |
| `reqwest` | HTTP client for self-update + remote embedding providers (Ollama) | Non-optional in the `cartog` binary (self-update). The Ollama provider in `cartog-rag` is gated by `provider-ollama`, which the binary enables by default (`ollama-embedding`). Uses `blocking` + `rustls-tls` |
| `sqlite-vec` | Vector similarity search (KNN) in SQLite | `vec0` virtual table, requires integer rowids (bridged via `symbol_embedding_map`) |
| `criterion` (dev) | Micro-benchmarks | Four `[[bench]]` targets — see [Benchmarks](#benchmarks). Inputs/results are `black_box`-wrapped so µs-scale benches measure real work |
| `rust-s3` 0.37 (`tokio-rustls-tls`) | S3-compatible client for `cartog push` / `cartog pull` | Optional via `remote-s3` feature (default on). Chosen over `aws-sdk-s3` for size (~5 MB vs ~18 MB); supports AWS S3, MinIO, R2, floci |

## Build Profiles

| Setting | Value | Rationale |
|---------|-------|-----------|
| `dev.opt-level` | `1` | Tree-sitter C grammars are machine-generated huge files that compile very slowly at opt-level 0. Level 1 also makes indexing usably fast during development |
| `release.lto` | `"thin"` | Most binary size / performance benefits of full LTO at a fraction of the link time |
| `release.strip` | `"debuginfo"` | Removes DWARF sections (~50% binary size reduction) but keeps function names in panic backtraces for diagnosable crash reports |

## Benchmarks

Two distinct surfaces, both rooted in `benchmarks/fixtures/` (8 language webapps):

- **Shell suite** (`benchmarks/run.sh`, 13 scenarios × fixtures) — token efficiency
  and recall versus grep/cat. Run with `make bench`.
- **Criterion micro-benchmarks** — in-process latency. The guiding rule: benchmark
  cartog's own CPU-bound work; anything dominated by an external service (the
  Ollama daemon, S3) gets a *correctness* test at the boundary, not a latency
  bench, so numbers never measure infrastructure.

Criterion benches are split into four `[[bench]]` targets so the ONNX boundary is
expressed by target membership — CI runs the three runtime-ONNX-free targets and
simply never names the fourth (criterion's regex filter cannot express exclusion):

| Target | Crate | Scope | Runtime ONNX | CI |
|--------|-------|-------|--------------|-----|
| `queries` | `cartog` | 8 query ops (search/refs/impact/outline/callees/hierarchy/deps/stats), Python + Java | no | ✅ |
| `indexing` | `cartog-indexer` | `index_full_force/<lang>` over all 8 fixtures + 2 incremental scenarios | no (crate has no `cartog-rag` dep) | ✅ |
| `rag_search` | `cartog` | `hybrid_search` (FTS5 + vector KNN + RRF) via a deterministic stub provider | no (stub vectors) | ✅ |
| `rag_onnx` | `cartog` | real fastembed embed + cross-encoder rerank | **yes** | ❌ opt-in (`make bench-onnx`) |

Conventions: every `b.iter` input and result is wrapped in `std::hint::black_box`
so the compiler cannot constant-fold literal inputs or eliminate unused results —
without it the µs-scale query benches would risk measuring nothing. Query latency
is language-agnostic (same SQL regardless of source language), so it is benched on
Python + Java only; per-language cost lives in the tree-sitter grammar/extractor,
so `index_full_force` is parameterized across all 8 fixtures. The shared scenario
bodies live in `cartog_indexer::bench_support` so `queries` and `indexing` cannot
drift. On PRs the CI `bench` job establishes a same-runner baseline at the merge
base and reports a `--baseline` delta (controlling for runner variance); it is
`continue-on-error`, so a noisy result never blocks. Run everything ONNX-free
locally with `make bench-criterion`.

## Architecture Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Parser | tree-sitter (CST + query API) | Incremental, multi-language, structural. JS/TS and Python use declarative tree-sitter queries for call/throw/type-ref extraction; other languages use cursor walks. Handles 90% of what LSP provides without running a language server |
| Storage | SQLite (single `.cartog/db.sqlite`) | Zero infra, ~1 MB, persists across sessions. WAL mode enables concurrent readers (watcher + MCP server). Legacy `.cartog.db` at the project root is still read for backwards-compatibility |
| Packaging | Skill (primary) | Changes agent workflow, not just adds a tool. Works with any LLM that has bash access |
| MCP server | `cartog serve` (stdio) | Skill remains primary; MCP as secondary for zero-context-cost tool access. 1:1 mapping with CLI commands — same `db.*()` code paths |
| Change detection | Layered: git diff → file SHA-256 → symbol Merkle (+ `--force`) | Each layer prunes a different unit (files, then symbols). Full design, layer diagram, hash invariants, and failure modes in [architecture/incremental-indexing.md](architecture/incremental-indexing.md) |
| Edge resolution | Name-based, scope-aware, multi-pass, scoped | 6-tier priority: same file > import-path > same dir > parent scope > unique global > kind disambiguation (type def > function > method). Two passes so import edges resolved in pass 1 feed pass 2. Incremental mode: invalidates dangling edges after symbol changes, then re-resolves only affected edges. Each resolved edge records the resolving tier/source in `edges.resolution_source` (`EdgeProvenance`), surfaced as `provenance` in `--json`/MCP output; LSP overwrites the heuristic tag, reset paths clear it |
| Symbol ID | `file_path:kind:qualified_name` | Stable across line movements. `kind` = function/class/method/etc. Qualified name encodes parent chain: `auth.py:method:TokenService.validate`. Deterministic, human-readable, no UUIDs |
| Ignore strategy | Hardcoded 18 dirs + `starts_with('.')` | No `.gitignore` parsing — simpler, faster, predictable. Covers node_modules, \_\_pycache\_\_, target, venv, dist, build, .next, vendor, etc. |
| Content truncation | 2048 bytes per symbol | ~512 tokens at code's ~2-3 chars/token ratio. Captures signature + leading body. Below 50 bytes → excluded (noise) |
| Name normalization | camelCase/snake_case splitting for FTS5 | `validateToken` → `"validate token"`, `get_http_response` → `"get http response"`. Stored in FTS5 alongside original name |
| Serialization (embeddings) | Raw little-endian f32 bytes | Zero-overhead, directly compatible with sqlite-vec `MATCH` operator. 384 dims × 4 bytes = 1536 bytes per symbol |
| Language detection | File extension | Simple, predictable, covers 99%. TSX is a separate key from TS (different grammar for JSX nodes) |
| Async boundary | Manual `tokio::Runtime` for `serve` only | 95% of commands are sync. Avoids async overhead for index/search/refs. `spawn_blocking` offloads sync SQLite calls from the async MCP handler |
| DB concurrency | `Arc<Mutex<Database>>` | Single connection, not a pool. MCP serves one agent session — contention is negligible. `std::sync::Mutex` (not tokio) because lock is never held across `.await` |
| Path security | Canonical CWD validation | MCP tool parameters come from LLM agents. Rejects paths outside CWD subtree via `canonicalize` + `starts_with`. Defense-in-depth against prompt injection |
| Secret redaction | Default-on, best-effort | Scrubs common secret patterns from stored symbol text and skips sensitive files. See [Secret redaction](#secret-redaction) below |
| Watch mode | Debounced re-index + deferred RAG | 5s debounce, 30s RAG delay. Embedding only fires after editing stops — avoids embedding code that changes seconds later |
| Vector search | sqlite-vec (opt-in) | Embedded in SQLite, no external infra. Models downloaded via `cartog rag setup` |
| Model cache | `~/.cache/cartog/models` | XDG-compliant shared cache avoids downloading ~1.2 GB of models per project. Precedence: `FASTEMBED_CACHE_DIR` > `XDG_CACHE_HOME/cartog/models` > `~/.cache/cartog/models` |
| Output format | Human default + `--json` flag (global) | Readable for humans, parseable for scripts. Both `cartog --json stats` and `cartog stats --json` work |
| Distribution | `cargo install` + pre-built binaries | GitHub Releases for 4 targets (Linux x86/ARM, macOS ARM, Windows x86), crates.io publish, in-place upgrade via `cartog self update` |
| LSP | Auto-detected (default feature) | Index-time refinement for edges unresolved by heuristics. Auto-detects language servers on PATH (rust-analyzer, pyright, typescript-language-server, gopls, ruby-lsp, solargraph, jdtls, intelephense), sends `textDocument/definition`, shuts down after. Silently skips when no server found. Ready-timeout 20s (override via `CARTOG_LSP_READY_TIMEOUT_SECS`). Edges LSP cannot map in-graph are persisted as `resolution_state=2` (truly unresolvable: typo, dyn dispatch, macro) or `=3` (external: stdlib, deps, node_modules); both are skipped on subsequent runs until a matching symbol is added. Disable at runtime with `--no-lsp`; opt out at build time with `cargo install cartog --no-default-features` |
| MCP response cap | 64 KB per tool result | Prevents oversized JSON from evicting agent context. Truncates at UTF-8 boundary with narrowing hint per tool. Override via `CARTOG_MCP_MAX_BYTES` |
| RAG tuning | `[rag]` section in `.cartog.toml` | `retrieval_multiplier`, `retrieval_floor`, `rerank_max`, `rerank_min` control FTS5/vector candidate pool size and cross-encoder cost. See [usage.md](usage.md#configuration) |
| Workspace | Cargo workspace (10 crates) | Incremental compilation, explicit dependency boundaries, independent crate reuse. See [structure.md](structure.md) for layout and dependency graph |
| Monorepo | Deferred | Index from CWD, user can `cd` into subproject |
| Remote index sync | Opt-in S3-compatible push/pull (default-on feature, inert without config) | `remote-s3` feature ON by default — single distributable binary, no rebuild from source for teams. Inert until `[remote]` is set or `--remote` passed: no network traffic, no impact on air-gapped use. Credentials resolved from the AWS env chain only; `.cartog.toml` rejects credential-shaped keys at parse time. `rust-s3` (~5 MB) over `aws-sdk-s3` (~18 MB) for binary size. Push records git-commit provenance (`x-amz-meta-git-commit` from the index's `last_commit`); pull reports it and cross-checks header vs file, but never blocks on staleness — the read-mostly "CI builds, devs pull" flow. See [usage.md](usage.md#cartog-push---remote-s3-url) |

### Secret redaction

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

## RAG Pipeline Design

### Why small models

BERT attention is **O(n²) in sequence length**. Keeping input short is the single biggest performance lever — more impactful than model size, quantization, or batching.

This drives two key decisions:

1. **Small embedding model** — BGE-small-en-v1.5 quantized (default, 384 dimensions). 2-3x faster than full precision with negligible quality loss for code symbol matching. Outputs are L2-normalized, enabling L2 distance in sqlite-vec (equivalent to cosine ranking). Model and dimension are configurable via `.cartog.toml`. Trade-off: English-only model — non-English identifiers/comments get degraded embeddings.

2. **AST-aware embedding text** — For code: header + signature + significant body lines (skipping blanks, comments, closing braces) up to ~200 tokens (~800 bytes):
   ```
   // File: auth/tokens.py | function validate_token
   def validate_token(token: str) -> bool:
       if token.is_expired():
           raise TokenError('expired')
       return lookup_session(token.session_id)
   ```
   For Markdown documents: heading text in the header field + section body:
   ```
   // File: docs/design.md
   // Type: document
   // Name: Authentication
   Users authenticate via JWT tokens. The server validates
   the token signature and checks expiration...
   ```
   This captures the "what does this function/section do" signal (~100-200 tokens) while staying within the model's 512-token window. Full source content is still stored separately for FTS5 keyword search and cross-encoder re-ranking. Decorators/annotations are kept (they carry semantic meaning like `@login_required`).

### What gets embedded (and what doesn't)

- **Functions, classes, methods**: embedded with AST-aware text (header + significant body lines)
- **Markdown documents**: chunked by heading, each section embedded with heading in header field. Large sections sub-chunked at paragraph boundaries (~1500 bytes). Files without headings use fixed-size paragraph chunking.
- **Variables**: excluded — too numerous, low signal for semantic search
- **Imports**: excluded at content extraction time — they exist as graph edges, not search targets

### Search pipeline

```
Query
  │
  ├─→ FTS5 keyword search (BM25 ranking)
  │     3-tier fallback: phrase → AND → OR
  │     Searches: symbol_name + normalized_name + full content
  │
  ├─→ Vector KNN search (sqlite-vec, L2 distance)
  │     L2-normalized embeddings → L2 distance ≡ cosine ranking
  │     Query embedded with configured provider (default: BGE-small-en-v1.5)
  │
  ├─→ Reciprocal Rank Fusion (RRF, k=60)
  │     Merges both ranked lists: score = Σ 1/(k + rank + 1)
  │     k=60 from Cormack et al. 2009
  │     Over-retrieval: max(limit × 3, 20) per source
  │
  └─→ Cross-encoder re-ranking (optional)
        BGE-reranker-base, scores (query, full_content) pairs jointly
        Capped at 50 candidates to bound latency
        Graceful degradation: tri-state cache (not attempted / failed / ready)
        If model unavailable → search works with RRF-only ordering
```

### FTS5 three-tier fallback (precision-first)

| Tier | Query form | Example for "validate token" |
|------|-----------|------------------------------|
| 1 | Phrase | `"validate token"` — exact adjacent match |
| 2 | AND | `"validate" AND "token"` — all terms, any order |
| 3 | OR | `"validate" OR "token"` — any term present |

Returns the first non-empty result. Only FTS5 syntax errors trigger fallback — real DB errors propagate.

### Key constants

| Constant | Value | Rationale |
|----------|-------|-----------|
| `EMBEDDING_DIM` | 384 | BGE-small-en-v1.5 output dimension (default, overridable per-provider) |
| `EMBED_BATCH_SIZE` | 64 | Limits ONNX padding waste when text lengths vary |
| `CHUNK_SIZE` | 512 | Symbols per embedding engine call |
| `DB_BATCH_LIMIT` | 256 | Pending DB writes before flush |
| `MAX_CONTENT_BYTES` | 2048 | ~512 tokens at code's char/token ratio (stored for FTS5 + reranker) |
| `MIN_CONTENT_BYTES` | 50 | Below this → noise, not embedded |
| `MAX_EMBED_TEXT_BYTES` | 800 | ~200 tokens for bi-encoder input (AST-aware significant lines) |
| `EMBEDDING_FORMAT_VERSION` | 2 | Auto-triggers re-embed when embedding strategy changes |
| `RERANK_MAX` | 50 | Cross-encoder candidate cap |
| RRF `k` | 60.0 | Standard constant from Cormack et al. 2009 |
| Over-retrieval | `limit × 3` (min 20) | Enough candidates for effective RRF merge |
| `MAX_SEARCH_LIMIT` | 100 | Hard cap on returned results |

### Provider architecture

Embedding providers implement the `EmbeddingProvider` trait with `embed_query()`/`embed_document()` separation for asymmetric models. Providers are selected at runtime via `.cartog.toml` (default `provider = "local"`) and gated behind Cargo feature flags (`provider-local`, `provider-ollama`). The `cartog` binary compiles both in by default; selecting Ollama is a config choice, not a rebuild.

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

```
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

1.77+ (edition 2021). Declared in `Cargo.toml` as `rust-version = "1.77"`.

## Further Reading

- [Product Overview](product.md) — purpose, target users, differentiation
- [Project Structure](structure.md) — module layout, conventions
- [Usage](usage.md) — CLI commands, agent skill setup, MCP server per client
