# cartog CLI Reference

> Configuration reference: [config.md](config.md) | MCP tools: [mcp-tools.md](mcp-tools.md)

## Commands

### `cartog index <path>`

Build or update the graph. Run this first, then again after code changes.

```bash
cartog index .              # index current directory
cartog index src/           # index a subdirectory only
cartog index . --force      # full re-index, bypassing change detection
cartog index . --jobs 4     # cap the parse worker pool at 4 threads
```

`--jobs N` caps the worker pool for the CPU-bound parse phase (default / `0` =
auto = CPU count, clamped `1..=64`; use `1` for single-threaded). It overrides
`CARTOG_JOBS` and `[index] jobs` (flag > env > TOML). The cap applies on every
index; for daemons (`serve`/`watch`, which take no flag) set `CARTOG_JOBS` or
`[index] jobs`.

Incremental by default — skips unchanged files (git diff + SHA-256), and within changed files, uses Merkle-tree diffing to update only modified symbols. Stable symbol IDs (`file:kind:qualified_name`) survive line movements, so edges from unchanged files remain valid. The LSP pass skips edges already classified as `resolution_state = 2` (unresolvable: typo, dynamic dispatch, or macro — retried when a matching symbol appears in-tree) or `3` (external: stdlib, dependency, or node_modules — also retried on new in-tree symbols). Use `--force` when results seem stale or after updating cartog itself — it resets state-2 and state-3 markers for a clean retry. See [../explanation/incremental-indexing.md](../explanation/incremental-indexing.md) for full details on change detection layers and edge states.

When a `cartog serve` for the same database is running (e.g. the MCP server of an active agent session), `cartog index` skips its own LSP pass — avoiding the language-server cold start (~20-40s for rust-analyzer) — and defers edge resolution to the serve peer's warm servers, which catch up on the server's next `cartog_index` call. In human mode the deferral prints a `note:` on stderr; under `--json`, stderr stays note-free and the result carries `lsp_deferred_to_peer: true` instead. The deferral never applies to a first-ever index. `--force` always runs the LSP pass locally.

Files whose language cartog doesn't support are reported, not silently dropped: on a mixed repo the summary adds a line like `12 files in unsupported languages not indexed (8 .scala, 4 .ex)`. The `--json` output carries the same data as `files_unsupported` (count) and `unsupported_by_ext` (`[ext, count]`, descending). cartog's own database sidecars (`.cartog.db*`, `db.sqlite*`) are excluded from the tally.

On a TTY a spinner shows the live phase with a climbing counter (`parsing M/N files`, `storing M/N files`, `resolving M/N edges with LSP`). Press **Ctrl-C** to cancel: indexing stops at the next file/edge-window boundary (including mid-LSP, the slowest phase), the whole pass rolls back so the index is left unchanged, and cartog prints `Indexing cancelled; the index was left unchanged.` Because the pass is one transaction, a re-run redoes it from scratch (it does not resume mid-pass). One caveat: a Ctrl-C while a language server is still starting up is only noticed once startup finishes (up to `CARTOG_LSP_READY_TIMEOUT_SECS`, default 20s).

**Consent gate.** On a project with no `.cartog.toml` and no existing index, `cartog index` (and `cartog rag index`, `cartog watch`) refuse rather than create a `.cartog/` for a project you haven't opted into. Run `cartog init` first (then index), or set `CARTOG_AUTO_INIT=1` to index with defaults without writing a config file. An existing index, or any present `.cartog.toml`, also grants consent. See [config.md § Index-creation consent gate](config.md#index-creation-consent-gate).

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

Available `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `enum-member`, `type-alias`, `trait`, `module`, `document`, `macro`.

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
of `cartog stats --savings`, promoted to a top-level verb so day-to-day
savings are one keystroke away.

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

Upload the local index DB to S3-compatible storage. See [../how-to/set-up-s3-sync.md](../how-to/set-up-s3-sync.md) for the full guide.

```bash
cartog push                                       # uses [remote].url
cartog push --remote s3://team-bucket/main.sqlite # explicit override
```

### `cartog pull [--remote <s3-url>] [--force] [--no-sign-request]`

Download a prebuilt index from S3-compatible storage. See [../how-to/set-up-s3-sync.md](../how-to/set-up-s3-sync.md) for the full guide.

```bash
cartog pull                              # uses [remote].url
cartog pull --remote s3://b/k.sqlite     # explicit override
cartog pull --force                      # overwrite even while peer is using the DB
cartog pull --no-sign-request            # anonymous (public-bucket) pull
```

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

Start cartog as an MCP server over stdio. See [mcp-tools.md](mcp-tools.md) for client configuration and tool reference.

```bash
cartog serve                  # MCP server only
cartog serve --watch          # MCP server + watcher; auto-embeds if the repo has embeddings
cartog serve --watch --rag    # force auto-embed even on a not-yet-embedded repo
```

When `--watch` is passed, a background file watcher keeps the code graph up to date as you edit, and (when the repo already has embeddings) refreshes RAG embeddings on a deferred timer — no `--rag` needed. The MCP server and watcher share the same SQLite database via WAL mode (concurrent readers are safe).

**Degraded start (consent gate).** Unlike the one-shot creators, `cartog serve` never refuses — on a project with no `.cartog.toml` and no index, it starts **degraded**: it creates no `.cartog/`, read tools return empty results, the 2 write tools refuse, and `cartog_stats` reports `"degraded": true` with a "run `cartog init`" banner. With `--watch`, the watcher pre-builds the index the moment a `.cartog.toml` appears (or an existing DB / `CARTOG_AUTO_INIT` is detected); the **running** server stays degraded until the client relaunches it, at which point the index is already built. See [config.md § Index-creation consent gate](config.md#index-creation-consent-gate).

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

For `cartog ide --client claude-code --dry-run`, both project (`.mcp.json`) and user (`~/.claude/settings.json`) entries are previewed, and `args` includes `--watch` by default.

Supported clients: `claude-code` (project + user), `claude-desktop`, `codex`, `cursor`, `gemini`, `opencode`, `vscode` (project + user), `windsurf`, `zed`, `antigravity`, `kiro` (project + user), `hermes`. User-scope clients whose config dir is missing are skipped.

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
cartog self version            # version + describe build string + target + install source + last check
cartog self rollback           # restore the previous binary saved at <bin>.old
cartog self migrate-db         # move legacy .cartog.db (+ -wal/-shm/.bak) into .cartog/
cartog self migrate-db --dry-run  # preview the planned moves without touching the filesystem
```

`cartog self update` refuses to overwrite a `cargo install cartog` binary (exit 3) and points at `cargo install cartog --force` instead. See [../how-to/update-cartog.md](../how-to/update-cartog.md) for the full procedural guide, and [exit-codes.md](exit-codes.md) for the exit-code matrix.

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

### Compact JSON (`--compact`)

`--compact` strips heavy, low-value fields from `--json` output to save agent tokens, while keeping every field needed to locate and rank a result. It is a no-op without `--json`.

```bash
cartog --json --compact rag search "authentication"   # ~60% smaller payload
```

Dropped in compact mode: symbol bodies (`content` in `rag search`/`trace`), `docstring`, and the `content_hash`/`subtree_hash` cache fields. Kept: `id`, `name`, `kind`, `file_path`, line/byte spans, `signature`, scores, `sources`, and edge `provenance`. `cartog context` is the exception — it keeps its inline bodies (already budgeted by `--tokens`), trimming only the per-entry symbol noise. The output stays valid JSON of the same shape (omitting optional fields), so it validates against the same schema as full output.

MCP tools are **compact by default** (agents are the consumer and the response cap already implies token pressure); set `CARTOG_MCP_COMPACT=0` to restore full bodies. There, `cartog_rag_search` and `cartog_trace` bound bodies to a ~500-byte snippet rather than dropping them, matching their "snippet excerpt" contract.

**Errors**: if the index doesn't exist yet, query commands print an error message and exit with a non-zero status. Run `cartog index .` first. If a symbol or file isn't found, the result is an empty array (not an error).
