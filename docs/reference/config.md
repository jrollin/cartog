# cartog Configuration Reference

> CLI commands: [cli.md](cli.md) | MCP tools: [mcp-tools.md](mcp-tools.md)

## Database path resolution

cartog resolves the database path using the following priority (highest wins):

| Priority | Source | Example |
|----------|--------|---------|
| 1 | `--db` flag or `CARTOG_DB` env var | `cartog --db /tmp/proj.db index .` |
| 2 | `.cartog.toml` in the project | `[database]\npath = "..."` |
| 3 | Auto git-root detection | `<root>/.cartog/db.sqlite` (legacy `<root>/.cartog.db` still read if only it exists) |
| 4 | Current directory fallback | `.cartog/db.sqlite` in cwd |

> **Migrating from `.cartog.db`** — older versions of cartog stored the database
> as `.cartog.db` at the project root. New installs default to
> `.cartog/db.sqlite`. Existing legacy files keep working (a one-shot warning
> is printed). Run `cartog self migrate-db` (add `--dry-run` to preview) to
> move the database and its WAL/SHM/backup siblings into `.cartog/`.

### Project config: `.cartog.toml`

Place `.cartog.toml` at the root of your project (or commit it to version control):

```toml
[database]
# Absolute path, or use ~ for home directory expansion
path = "~/.local/share/cartog/myproject.db"
```

This is useful when:
- Indexing from a parent directory that contains multiple projects
- Storing the DB outside the repo (e.g., to avoid committing it)
- Sharing a consistent DB location across team members via `.cartog.toml`

### Override examples

```bash
# Explicit flag (highest priority)
cartog --db /tmp/myproj.db index .
cartog --db /tmp/myproj.db search foo

# Environment variable
CARTOG_DB=~/.local/share/cartog/myproject.db cartog index .

# --db applies globally to all subcommands
cartog --db /tmp/x.db stats
cartog --db /tmp/x.db map
```

## Embedding provider configuration

For a task-oriented guide to switching providers, see [../how-to/switch-embedding-provider.md](../how-to/switch-embedding-provider.md).

Configure the embedding provider in `.cartog.toml`:

These options are mutually exclusive — use only one `[embedding]` section per `.cartog.toml`.

**Watcher auto-embed** (keep the local ONNX provider, control when embeddings run):

```toml
# omitted / unset → auto-detect: embed only if the repo already has embeddings
# auto_embed = true  → always auto-embed (even a never-indexed repo)
# auto_embed = false → never auto-embed
# Precedence: CARTOG_WATCH_RAG env > this key > --rag flag.
[embedding]
auto_embed = true
```

**Ollama provider:**

```toml
[embedding]
provider = "ollama"
model = "nomic-embed-text"

[embedding.ollama]
base_url = "http://localhost:11434"
```

**OpenAI-compatible `/v1/embeddings` endpoint** (OpenAI, Mistral, Voyage, Jina, OVHcloud, or a local server like Ollama /v1, LM Studio, vLLM):

```toml
[embedding]
provider = "openai"
model    = "text-embedding-3-small"

[embedding.openai]
base_url    = "https://api.openai.com/v1"  # or http://localhost:11434/v1 (Ollama), etc.
api_key_env = "OPENAI_API_KEY"             # env var NAME, not the key itself
```

**Concurrent embedding requests (ollama/openai):**

```toml
[embedding]
max_concurrent_requests = 4   # in-flight HTTP embed requests; 1..16, default 4
```

`max_concurrent_requests` caps how many embedding requests are in flight at once
for the network providers (the local ONNX provider ignores it — it parallelizes
via `[embedding.local] intra_threads`). Default **4**, clamped `1..=16`; `1` is
serial. `CARTOG_EMBED_CONCURRENCY` overrides it (env > TOML > default). It is a
transport setting, not part of the embedding fingerprint, so changing it never
forces a re-embed. Note: the fan-out is currently gated behind a live
batch-composition parity check; until that passes, both providers run serially
regardless of this value.

**Provider options:**

| Provider | Config | Setup | Notes |
|----------|--------|-------|-------|
| `local` (default) | No config needed | `cartog rag setup` to download models | ONNX Runtime via fastembed, ~230MB models |
| `ollama` | `provider = "ollama"` | Ollama server running with model pulled | No model download needed, dimension auto-detected. Compiled into every default build; **local ONNX stays the default provider** — set `provider = "ollama"` to use it. |
| `openai` | `provider = "openai"` | Reachable OpenAI-compatible `/v1` endpoint; API key in an env var (keyless for local servers) | One generic client for OpenAI, Mistral, Voyage, Jina, OVHcloud AI Endpoints, Together/Fireworks/DeepInfra, and local `/v1` servers (Ollama, LM Studio, vLLM) — switch vendors by changing `base_url`. Dimension auto-detected. **API key read from the `api_key_env` env var, never stored in `.cartog.toml`**; unset → no auth header (keyless local). Compiled into every default build; opt in with `provider = "openai"`. Azure OpenAI is not supported (its `…/deployments/{id}/embeddings?api-version=…` path + `api-key:` header differ from the plain `/v1` + `Bearer` shape). |

**Default models (local provider):**

| Role | Config value | HuggingFace repo (downloaded) | Dim | Size |
|------|-------------|-------------------------------|-----|------|
| Embedding | `BAAI/bge-small-en-v1.5` | `Qdrant/bge-small-en-v1.5-onnx-Q` (ONNX-quantized) | 384 | ~80MB |
| Reranker | `jinaai/jina-reranker-v1-turbo-en` (default) | `jinaai/jina-reranker-v1-turbo-en` | — | ~150MB |

The embedding config value is the fastembed model code you set under `[embedding]
model`; cartog downloads the matching ONNX-quantized repo from HuggingFace into the
shared model cache (`$FASTEMBED_CACHE_DIR`, else `$XDG_CACHE_HOME/cartog/models`, else
`~/.cache/cartog/models`). English-only — non-English identifiers/comments get
degraded embeddings. Override the embedding model with any fastembed built-in via
`[embedding] model = "..."`.

An unknown `provider` value (embedding: `local`, `ollama`, `openai`; reranker: `local`, `none`) is rejected when `.cartog.toml` is loaded, with an error naming the bad value — a typo like `provider = "ollma"` fails fast instead of silently falling back to the default.

**Advanced local configuration:**

```toml
[embedding]
provider = "local"
model = "BAAI/bge-base-en-v1.5"    # any fastembed built-in model

[embedding.local]
query_prefix = "search_query: "     # for asymmetric models
document_prefix = "search_document: "
intra_threads = 4                   # cap ONNX CPU threads (default: all cores)
```

`intra_threads` **caps** the ONNX Runtime threads used while embedding
(`rag index`) and reranking. Default: **all cores** (fastembed's default); set
this to leave headroom on a busy machine (e.g. `intra_threads = 4`). The
`CARTOG_ONNX_THREADS` env var overrides it (e.g. `CARTOG_ONNX_THREADS=1`); env >
TOML > uncapped. Read at provider load, so restart `cartog serve` to change it.

**Reranker model** — the cross-encoder is configurable, mirroring `[embedding]
model`. The value is a fastembed reranker HuggingFace repo path; unset uses the
default (`jinaai/jina-reranker-v1-turbo-en`, ~150MB — small, fast, and higher
BEIR NDCG@10 than the older `bge-reranker-base`):

```toml
[reranker]
provider = "local"                              # "local" (default) | "none"
model    = "BAAI/bge-reranker-base"             # opt back to the former default (~1.1GB)
# model  = "jinaai/jina-reranker-v2-base-multilingual"  # multilingual (~300MB)
```

The reranker is not persisted, so switching models needs no re-index — the change
takes effect on the next search (a new model downloads once; a previously-used one
is reused from cache). Existing users who never pinned `model` are switched to the
new default automatically; pin `model = "BAAI/bge-reranker-base"` to keep the old
one (it reuses the already-downloaded weights). See
[troubleshooting](../troubleshooting.md) to reclaim the orphaned `bge-reranker-base`
cache.

**Disable re-ranking** (skips the ~150MB reranker download):

```toml
[reranker]
provider = "none"
```

## Hybrid search tuning (`[rag]`)

The `[rag]` section controls retrieval behaviour for `cartog rag search`. Defaults are tuned for the common case; override only when profiling reveals a bottleneck.

```toml
[rag]
retrieval_multiplier = 3   # over-retrieve N× results before fusion (default: 3)
retrieval_floor = 20       # minimum candidates to retrieve regardless of limit (default: 20)
rerank_max = 50            # max candidates sent to the cross-encoder reranker (default: 50)
rerank_min = 8             # skip reranker if fewer than this many candidates survive RRF (default: 8)
```

- `retrieval_multiplier` — both FTS5 and vector search retrieve `limit × retrieval_multiplier` candidates before Reciprocal Rank Fusion. Larger values improve recall at the cost of more reranker work.
- `retrieval_floor` — minimum number of candidates to retrieve, regardless of `limit`. Prevents degenerate behaviour on very small `--limit` values.
- `rerank_max` — caps the number of RRF-merged candidates forwarded to the cross-encoder. Lowering it speeds up reranking; raising it improves precision at the tail.
- `rerank_min` — if fewer than this many candidates survive RRF merge, the cross-encoder is skipped entirely. Avoids model overhead when the result set is already small.

## Remote storage (`[remote]`)

For a task-oriented guide to push/pull setup, see [../how-to/set-up-s3-sync.md](../how-to/set-up-s3-sync.md).

Opt in to sharing a pre-built index over an S3-compatible bucket by adding a `[remote]` section. This is **off by default** — no push or pull happens until the section is configured.

```toml
[remote]
url        = "s3://my-team-bucket/cartog/main"
# region   = "us-east-1"        # optional when endpoint is set
# endpoint = "https://minio.example.com"  # MinIO / Cloudflare R2 / floci
# path_style = true             # set true for most non-AWS endpoints
```

| Key | Default | Notes |
|-----|---------|-------|
| `url` | required | `s3://bucket/key` target. `--remote` on push/pull overrides it. |
| `region` | AWS-resolved | AWS region (e.g. `us-east-1`). Optional when `endpoint` is set. |
| `endpoint` | AWS S3 | Custom endpoint for S3-compatible stores. |
| `path_style` | `false` | Force path-style addressing. Set `true` for most non-AWS endpoints. |

**Credentials are never stored in `.cartog.toml`.** They are resolved exclusively from the standard AWS environment chain (env vars, `~/.aws` profile, or IMDS). Any credential-shaped key in `[remote]` (`access_key`, `secret_key`, `aws_*`, …) is rejected at parse time so a secret can't be committed by mistake.

Requires the binary to be built with the default `remote-s3` feature. `cartog doctor` verifies reachability.

## Secret redaction

cartog scrubs common secret patterns from indexed symbol text and skips
sensitive files. **On by default.** Disable with:

```toml
[security]
redact_secrets = false
```

- Redacts AWS keys, GitHub PATs, Slack/Stripe tokens, JWTs, and quoted
  `password`/`secret`/`token`/`api_key` assignments, replacing them with
  `[REDACTED_SECRET]` in stored content, signatures, docstrings, and embeddings.
  Best-effort: not every secret is caught (see
  [../explanation/architecture.md](../explanation/architecture.md#secret-redaction)).
- Always excludes sensitive files (`.env`, `*.pem`, `id_rsa`,
  `credentials.json`, ...) from indexing, regardless of `redact_secrets`.
- Toggling `redact_secrets` triggers a one-time full re-index so already-stored
  content is scrubbed (or restored); a notice is printed.

## Index exclude globs

For `.gitignore` interaction and the built-in prune list, see [`.gitignore` awareness](#gitignore-awareness) below.

cartog already prunes common dependency and build directories (`node_modules`,
`target`, `vendor`, `dist`, `build`, `.dart_tool`, dot-directories, ...). To skip
additional paths — vendored or generated trees under non-standard names that the
built-in list misses — add repo-root-relative globs:

```toml
[index]
exclude = ["vendor/**", "third_party/**", "**/*.generated.*"]
```

- Globs are repo-root-relative and match both files and directories. A matched
  **directory is pruned** (never descended into), so excluding a large vendored
  tree also saves the walk cost.
- `dir/**` excludes everything under `dir` and prunes `dir` itself; `**/*.ext`
  excludes files by extension anywhere in the tree.
- `*` and `?` are **segment-local** (they don't cross `/`), like `.gitignore`.
  `src/*` matches every direct entry of `src` — including subdirectories, which
  are then pruned — so it effectively excludes the whole of `src` (same as
  `.gitignore`). To exclude only files at one level, use a suffix glob like
  `src/*.rs`; to exclude a whole subtree explicitly, use `src/**`.
- A malformed or empty glob is rejected when the config loads, not at first index.
- The same globs are honored by `cartog watch` / `serve --watch`, so the live
  index and a manual `cartog index` always agree on scope.
- This is a cartog-specific filter applied on top of `.gitignore` and the
  built-in prune list (see below).

Run `cartog index --force <path>` after changing `exclude` so paths that are now
excluded are removed from an existing index.

## Parse parallelism

The parse phase runs across a rayon worker pool. Cap it for low-CPU or
memory-constrained hosts (e.g. shared CI):

```toml
[index]
jobs = 4
```

- Absent or `0` = auto (`available_parallelism`, i.e. CPU count); any value is
  clamped to `1..=64`. (`0` means auto, not serial — use `1` for single-threaded.)
- Precedence: `cartog index --jobs N` flag > `CARTOG_JOBS` env > this key.
- The parse phase runs inside a dedicated pool sized to this value, so the cap
  applies on **every** index, including under a long-lived `cartog serve` /
  `serve --watch`. The `--jobs` flag is per-invocation (the `index` command);
  for daemons set the env var or this key.

## `.gitignore` awareness

cartog honors `.gitignore` (and `.git/info/exclude`) by default, including
**nested** `.gitignore` files in subdirectories — so vendored or generated trees
that git already ignores (`node_modules`, a CocoaPods `Pods/`, build output, …)
are skipped without any cartog config. `.gitignore` is applied even when the
tree has no `.git` directory.

- A `.cartogignore` file (same syntax as `.gitignore`, including negation and
  nested files) adds cartog-specific ignores and is always honored.
- The built-in prune list (`node_modules`, `target`, `vendor`, `dist`, `build`,
  `.dart_tool`, dot-directories, …) and `[index] exclude` apply **on top** —
  they prune even paths that `.gitignore` would have kept (e.g. a repo that
  `!`-unignores `node_modules`).
- To index files git ignores (e.g. committed generated code), opt out:

```toml
[index]
respect_gitignore = false
```

  The built-in prune list and `[index] exclude` still apply with the opt-out on.

## LSP server overrides

For a task-oriented guide, see [../how-to/configure-lsp-servers.md](../how-to/configure-lsp-servers.md).

By default cartog resolves a language's LSP server from `PATH` (e.g.
`rust-analyzer`, `gopls`). `[lsp.<lang>]` overrides that with an explicit
command — most usefully a Dockerized server, so cartog can resolve edges on a
host without the language's native toolchain installed:

```toml
[lsp.dart]
command = ["docker", "run", "--rm", "-i",
           "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-dart:stable"]
```

- `command` is the full argv; `command[0]` is the executable (looked up on
  `PATH` or given as an absolute path), the rest are its arguments.
- `${ROOT}` in any element expands to the indexed project root (host-absolute).
- **Path mirroring is required.** cartog talks to the server over stdio using
  `file://` URIs built from the host path. A container must therefore see the
  repo at the *same* path — hence `-v ${ROOT}:${ROOT} -w ${ROOT}`. A container
  path that differs from the host path will make every definition resolve as
  "external" and is unsupported.
- The override only applies to the keyed cartog language (`dart`, `go`,
  `python`, ...); it must be a language cartog already supports.
- The server's stderr is logged to `${TMPDIR}/cartog-lsp/<language>.log`.
- cartog sends `processId: null` in the LSP `initialize` for an override server
  (a native server gets cartog's real PID). A container runs in its own PID
  namespace where cartog's host PID does not exist, so a real `processId` would
  trip the LSP parent-liveness check and make some servers (notably pyright and
  typescript-language-server) exit at startup. Handled automatically — no config
  needed. The override server is still reaped when cartog exits normally.

Run `cartog index --force <path>` after adding an override; the server is
spawned during the LSP edge-resolution pass.

## Concurrent LSP servers

```toml
[lsp]
max_concurrent_servers = 2
```

- Absent or `0` = auto (`min(languages_in_pass, 4)`). `CARTOG_LSP_MAX_SERVERS`
  overrides (env > TOML); `1` forces serial.
- Each server is RAM-heavy (rust-analyzer ~1-2GB resident). Lower the cap on a
  constrained host; most repos have fewer than 4 LSP languages so the cap rarely
  binds.
- Applies to `cartog index` with no live `cartog serve` peer (a bare index). When
  a serve peer holds the DB, the index defers its LSP pass to that warm peer,
  which resolves serially. Resolution output is byte-identical regardless of cap.

## Compile-time feature flags

```bash
cargo install cartog                                    # default: LSP + S3 sync + Ollama + OpenAI providers (local ONNX is the runtime default)
cargo install cartog --no-default-features              # minimal: drops LSP, S3 sync, Ollama, and OpenAI
cargo install cartog --no-default-features --features lsp  # selective: LSP only
```

## Environment variables

Runtime overrides (per-machine / per-invocation), in addition to `.cartog.toml`:

| Variable | Default | Effect |
|----------|---------|--------|
| `CARTOG_DB` | auto-detect | Database path (same as `--db`). |
| `CARTOG_JOBS` | CPU count | Parse worker pool size for `cartog index` (clamped `1..=64`). Overrides `[index] jobs`; the `--jobs` flag overrides it. |
| `CARTOG_LSP_MAX_SERVERS` | `min(langs, 4)` | Max concurrent LSP servers in the indexer's edge pass. Overrides `[lsp] max_concurrent_servers`; `1` forces serial. |
| `CARTOG_ONNX_THREADS` | all cores | Caps ONNX CPU threads for `rag index` + reranking. Overrides `[embedding.local] intra_threads`. `1` forces single-core. |
| `CARTOG_EMBED_CONCURRENCY` | `4` | In-flight HTTP embed requests for ollama/openai (clamped `1..=16`). Overrides `[embedding] max_concurrent_requests`. Ignored for local. |
| `CARTOG_WATCH_RAG` | unset | Force watcher auto-embed; overrides `[embedding] auto_embed` and `--rag`:<br>`1` = force on<br>`0` = force off<br>unset = auto-detect from the DB |
| `CARTOG_SINGLE_WRITER` | `1` | `0` disables MCP single-writer election (every `cartog serve` opens read-write). |
| `CARTOG_MCP_MAX_BYTES` | `65536` | Max bytes per MCP tool response before truncation. |
| `CARTOG_MCP_COMPACT` | `1` (on) | MCP tools strip heavy fields by default (symbol cache hashes + docstrings; `cartog_rag_search`/`cartog_trace` bodies bounded to a ~500-byte snippet; `cartog_context` keeps budgeted bodies). Set `0`/`false`/`no`/`off` to return full bodies. |
| `CARTOG_NO_UPDATE_CHECK` | unset | Set to skip the background self-update check. |
| `CARTOG_UPDATE_CHECK` | unset | Force an update check regardless of cadence. |
| `CARTOG_GITHUB_API_URL` | GitHub releases API | Override the GitHub API endpoint used for update checks. Useful in air-gapped environments or integration tests. |
| `CARTOG_PLUGIN_JSON` | unset | Explicit path to the plugin manifest file, used by the MCP server to discover a deferred-update pin. |
| `CARTOG_PROGRESS` | unset | Set to any value to force the progress spinner on non-TTY output (e.g. CI). Normally auto-gates to TTY only. |
| `CARTOG_LOG_DIR` | `~/.cache/cartog` | Directory for the `last-update` breadcrumb written after a self-update. Respects `$XDG_CACHE_HOME` when set. |
| `CARTOG_LSP_READY_TIMEOUT_SECS` | `20` | Seconds to wait for an LSP server to finish loading its project model before the first definition request. |
