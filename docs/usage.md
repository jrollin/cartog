# cartog — Usage

> For editor-specific setup (Neovim keymaps, VS Code tasks, Emacs compile-mode, Zed MCP), see [editor-integration.md](editor-integration.md). For common errors, see [troubleshooting.md](troubleshooting.md).

## Setup

Three install paths, pick whichever matches your environment:

```bash
# 1. Install script (macOS / Linux, no Rust required) — detects platform,
#    downloads + verifies the matching release binary, installs to
#    /usr/local/bin or ~/.local/bin.
curl -fsSL https://jrollin.github.io/cartog/install.sh | sh

# 2. Cargo (Rust 1.77+; lets advanced users strip features, e.g. --no-default-features).
cargo install cartog

# 3. Build from source.
cargo build --release && cargo install --path .
```

Override the install location with `CARTOG_INSTALL_DIR=...`; pin a version
with `CARTOG_VERSION=<version>` (e.g. the tag from [Releases](https://github.com/jrollin/cartog/releases)). To upgrade an existing install in place,
`cartog self update`. See [updates.md](updates.md) for the full `cartog self`
command surface, env vars, and rollback.

## Bootstrap

From the repo root, two commands to start. The third is optional.

```bash
cargo install cartog          # one-time, global
cartog init                   # 1. scaffold .cartog.toml (config only)
cartog index                  # 2. build the code graph

cartog ide                    # optional — wire MCP into installed editors
```

| Verb | When you need it | Files |
|---|---|---|
| `cartog init` | Always (once per project) | `.cartog.toml` only |
| `cartog index` | Always (after every code change) | `.cartog/db.sqlite` |
| `cartog ide` | Only if you want MCP in your editor | `.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json`, user-scope configs |

Edit `.cartog.toml` between steps 1 and 2 to change the DB path or embedding
provider before any heavy work runs. CLI-only users stop after step 2.

All three commands are idempotent. `cartog init` never overwrites an existing
`.cartog.toml`; `cartog ide` merges entries instead of clobbering (other MCP
servers in the file are preserved); `cartog index` is incremental.

## Configuration

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

### Embedding Provider Configuration

Configure the embedding provider in `.cartog.toml`:

```toml
# Default: BGE-small-en-v1.5 quantized embedding + jina-turbo reranker (no config needed)

# Auto-embed under `serve --watch` / `watch`:
#   omitted / unset → auto-detect: embed only if the repo already has embeddings
#   auto_embed = true  → always auto-embed (even a never-indexed repo)
#   auto_embed = false → never auto-embed
# Precedence: CARTOG_WATCH_RAG env > this key > --rag flag.
[embedding]
auto_embed = true

# Use Ollama instead of local ONNX
[embedding]
provider = "ollama"
model = "nomic-embed-text"

[embedding.ollama]
base_url = "http://localhost:11434"

# Or any OpenAI-compatible /v1/embeddings endpoint (OpenAI, Mistral, Voyage,
# Jina, OVHcloud, or a local server like Ollama /v1, LM Studio, vLLM)
[embedding]
provider = "openai"
model    = "text-embedding-3-small"

[embedding.openai]
base_url    = "https://api.openai.com/v1"  # or http://localhost:11434/v1 (Ollama), etc.
api_key_env = "OPENAI_API_KEY"             # env var NAME, not the key itself
```

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
[troubleshooting](troubleshooting.md) to reclaim the orphaned `bge-reranker-base`
cache.

**Disable re-ranking** (skips the ~150MB reranker download):

```toml
[reranker]
provider = "none"
```

### Secret redaction

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
  [tech.md](tech.md#secret-redaction)).
- Always excludes sensitive files (`.env`, `*.pem`, `id_rsa`,
  `credentials.json`, ...) from indexing, regardless of `redact_secrets`.
- Toggling `redact_secrets` triggers a one-time full re-index so already-stored
  content is scrubbed (or restored); a notice is printed.

**Compile-time feature flags**:

```bash
cargo install cartog                                    # default: LSP + S3 sync + Ollama + OpenAI providers (local ONNX is the runtime default)
cargo install cartog --no-default-features              # minimal: drops LSP, S3 sync, Ollama, and OpenAI
cargo install cartog --no-default-features --features lsp  # selective: LSP only
```

### Environment variables

Runtime overrides (per-machine / per-invocation), in addition to `.cartog.toml`:

| Variable | Default | Effect |
|----------|---------|--------|
| `CARTOG_DB` | auto-detect | Database path (same as `--db`). |
| `CARTOG_ONNX_THREADS` | all cores | Caps ONNX CPU threads for `rag index` + reranking. Overrides `[embedding.local] intra_threads`. `1` forces single-core. |
| `CARTOG_WATCH_RAG` | unset | Force watcher auto-embed on/off (`1`/`0`). Overrides `[embedding] auto_embed` and `--rag`. Unset = auto-detect from the DB. |
| `CARTOG_SINGLE_WRITER` | `1` | `0` disables MCP single-writer election (every `cartog serve` opens read-write). |
| `CARTOG_MCP_MAX_BYTES` | `65536` | Max bytes per MCP tool response before truncation. |
| `CARTOG_NO_UPDATE_CHECK` | unset | Set to skip the background self-update check. |
| `CARTOG_UPDATE_CHECK` | unset | Force an update check regardless of cadence. |
| `CARTOG_INSTALL_DIR` | `~/.local/bin` | Install location used by `install.sh`. |
| `CARTOG_VERSION` | latest | Pin the version `install.sh` fetches. |

---

## Semantic Search

### `cartog rag setup`

Download embedding and re-ranker models from HuggingFace. Run once before using RAG search.

```bash
cartog rag setup
```

**First-time download**: ~230MB of ONNX models (embedding ~80MB + reranker `jinaai/jina-reranker-v1-turbo-en` ~150MB). Models are cached in `~/.cache/cartog/models/` and reused across all projects — subsequent runs are instant. `rag setup` downloads the configured reranker, so pinning `[reranker] model` fetches that one instead.

Note: `rag setup` downloads models for the **local** provider only. When using Ollama, models are managed by the Ollama server — run `ollama pull nomic-embed-text` instead.

### `cartog rag index [path] [--force]`

Build the embedding index for semantic search. Requires `cartog index` and `cartog rag setup` first. Indexes both code symbols and Markdown documents (`.md` files).

```bash
cartog rag index              # embed all symbols + documents in CWD
cartog rag index src/         # embed a subdirectory
cartog rag index --force      # re-embed everything
```

After a cartog upgrade that changes the embedding strategy, `cartog rag index` automatically detects the format change and re-embeds all symbols — no `--force` needed.

### `cartog rag search <query> [--kind <kind>] [--limit N]`

Semantic search over code and documentation — use natural language to find code by what it does, or search project docs alongside code.

```bash
cartog rag search "validate authentication tokens"
cartog rag search "error handling" --kind function
cartog rag search "database connection" --limit 5
cartog rag search "deployment architecture" --kind document
```

Combines keyword (BM25/FTS5) and vector similarity search, merged via RRF, then re-ranked by a cross-encoder model. By default, returns code only; use `--kind document` for docs or `--kind all` for both.

Available `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `type-alias`, `trait`, `module`, `document`, `all`.

## Claude Plugin

cartog is available as a [Claude Code plugin](https://docs.anthropic.com/en/docs/claude-code) — the recommended way to install for Claude Code users. Plugins bundle skills, scripts, and MCP server configuration into a single installable package.

### Installation

Run these two commands **one at a time** in Claude Code:

1. Register the marketplace:

```bash
/plugin marketplace add jrollin/cartog
```

2. Install the plugin:

```bash
/plugin install cartog@cartog-plugins
```

This installs the plugin from GitHub, which includes:

- The agent skill (behavioral instructions in `skills/cartog/SKILL.md`)
- Setup and install scripts
- Plugin manifest (`.claude-plugin/plugin.json`)

### Plugin Structure

```text
.claude-plugin/
└── plugin.json          # Plugin manifest
skills/
└── cartog/
    ├── SKILL.md         # Agent skill instructions
    ├── scripts/         # install.sh, ensure_indexed.sh
    ├── tests/           # Behavioral evals
    └── references/      # Query cookbook, language support
```

## Agent Skill

cartog also ships as an [Agent Skill](https://agentskills.io) — behavioral instructions that teach your AI agent *when and how* to use cartog, including search routing, refactoring workflows, and fallback heuristics. Use this method for non-Claude Code environments or any LLM with bash access.

### Installation

```bash
npx skills add jrollin/cartog
```

Or install manually:

```bash
cp -r skills/cartog ~/.claude/skills/
```

The plugin wires two Claude Code hooks plus a user-typed skill:

- **SessionStart** (`ensure_indexed.sh`): non-blocking. If the binary is missing, forks a background pipeline (install.sh pinned to the plugin version → optionally `cartog index .` → `cartog rag setup` → `cartog rag index .`) and exits fast — MCP becomes available on the next session. If the binary is present, runs `cartog index .` foreground (incremental, typically <1s) and backgrounds rag setup + rag index. Prints a one-line drift warning if the installed binary doesn't match the plugin pin — or, when a deferred update is already armed for that pin, "cartog X will be applied when this session ends". Surfaces a "cartog updated to X" breadcrumb once after a completed boundary swap. Missing `.cartog.toml` on an interactive session prints a hint pointing at `cartog init`; on a non-interactive session it exits silently.
- **SessionEnd** (`update_on_exit.sh`): brings the binary in line with the plugin pin at the safe boundary, gated by what the binary can actually do (probed via `self update --help`, not assumed from the version). Deferred-capable binaries (>= 0.20.0) run `cartog self update --apply-pending` once the serve process has exited (no-op if nothing was armed; skipped while a RAG pipeline lock is active; exit 6 means a peer is still live and it retries next session). Any older binary — the 0.14.0–0.20.0 band (has `cartog self update` but lacks the deferred flags) and pre-0.14.0 (lacks it entirely) — upgrades via the bundled `install.sh` pinned to the plugin version; using a plain `cartog self update` for the 0.14–0.20 band would fetch the latest release and overshoot the pin. The pre-0.14.0 leg is transitional and scheduled for removal.
- **`/cartog-install` skill** (`skills/cartog-install/SKILL.md`): user-typed verb. Installs the binary (when missing) or upgrades it to match the plugin's pinned version. On >=0.14.0 it runs `cartog self update`; inside a Claude Code session, where the MCP server is the live peer (exit 6), it arms a deferred update with `cartog self update --defer` instead. On <0.14.0 it upgrades via `install.sh`. Use this to repair a failed background install or to bring a drifted binary back in sync.

To run the SessionStart steps manually:

```bash
bash skills/cartog/scripts/ensure_indexed.sh
```

### Skill Contents

| File | Purpose |
|------|---------|
| [`SKILL.md`](../skills/cartog/SKILL.md) | Behavioral instructions, commands, and workflows |
| [`scripts/install.sh`](../skills/cartog/scripts/install.sh) | Automated installation (pre-built binary or cargo install), accepts optional version arg |
| [`scripts/ensure_indexed.sh`](../skills/cartog/scripts/ensure_indexed.sh) | SessionStart hook: install-if-missing + foreground index + background rag setup/index + drift warning |
| [`scripts/update_on_exit.sh`](../skills/cartog/scripts/update_on_exit.sh) | SessionEnd hook: applies a deferred update (`cartog self update --apply-pending`) once the serve peer exits; legacy `install.sh` upgrade for <0.14.0 |
| [`tests/golden_examples.yaml`](../skills/cartog/tests/golden_examples.yaml) | Behavioral test scenarios (expected tool calls per query) |
| [`tests/test_ensure_indexed.sh`](../skills/cartog/tests/test_ensure_indexed.sh) | Bash unit tests for ensure_indexed.sh |
| [`tests/test_update_on_exit.sh`](../skills/cartog/tests/test_update_on_exit.sh) | Bash unit tests for update_on_exit.sh |
| [`tests/test_install.sh`](../skills/cartog/tests/test_install.sh) | Bash unit tests for install.sh |
| [`tests/eval.sh`](../skills/cartog/tests/eval.sh) | LLM-as-judge evaluation via `claude` CLI |
| [`references/query_cookbook.md`](../skills/cartog/references/query_cookbook.md) | Recipes for common navigation patterns |
| [`references/supported_languages.md`](../skills/cartog/references/supported_languages.md) | Language support matrix |

## MCP Server

`cartog serve` runs cartog as an MCP server over stdio, exposing 16 tools for MCP-compatible clients (Claude Code, Cursor, Windsurf, etc.). Each tool carries a human-readable `title` and a `readOnlyHint` annotation: 13 query tools are read-only (including `cartog_trace` for call paths and `cartog_context` for one-shot task bundles); `cartog_index` and `cartog_rag_index` write the index; and `cartog_update` arms a deferred self-update (`readOnlyHint = false` because it writes the machine-level state file, but it never touches the index). Clients can skip approval prompts for the read-only ones.

When `cartog serve --watch` is running and a file changes (or RAG embeddings are still catching up — including symbols whose body was just edited and not yet re-embedded), affected read-tool responses are prefixed with a `⚠️` staleness banner so the agent knows the answer may be momentarily behind the working tree. Read-only secondaries and `cartog serve` without `--watch` never show the banner.

Read tools also declare an `outputSchema` and return `structuredContent` (the typed result mirrored alongside the human-readable text block) so schema-aware clients get validated, machine-readable output. To keep responses within the caller's context window, the size cap (`CARTOG_MCP_MAX_BYTES`, default 64 KB) counts the text block plus the structured copy: `structuredContent` is dropped when the combined size would exceed the cap (and when the text block itself is truncated, which adds a truncation notice).

For editor-specific recipes (Neovim keymaps, VS Code tasks, Emacs `compile`, Telescope picker, `cartog watch --json` floating buffer), see **[Editor integration](editor-integration.md)**.

```bash
cartog serve                  # basic MCP server
cartog serve --watch          # auto-re-index + auto-embed when the repo already has embeddings
cartog serve --watch --rag    # force auto-embed even on a not-yet-embedded repo
```

Under `--watch`, embeddings auto-refresh on edits when the repo already has
embeddings (i.e. you have run `cartog rag index` at least once) — no `--rag`
needed. Repos that never used RAG pay nothing: no model loads. Force it on/off
with three controls, highest precedence first: `CARTOG_WATCH_RAG` (env) overrides
`[embedding] auto_embed` (config), which overrides `--rag` (flag). `--rag` without
`--watch` is a no-op (the watcher owns embedding) and warns.

### Per-editor wiring: `cartog ide`

The verb that actually writes MCP configs. Run it once per machine, plus any
time you install a new editor.

```bash
cartog ide                          # configure all installed clients
cartog ide --scope project          # only .mcp.json + .cursor/mcp.json
cartog ide --scope user             # only user-scope clients
cartog ide --client cursor          # one client
cartog ide --dry-run                # preview without writing
```

Supported clients (matches the per-client list below): `claude-code` (project
+ user), `claude-desktop`, `codex`, `cursor`, `gemini`, `opencode`, `vscode`
(project + user), `windsurf`, `zed`, `antigravity`, `kiro` (project + user),
`hermes`. User-scope clients whose config directory does not exist are skipped
(treated as "not installed").

Codex stores all MCP servers in a single user-global `~/.codex/config.toml`,
so cartog writes one per-project section named `cartog-<slug>-<hash8>` (slug
is your project directory name, hash8 is the first 4 bytes of SHA-256 of the
absolute path) to keep multiple projects coexisting. The TOML editor preserves
comments and ordering in the rest of the file.

#### Flag reference

| Flag | Effect |
|---|---|
| `--client <name>` | Target a single client (one of the names above). Default: all matching the scope. |
| `--scope project\|user\|all` | Limit to project-scoped files, user-scoped files, or both (default `all`). |
| `-y`, `--yes` | Skip interactive prompts. Also implied by `--dry-run`, `--client`, `--json`, or a non-TTY stdin. |
| `--dry-run` | Print the planned changes (before/after diff per file) without writing. Implies non-interactive. |
| `--no-watch` | Drop `--watch` from Claude Code's serve args. Other clients register plain `["serve"]` regardless. |
| `--json` (global) | Emit a structured `IdeReport` on stdout instead of human text. |

#### Troubleshooting

| Symptom | Meaning | Action |
|---|---|---|
| `not modified (config file is not valid JSON); …` | The existing file is JSONC (JSON with comments) or malformed. Cartog refuses to clobber it. | Open the file, fix the JSON manually using the snippets below. |
| `top-level <key> is a <kind> (expected object); refusing to overwrite` | The user file has the right path but the wrong shape at the container key. | Delete the offending key (or the whole file) and re-run. |
| `config directory not found (client likely not installed)` | The user-scope config parent dir doesn't exist on this machine. | Install the editor (or skip with `--client X` targeting only what you have). |
| `0 clients configured` | None of the targeted clients matched. | Check `--client` / `--scope` are compatible (e.g. `cursor` is project-only). |

For deeper diagnostics on the index, embeddings, and language servers, run `cartog doctor`.

### Manual setup (per client)

See **[mcp-setup.md](mcp-setup.md)** for the config-file recipes for every
supported client (Claude Code, Claude Desktop, Cursor, Windsurf, OpenCode,
Zed, Codex CLI, Gemini CLI, VS Code/Copilot, and the generic stdio pattern).

`cartog ide` writes those files for you; the manual page is for users who
want to edit by hand or audit what cartog wrote.

### Available Tools

| Tool | Parameters | Description |
|------|-----------|-------------|
| `cartog_index` | `path?`, `force?` | Build/update the code graph |
| `cartog_search` | `query`, `kind?`, `file?`, `limit?` | Find symbols by partial name |
| `cartog_outline` | `file` | File structure (symbols, line ranges) |
| `cartog_refs` | `name`, `kind?` | All references to a symbol |
| `cartog_callees` | `name` | What a symbol calls |
| `cartog_impact` | `name`, `depth?` | Transitive impact analysis |
| `cartog_trace` | `from`, `to`, `depth?` | Shortest call path between two symbols, bodies inline |
| `cartog_context` | `task`, `tokens?` | One-shot task bundle: relevant symbols + bodies |
| `cartog_hierarchy` | `name` | Inheritance tree |
| `cartog_deps` | `file` | File-level imports |
| `cartog_stats` | — | Index summary |
| `cartog_map` | `tokens?` | Token-budget-aware codebase summary (file tree + top symbols by centrality) |
| `cartog_changes` | `commits?`, `kind?` | Symbols affected by recent git changes |
| `cartog_rag_index` | `path?`, `force?` | Build embedding index for semantic search (write) |
| `cartog_rag_search` | `query`, `kind?`, `limit?` | Semantic search (FTS5 + vector + re-ranking) |
| `cartog_update` | `version?` | Arm a deferred self-update (write; touches the state file, not the index) |

Read tools (everything except `cartog_index`, `cartog_rag_index`, and `cartog_update`)
carry an `outputSchema` and return `structuredContent`. All tool responses also include a JSON text block.

**Path restriction**: `cartog_index` and `cartog_rag_index` reject paths outside the project directory (CWD subtree). Agents cannot index arbitrary filesystem locations.

**Progress notifications**: `cartog_index` and `cartog_rag_index` emit standard MCP `notifications/progress` when the client includes a `progressToken` in the request's `_meta`. `cartog_index` emits 3 phase events (`walking`, `parsing N files`, `storing N files`) plus an optional fourth (`resolving with LSP`) when the LSP pass runs. `cartog_rag_index` emits `preparing`, one `embedding processed/total` per ~512-symbol batch, then `storing` — so larger re-embed runs produce more events. The `message` field is human-readable, not a contract. Clients that do not supply a `progressToken` see no notifications and behavior is unchanged. Cold-cache or `force=true` runs report larger `total` values than warm runs; `total` is per-request, not historical.

**Cancellation**: `cartog_index` and `cartog_rag_index` honor MCP `notifications/cancelled`. The indexer aborts at the next phase boundary or per-file checkpoint (sub-second latency in typical cases), and the tool returns an error whose message contains `cancelled`. `cartog_index` runs inside a single rusqlite transaction that rolls back on the error path, so a cancelled run leaves no code-graph changes. `cartog_rag_index` keeps any embedding batches that were already flushed to SQLite; the in-flight batch is dropped. In both cases the next index run redoes the missing work.

### Built-in Workflow Guidance

The MCP server sends workflow instructions to the client at initialization, covering tool chaining order (index → search → refs/callees/impact → re-index) and when to use semantic search. Clients that support the MCP `instructions` field will surface these automatically.

### Logging

Logs go to stderr. The default level depends on how cartog is invoked:

| Invocation | Default level | Why |
|------------|---------------|-----|
| `cartog serve` / `cartog watch` / `cartog rag index`, stderr is a TTY | `info` | Foreground user wants progress |
| Same, stderr is captured (MCP child, piped CI) | `warn` | The parent reads stderr; info-level lines surfaced as `[ERROR]` in client debug logs |
| Other commands (one-shot CLI) | `warn` | Stay quiet by default |

Set `RUST_LOG` to override in either direction:

```bash
RUST_LOG=debug cartog serve   # per-request tool call logging
RUST_LOG=info  cartog serve   # force info under MCP-child mode
RUST_LOG=warn  cartog watch   # quieten down a foreground watcher
```

### Plugin vs MCP vs Skill

| | Claude Plugin | MCP Server | Agent Skill |
|-|--------------|-----------|-------------|
| Install | `/plugin marketplace add jrollin/cartog` then `/plugin install cartog@cartog-plugins` | `claude mcp add cartog -- cartog serve` | `npx skills add jrollin/cartog` |
| Context cost | ~150 lines of prompt | Zero (tools are protocol-level) | ~150 lines of prompt |
| Workflow guidance | Full heuristics | Basic (via `instructions` field) | Full heuristics |
| Compatibility | Claude Code only | MCP clients only | Any LLM with bash |
| Latency | Fork+exec per command | Persistent process | Fork+exec per command |

Use the **plugin** for Claude Code (simplest setup, includes skill + scripts + agents). Use **MCP** when you want lower token cost with an MCP-compatible client. Use the **skill** for non-Claude Code environments.

## Agents

cartog ships autonomous agents that execute multi-step workflows end-to-end. Agents are bundled in the plugin and available after plugin installation.

### Available Agents

| Agent | Description | Invocation |
|-------|-------------|------------|
| `codebase-onboarding` | Structured onboarding report — adapts to project type and size | `@codebase-onboarding` or "help me understand this project" |
| `refactoring-scout` | Pre-flight blast radius analysis before a refactoring | `@refactoring-scout` or "is it safe to change X?" |

### How Agents Work

Agents differ from the cartog skill:

- **Skill** (reactive): Claude uses cartog commands in response to your questions — you drive the workflow
- **Agent** (autonomous): you give a goal, the agent executes a multi-step plan using cartog, and produces a deliverable

Agents use the CLI via Bash (not MCP), so they work as subagents with isolated context. They are self-contained — no skill injection overhead.

### Manual Installation

If not using the plugin, copy agent definitions to your Claude Code agents directory:

```bash
cp agents/*.md ~/.claude/agents/
```

### Agent: `codebase-onboarding`

Produces a structured onboarding report for an unfamiliar codebase. The agent adapts to the project — a CLI tool gets different treatment than a web API or a library.

**Workflow:**
1. **Discover** — `cartog stats` + `cartog map` + manifest/README to determine project type and scale
2. **Architecture** — trace top-centrality symbols with `callees`/`refs` to map module layout
3. **Entry points** — targeted searches based on project type (CLI commands, API routes, public surface, etc.)
4. **Conventions** — test patterns, code style config, recent git activity

Output: a structured markdown report. Sections that don't apply are omitted.

**Usage:**
```text
@codebase-onboarding
# or
"Use the codebase-onboarding agent to analyze this project"
# or start a session as the agent
claude --agent codebase-onboarding
```

### Agent: `refactoring-scout`

Pre-flight analysis before changing a symbol, module, or file. Maps the full blast radius and produces a go/no-go recommendation.

**Workflow:**
1. **Locate** — confirm exact symbol with `cartog search`, disambiguate if needed
2. **Map blast radius** — `refs` + `impact --depth 3` + `callees` + `hierarchy` (for classes)
3. **Assess risk** — Low / Medium / High based on affected file count and transitive depth
4. **Report** — affected files, risk warnings, and a concrete update checklist

**Usage:**
```text
@refactoring-scout "rename TrackerRepository"
# or
"Is it safe to delete the UtilsHelper class?"
# or start a session as the agent
claude --agent refactoring-scout
```

## Commands

### `cartog index <path>`

Build or update the graph. Run this first, then again after code changes.

```bash
cartog index .              # index current directory
cartog index src/           # index a subdirectory only
cartog index . --force      # full re-index, bypassing change detection
```

Incremental by default — skips unchanged files (git diff + SHA-256), and within changed files, uses Merkle-tree diffing to update only modified symbols. Stable symbol IDs (`file:kind:qualified_name`) survive line movements, so edges from unchanged files remain valid. The LSP pass skips edges already classified as `resolution_state = 2` (unresolvable: typo, dyn dispatch, macro) or `3` (external: stdlib, deps, node_modules); both auto-retry when a matching symbol is added in-tree. Use `--force` when results seem stale or after updating cartog itself — it also resets state-2 and state-3 markers for a clean retry.

Files whose language cartog doesn't support are reported, not silently dropped: on a mixed repo the summary adds a line like `12 files in unsupported languages not indexed (8 .kt, 4 .cpp)`. The `--json` output carries the same data as `files_unsupported` (count) and `unsupported_by_ext` (`[ext, count]`, descending). cartog's own database sidecars (`.cartog.db*`, `db.sqlite*`) are excluded from the tally.

### `cartog search <query> [--kind <kind>] [--file <path>] [--limit N]`

Find symbols by partial name — use this when you know roughly what you're looking for but need the exact name before calling `refs`, `callees`, or `impact`.

```bash
cartog search validate                       # prefix + substring match
cartog search validate --kind function       # functions only
cartog search config --file src/db.rs        # scoped to one file
cartog search parse --limit 5               # cap results
```

```text
function  validate_token    auth/tokens.py:30
function  validate_session  auth/tokens.py:68
function  validate_user     services/user.py:12
```

Results ranked: exact match → prefix → substring. Case-insensitive. Max 100 results.

Available `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `type-alias`, `trait`, `module`, `document`.

### `cartog outline <file>`

Show all symbols in a file with their types, signatures, and line ranges. Use this instead of reading a file when you need structure.

```bash
cartog outline src/db.rs
```

```text
use anyhow  L1
use rusqlite  L2
class Database  L62-500
  method open(path: &str) -> Result<Self>  L64-72
  method insert_symbol(&self, sym: &Symbol) -> Result<()>  L130-148
  ...
```

### `cartog callees <name>`

Find what a function calls — answers "what does this depend on?".

```bash
cartog callees validate_token
```

```text
lookup_session  auth/tokens.py:37
TokenError      auth/tokens.py:39
ExpiredTokenError  auth/tokens.py:42
```

### `cartog impact <name> [--depth N]`

Transitive impact analysis — follows the caller chain up to N hops (default 3). Answers "what breaks if I change this?".

```bash
cartog impact validate_token --depth 3
```

```text
  calls  get_current_user  auth/service.py:40
  calls  refresh_token  auth/tokens.py:54
    calls  impersonate  auth/service.py:52
```

Indentation shows depth.

### `cartog refs <name> [--kind <kind>]`

All references to a symbol (calls, imports, inherits, type references, raises). Optionally filter by edge kind.

```bash
cartog refs UserService                  # all reference types
cartog refs validate_token --kind calls  # only call sites
```

```text
imports  ./service  routes/auth.py:3
calls    login  routes/auth.py:15
inherits AdminService  auth/service.py:47
references  process  routes/auth.py:22
```

Available `--kind` values: `calls`, `imports`, `inherits`, `references`, `raises`, `implements`, `type-of`.

### `cartog hierarchy <class> [--mermaid]`

Show inheritance relationships involving a class — both parents and children.
Add `--mermaid` for a `graph TD` diagram you can paste into a PR or doc.

```bash
cartog hierarchy AuthService
cartog hierarchy AuthService --mermaid
```

```text
AuthService -> BaseService
AdminService -> AuthService
```

With `--mermaid`:

```text
graph TD
    AuthService["AuthService"] --> BaseService["BaseService"]
    AdminService["AdminService"] --> AuthService["AuthService"]
```

### `cartog deps <file> [--mermaid]`

List symbols imported by a file — answers "what does this file depend on?".
Add `--mermaid` for a `graph LR` diagram rooted at the file.

```bash
cartog deps auth/service.py
cartog deps auth/service.py --mermaid
```

```text
validate_token  L5
generate_token  L5
User            L6
```

With `--mermaid`:

```text
graph LR
    auth_service_py["auth/service.py"]
    auth_service_py --> validate_token["validate_token (L5)"]
    auth_service_py --> generate_token["generate_token (L5)"]
    auth_service_py --> User["User (L6)"]
```

### `cartog stats [--savings]`

Summary of the index — file count, symbol count, edge resolution rate. Pass
`--savings` to instead show per-tool query counts and estimated tokens saved
versus a grep+read baseline (or use the [`cartog savings`](#cartog-savings)
top-level alias).

```bash
cartog stats
cartog stats --savings
```

```text
Files:    42
Symbols:  387
Edges:    1204 (891 resolved)
Languages:
  python: 30 files
  typescript: 12 files
Symbols by kind:
  function: 142
  method: 98
  class: 45
  import: 62
  variable: 40
```

On an unindexed repo all counts are `0` and the output ends with
`Index is empty — run \`cartog index .\` to build the code graph.`

### `cartog savings`

Per-tool query counts + an estimated tokens-saved figure versus a grep+read
baseline (~1,420 tokens/query, drawn from the benchmark suite). Visible alias
of `cartog stats --savings`; shipped as a top-level verb because it's the
retention hook — surfaces ongoing ROI in one keystroke.

```bash
cartog savings
cartog --json savings
```

```text
cartog · my-project · 5 queries

████████░░  ~83% tokens saved

Without cartog    ~8.5k tokens   (~1700 / query)
With cartog       ~1.4k tokens   (~280 / query)
──────────────────────────────────────────────
Saved             ~7.1k tokens   (~1420 / query)

By tool (call counts):
     2  search
     1  impact
     1  map
     1  refs

Baseline: ~1700 tokens for an equivalent grep+read sweep vs cartog's ~280.
Measured across 13 benchmark scenarios (see benchmarks/scenarios/).
```

The header line is `cartog · <project> · <N> queries`, where `<project>` is
the directory holding `.cartog/`. **By tool** lists *call counts*, not per-tool
token savings — every tool uses the same baseline (1,420 tokens saved per
call), so the breakdown shows which navigation patterns the user actually
relies on, not which one saved the most.

Only queries that returned a non-empty result count toward the totals.
Empty-index calls, typo'd symbol names, and "no such file" outline calls are
skipped so the figure reflects real work, not zero-value pings.

Data comes from a local `query_log` table inside `.cartog/db.sqlite`. Nothing
leaves the machine; no query payloads are recorded — only the tool name, call
surface (`cli` / `mcp`), and a timestamp. Secondary read-only MCP attaches
skip the write (they can't write at all), so multi-MCP-server setups *under-*
report secondary traffic rather than double-counting it.

**JSON schema** (`cartog --json savings`): v0.18+ adds `tokens_used_cartog`,
`tokens_used_grep`, and `percent_saved` to the existing
`{by_tool, by_source, total_queries, estimated_tokens_saved, baseline_delta}`.
Consumers must not parse with `deny_unknown_fields` — additive fields are
expected as the metric evolves.

### `cartog push [--remote <s3-url>]`

Upload the local index DB to S3-compatible storage (AWS S3, MinIO, Cloudflare R2,
floci). Built in by default (`remote-s3` feature, on); cartog still runs 100%
local until you configure `[remote]` or pass `--remote`.

```bash
cartog push                                       # uses [remote].url
cartog push --remote s3://team-bucket/main.sqlite # explicit override
```

What it does, in order:

1. Refuses to push while `cartog serve` or `cartog watch` is using the DB.
2. Runs `PRAGMA wal_checkpoint(TRUNCATE)` so the file is self-contained.
3. Streams a SHA-256 hash of the DB.
4. Uploads via multipart with object metadata: `x-amz-meta-sha256`,
   `x-amz-meta-schema-version`, `x-amz-meta-cartog-version`, and
   `x-amz-meta-git-commit` (the commit the index was built at; omitted when
   the index has no git provenance). The `--json` output adds a `git_commit`
   field (`null` when absent) so a puller can decide whether the remote index
   matches its checkout.

Credentials come from the AWS environment chain (env vars, `~/.aws/credentials`,
IMDS) — **never from `.cartog.toml`**. Storing a credential-shaped key
(`access_key`, `secret_key`, `aws_*`, etc.) in `[remote]` fails at config-load
time with a security error.

### `cartog pull [--remote <s3-url>] [--force] [--no-sign-request]`

Download a prebuilt index from S3-compatible storage. Useful for CI warm-start
and for sharing a team-wide index instead of every dev rebuilding from zero.

```bash
cartog pull                              # uses [remote].url
cartog pull --remote s3://b/k.sqlite     # explicit override
cartog pull --force                      # overwrite even while peer is using the DB
cartog pull --no-sign-request            # anonymous (public-bucket) pull
```

Safety guarantees:

- **Atomic install** — the file is downloaded to `<db>.partial`, verified,
  then renamed; a mid-pull crash or network failure never leaves a torn DB.
- **Checksum required** — refuses to install if the remote object has no
  `x-amz-meta-sha256` metadata. Same for `x-amz-meta-schema-version`.
- **Non-cartog files refused** — pulling a SQLite file that lacks cartog's
  schema (e.g. an unrelated app's DB) is refused even when its sha256
  matches; cartog cross-checks the `schema_version` row against the header.
- **Commit provenance (report-only)** — pull prints the commit the index was
  built at (`commit=<short>`, also `git_commit` in `--json`) and, when both
  the `x-amz-meta-git-commit` header and the file's `last_commit` row are
  present, refuses to install if they disagree. Pull never blocks on
  staleness: the install always proceeds, and the caller (CI script, agent)
  decides whether the reported commit is fresh enough.
- **Schema-version guard** — refuses to install a DB produced by a newer
  cartog, naming both the pulled and supported versions.
- **WAL/SHM cleanup** — stale `db-wal` / `db-shm` siblings are deleted
  before rename to prevent SQLite from replaying phantom WAL frames.
- **Peer-process check** — best-effort refusal to overwrite the local DB
  while a `cartog serve` or `cartog watch` is holding it open. cartog
  checks for peer PID locks twice (at the start of pull and right before
  the atomic rename), but a peer that wins the lock election in the few
  syscalls between the second check and the rename can still be corrupted
  by the swap (SQLite holds the file by inode; the rename divorces its
  FD from on-disk state). The window is small but non-zero. `--force`
  bypasses both checks. **Safest practice: stop `cartog serve` /
  `cartog watch` on the project before pulling, and restart them after.**

> **Trust boundary**: the `x-amz-meta-sha256` header is self-attested by
> whoever pushed the object — it catches corruption and accidental swaps
> but not a deliberate malicious push by someone with write access to the
> bucket. Treat the bucket like a shared filesystem under the same
> trust assumptions as your team's git remote.

### Configuring `[remote]`

In `.cartog.toml`:

```toml
[remote]
url        = "s3://team-bucket/cartog/main.sqlite"
region     = "us-east-1"
endpoint   = "https://minio.example.com"   # only for MinIO / R2 / floci
path_style = true                          # required for most non-AWS endpoints
```

Only those four keys are accepted. Credential-shaped keys (`access_key`,
`secret_key`, `aws_*`, `token`, `password`, …) are rejected at parse time —
configure credentials via the AWS environment chain instead.

### Minimal build (drop default features)

Users who want the smallest possible binary, or who run in fully air-gapped
environments, can drop the default features and add back only what they need:

```bash
cargo install cartog --no-default-features                 # drops LSP, S3, Ollama, OpenAI
cargo install cartog --no-default-features --features lsp   # keep LSP only (still drops OpenAI)
```

A binary built without `remote-s3` refuses `cartog push` and `cartog pull` with
a clear error pointing at the reinstall command.

### `cartog map [--tokens N] [--mermaid]`

Token-budget-aware codebase summary — file tree + top symbols ranked by reference count (in-degree centrality). Add `--mermaid` for a `graph TD` rooted at "Repo"; the token budget still applies (the renderer stops adding nodes before it overflows).

```bash
cartog map                    # default 4000 tokens
cartog map --tokens 2000      # compact summary
cartog map --tokens 8000      # detailed summary
cartog map --mermaid          # paste-into-PR diagram
```

```text
# Codebase Map (42 files)

  src/auth/tokens.py
  src/auth/service.py
  ...

# Top Symbols (by reference count)

src/auth/tokens.py:
  function validate_token(token: str) -> bool  L30-45  (15 refs)
  function generate_token(user_id: int) -> str  L10-28  (9 refs)
src/auth/service.py:
  class AuthService  L8-95  (12 refs)
  ...
```

Phase 1 shows the file tree; phase 2 fills remaining budget with symbols ordered by centrality (most-referenced first). Use `--json` for structured output.

With `--mermaid`:

```text
graph TD
    repo["Repo"]
    repo --> auth_service_py["auth/service.py"]
    repo --> auth_tokens_py["auth/tokens.py"]
    auth_tokens_py --> auth_tokens_py__validate_token["validate_token (function)"]
    auth_tokens_py --> auth_tokens_py__generate_token["generate_token (function)"]
    ...
```

`--json` wins over `--mermaid` when both are set.

### `cartog changes [--commits N] [--kind <kind>]`

Show symbols affected by recent git changes — answers "what code changed recently?".

```bash
cartog changes                        # last 5 commits + working tree
cartog changes --commits 10           # last 10 commits
cartog changes --kind function        # only functions that changed
```

```text
27 files changed in last 5 commits, 158 symbols affected

src/commands.rs:
  function open_db() -> Result<Database>  L14-16
  function cmd_index(path: &str, force: bool, json: bool) -> Result<()>  L62-75
  ...

2 changed files not in index:
  Cargo.lock
  .gitignore
```

Symbols are grouped by file. Files changed but not indexed (e.g., lock files, config) are listed separately. Markdown files (`.md`) are now indexed as document sections.

### `cartog doctor`

Check that all requirements are met and everything is working. Validates the environment based on the current configuration.

```bash
cartog doctor
```

```text
  [+] git: git repository at /home/user/project
  [+] config: loaded from /home/user/project/.cartog.toml
  [+] database: 42 files, 387 symbols at /home/user/project/.cartog/db.sqlite
  [+] embedding: local model cached
  [+] reranker: jinaai/jina-reranker-v1-turbo-en cached

All 5 checks passed
```

**Checks performed:**

| Check | OK | Warn | Error |
|-------|-----|------|-------|
| git | Inside a git repo | — | Not a git repo |
| config | `.cartog.toml` found and parsed | No config file (using defaults) | — |
| database | DB exists with indexed data | DB empty or missing | DB cannot be opened |
| embedding | Local model cached / Ollama reachable | Local model not downloaded | Ollama unreachable / unknown provider |
| reranker | Model cached / disabled | Model not downloaded | Unknown provider |

Exits with code 1 if any check is an error. Supports `--json` for structured output.

### `cartog watch [path] [--debounce N] [--rag] [--rag-delay N] [--json]`

Watch for file changes and auto-re-index. Keeps the code graph fresh during development.

```bash
cartog watch                          # watch CWD; auto-embeds if the repo already has embeddings
cartog watch src/                     # watch subdirectory
cartog watch --rag                    # force auto-embed (even on a not-yet-embedded repo)
cartog watch --rag --rag-delay 60     # embed after 60s of inactivity
cartog watch --debounce 10            # 10s debounce window
cartog watch --json                   # NDJSON event stream on stdout
```

The watcher runs an initial incremental index on startup, then re-indexes when supported source files change. Changes are debounced (default 5s) to avoid re-indexing on every keystroke and to absorb bulk file changes (e.g. `git pull`).

Auto-embed is on when the repo already has embeddings (auto-detected from the DB), forced by `--rag`, or set via `[embedding] auto_embed` / `CARTOG_WATCH_RAG`; otherwise off. When on, embedding is deferred until `--rag-delay` seconds (default 30) have elapsed without new file changes, batching all pending symbols in one pass. A symbol whose body is edited has its stale embedding invalidated on re-index, so it is re-embedded on the next pass.

With `--json`, every lifecycle event is emitted as one NDJSON record on stdout (`started`, `reindex`, `reindex_failed`, `rag_embedded`, `rag_failed`, `shutdown`). Human-readable tracing still goes to stderr.

Press Ctrl+C to stop. Pending RAG embeddings are flushed before exit.

### `cartog serve [--watch] [--rag]`

Start cartog as an MCP server over stdio. See the [MCP Server](#mcp-server) section below for client configuration.

```bash
cartog serve                  # MCP server only
cartog serve --watch          # MCP server + watcher; auto-embeds if the repo has embeddings
cartog serve --watch --rag    # force auto-embed even on a not-yet-embedded repo
```

When `--watch` is passed, a background file watcher keeps the code graph up to date as you edit, and (when the repo already has embeddings) refreshes RAG embeddings on a deferred timer — no `--rag` needed. The MCP server and watcher share the same SQLite database via WAL mode (concurrent readers are safe).

#### Multiple `cartog serve` instances on the same project

Opening two Claude Code windows on the same project (or running `cartog serve` in a terminal while a Claude Code window has its own MCP child) is supported via **single-writer election**:

- The first instance acquires `<state_dir>/serve-<hash>.pid` atomically (O_EXCL) and runs as **primary** — owns the file watcher, exposes all 16 MCP tools. The `<hash>` is a 16-char SHA-256 prefix of the canonical DB path, so two cartog peers on different projects coexist without colliding on the same slot.
- Subsequent instances see the held lock, attach **read-only** (no migrations), and expose 14 of 16 tools. The two indexing tools (`cartog_index`, `cartog_rag_index`) return a clear error pointing at the primary; queries (`cartog_search`, `cartog_rag_search`, etc.) and `cartog_update` (which arms a machine-level deferred update, not a DB write) work normally. `cartog_stats` includes `"role": "read-only"` so you can tell which is which.
- If the primary process dies (Cmd-Q, `kill`, crash), the secondary's background promoter detects this within ~10s, validates the on-disk schema hasn't drifted, atomically acquires the lock, and takes over without restart. All 16 tools become available on what was the secondary.

Escape hatches:

| Env var | Effect |
|---|---|
| `CARTOG_SINGLE_WRITER=0` | Disables election. Every `cartog serve` opens RW; the migration busy-retry is the only defense against the rare race window. |
| `RUST_LOG=info` | Restores `info`-level tracing in MCP-child mode (defaults to `warn` when stderr is not a TTY so info lines don't surface as `[ERROR]` in the parent's log). |

`<state_dir>` resolves to the platform's standard state directory (via the `directories` crate):

| OS | Path |
|----|------|
| Linux | `$XDG_STATE_HOME/cartog/` (default `~/.local/state/cartog/`) |
| macOS | `~/Library/Application Support/io.cartog.cartog/` |
| Windows | `%LOCALAPPDATA%\cartog\cartog\data\` |

The directory holds `state.toml` (used by `cartog self update`) and PID lock files for live long-lived commands. Slots are scoped per DB so peers in different projects coexist:

- `serve-<hash>.pid` — `cartog serve` (the MCP server)
- `watch-<hash>.pid` — `cartog watch` or the `serve --watch` background watcher

`<hash>` is a 16-char SHA-256 prefix of the canonical DB path. On every `cartog serve` / `cartog watch` startup, stale PID files (whose recorded process has exited) are reaped automatically — no manual cleanup needed.

### `cartog init [--dry-run]`

Scaffold a `.cartog.toml` template in the current project. That's all it does. The next-steps hint points at `cartog ide` (MCP wiring) and `cartog index` (build the graph).

```bash
cartog init                  # scaffold .cartog.toml
cartog init --dry-run        # preview without writing
```

`cartog init` never overwrites an existing `.cartog.toml`. Re-running is a no-op (still prints the next-steps hint, useful when you've forgotten the next verb).

### `cartog ide [--client <name>] [--scope project|user|all] [-y] [--dry-run] [--no-watch]`

Wire `cartog serve` into one or all MCP-compatible editors. This is the only verb that touches editor configs.

```bash
cartog ide                          # all installed clients, all scopes (interactive picker)
cartog ide --client cursor          # one client
cartog ide --scope project          # only project-scoped (.mcp.json, .cursor/, .vscode/)
cartog ide --scope user             # only user-scope clients
cartog ide --dry-run                # preview with before/after diff
```

Example output (`cartog ide --client cursor --dry-run`):

```text
+ cursor (project, /your/repo/.cursor/mcp.json): would create
  --- after ---
    {
      "mcpServers": {
        "cartog": { "command": "cartog", "args": ["serve"] }
      }
    }

1 clients: 1 created, 0 updated, 0 unchanged, 0 skipped, 0 errors
Dry run only. Re-run without --dry-run to apply.
```

For `cartog ide --client claude-code --dry-run`, both project (`.mcp.json`) and user (`~/.claude/settings.json`) entries are previewed, and `args` includes `--watch` by default ([see why](#claude-code-watch-default)).

Supported clients: `claude-code` (project + user), `claude-desktop`, `codex`, `cursor`, `gemini`, `opencode`, `vscode` (project + user), `windsurf`, `zed`, `antigravity`, `kiro` (project + user), `hermes`. User-scope clients whose config dir is missing are skipped. See [Per-editor wiring: `cartog ide`](#per-editor-wiring-cartog-ide) for the flag and troubleshooting tables.

### `cartog install [client ...] [--scope ...] [--dry-run] [--no-watch]`

Friendlier shape of `cartog ide` — takes editors as positional args
(brew/npm/pip/cargo convention) and is always non-interactive. Safe to call
from scripts and agents (no picker, no `--yes` required).

```bash
cartog install cursor                 # one editor
cartog install cursor vscode codex    # several editors at once
cartog install                        # all detected editors
cartog install cursor --dry-run       # preview without writing
cartog install claude-code --no-watch # wire Claude Code without --watch
```

Example output (`cartog install cursor vscode --dry-run`):

```text
+ cursor (project, /your/repo/.cursor/mcp.json): would create
  --- after ---
    {
      "mcpServers": {
        "cartog": { "command": "cartog", "args": ["serve"] }
      }
    }
+ vscode (project, /your/repo/.vscode/mcp.json): would create
  --- after ---
    {
      "servers": {
        "cartog": { "type": "stdio", "command": "cartog", "args": ["serve"] }
      }
    }

2 clients: 2 created, 0 updated, 0 unchanged, 0 skipped, 0 errors
Dry run only. Re-run without --dry-run to apply.
```

Same supported-client list as `cartog ide`. For the interactive multi-select
picker (useful at a fresh-machine setup), use `cartog ide` directly.

### `cartog config`

Print the resolved configuration (merged defaults, `.cartog.toml`, and env overrides).

```bash
cartog config            # human-readable
cartog config --json     # JSON for scripts
```

Useful for verifying `[rag]` tuning, watch debounce, and provider selection without running a full command.

### `cartog completions <shell>`

Generate shell completion scripts for `bash`, `zsh`, `fish`, `powershell`, or `elvish`.

```bash
cartog completions zsh   > ~/.zfunc/_cartog
cartog completions bash  > /usr/local/etc/bash_completion.d/cartog
cartog completions fish  > ~/.config/fish/completions/cartog.fish
```

### `cartog manpage`

Emit a `roff` man page on stdout. Intended for packagers and distro maintainers.

The output filename **must** end in `.1` (the section-1 extension `man` looks
for) and the target directory must exist:

```bash
sudo mkdir -p /usr/local/share/man/man1
cartog manpage | sudo tee /usr/local/share/man/man1/cartog.1 > /dev/null
man cartog
```

On macOS the path is the same. On Linux distros some packagers prefer
`/usr/share/man/man1/`; either works as long as it is on `MANPATH`.

### `cartog self <update|version|rollback|migrate-db>`

Manage the installed cartog binary in place: upgrade, inspect, roll back, or migrate the on-disk DB layout.

```bash
cartog self update             # upgrade to the latest stable
cartog self update --check     # report whether an update exists; exit 1 if outdated
cartog self version            # version + target + install source + last check
cartog self rollback           # restore the previous binary saved at <bin>.old
cartog self migrate-db         # move legacy .cartog.db (+ -wal/-shm/.bak) into .cartog/
cartog self migrate-db --dry-run  # preview the planned moves without touching the filesystem
```

`cartog self update` refuses to overwrite a `cargo install cartog` binary (exit 3) and points at `cargo install cartog --force` instead. See [updates.md](updates.md) for the full exit-code matrix, env vars (`CARTOG_NO_UPDATE_CHECK`, `CARTOG_UPDATE_CHECK`), platform-specific state file location, and rollback contract.

`cartog self migrate-db` refuses to overwrite an existing `.cartog/db.sqlite`, refuses to run while a peer cartog process (`serve` / `watch`) holds the lock **for this project's database** (a peer serving an unrelated project does not block it), and refuses to migrate a symlinked `.cartog.db`.

## Recommended Workflow

```text
cartog index .          # 1. build the graph
cartog search foo       # 2. discover exact symbol names
cartog refs foo         # 3. find all usages
cartog callees foo      # 4. see what it depends on
cartog impact foo       # 5. assess blast radius before changing
cartog index .          # 6. re-index after code changes
```

For semantic search, add the RAG pipeline:

```text
cartog rag setup        # one-time model download (~230MB)
cartog rag index        # embed symbols
cartog rag search "..."  # natural language queries
```

## Token Budget

All query commands accept `--tokens N` to limit human-readable output to approximately N tokens. Useful for LLM agents with limited context windows.

```bash
cartog --tokens 500 search validate        # cap output to ~500 tokens
cartog --tokens 200 outline src/db.rs      # compact file outline
cartog --tokens 1000 changes --commits 10  # budget-aware recent changes
```

Uses a `len / 4` byte-to-token approximation. Output is truncated at a character boundary with a notice. Ignored when `--json` is used (agents handle their own truncation with `--limit`).

## JSON Output

All commands accept `--json` for structured output. The flag can go before or after the subcommand:

```bash
cartog --json refs validate_token
cartog refs validate_token --json    # equivalent
cartog --json outline src/auth/tokens.py
cartog --json stats
```

Returns arrays of objects with fields like `name`, `kind`, `file_path`, `start_line`, `end_line`, `signature`, etc. Empty results return `[]`.

**Errors**: if the index doesn't exist yet, query commands print an error message and exit with a non-zero status. Run `cartog index .` first. If a symbol or file isn't found, the result is an empty array (not an error).

