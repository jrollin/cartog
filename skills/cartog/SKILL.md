---
name: cartog
description: >-
  Code graph navigation, semantic code search, and impact analysis. Use when the user asks
  "where is X defined?", "what calls X?", "who imports X?", "what depends on X?",
  "how is X used?", "where is X called from?",
  "what breaks if I change X?", "is it safe to change/delete X?",
  "help me refactor X", "show me the call graph", "trace the execution flow",
  "find all usages of X", "find code related to...",
  "show me how X works", "show me the X implementation",
  "show file structure", "list all functions/classes in a file",
  "show the inheritance tree", "show class hierarchy",
  or needs to navigate code, locate definitions, search code by concept or behavior,
  trace dependencies, assess blast radius of changes, explore how a feature is implemented,
  support refactoring (rename, extract, move, delete), or explore an unfamiliar codebase.
  Supports Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, C, C++, C#, PHP, Dart, Swift, Kotlin, Vue, Svelte, Astro, and Markdown documents, plus React/JSX and Vue/Svelte/Astro component-usage edges.
---

# cartog — Code Graph Navigation Skill

## Quick Start

Three states a repo can be in. Detect which one the user is in, then act.

| State | Signal | What the user should run |
|---|---|---|
| Fresh repo, no cartog yet | No `.cartog.toml` **and** no `.cartog/db.sqlite` at the git root | Ask the user first, then `cartog init` then `cartog index` (see "Fresh-repo handling" below) |
| Indexed but no editor MCP | `.cartog.toml` or `.cartog/db.sqlite` exists, no `.mcp.json` / `.cursor/mcp.json` / `.vscode/mcp.json` | `cartog index` (refresh) — only suggest `cartog ide` if the user mentions Claude Code / Cursor / VS Code / Codex / Gemini / Windsurf / Zed / Claude Desktop / OpenCode |
| Fully wired | `.cartog.toml` + editor MCP files present | Just query: `cartog map`, `cartog rag search`, etc. |

### Fresh-repo handling (the most common pitfall)

cartog will **not** create a `.cartog/` index for a config-less project on its
own — `cartog index` / `rag index` / `watch` refuse until the user opts in
(consent = a present `.cartog.toml`, an existing index, or `CARTOG_AUTO_INIT`).
So when `.cartog.toml` is missing **and** no `.cartog/db.sqlite` exists yet:

1. **ASK the user** before running `cartog init`. Do not run it automatically — it writes a `.cartog.toml` file in the user's repo.
2. On user **YES**: run `cartog init` via Bash, check the exit code, then run `cartog index .`. Both commands are safe to chain in a single session. (Alternatively, `CARTOG_AUTO_INIT=1 cartog index .` indexes with defaults and writes **no** config file.)
3. On user **NO** (or "skip"): **do not** run `cartog index` — it would refuse, since the user declined to opt in, and nothing is created. Leave the repo untouched. The user can run `cartog init` (or set `CARTOG_AUTO_INIT=1`) later if they change their mind.

If `cartog init` returns non-zero (rare — usually a filesystem permission issue), surface the error to the user and **do not proceed** to `cartog index`.

**MCP server while config-less.** The plugin runs `cartog serve --watch`, which never refuses — on a config-less, un-indexed repo it starts **degraded**: no `.cartog/` is created, read tools return empty, and the write tools refuse with a "run `cartog init`" message. `cartog_stats` shows `"degraded": true`. If a tool reports the degraded state, tell the user to run `cartog init` (after asking). Once they do, the watcher **pre-builds** the index, but the **running** MCP server stays degraded until Claude Code is relaunched — at which point the freshly-built index loads. There is no live in-place upgrade.

**Non-interactive sessions** (CI, piped): cartog's SessionStart hook exits silently when no `.cartog.toml` is present (and no index exists). Setting `CARTOG_AUTO_INIT=1` bypasses the gate and indexes with defaults.

### Running cartog commands while MCP is alive

Both `cartog init` and `cartog index` are safe to run via Bash during an active MCP session, but for different reasons:
- `cartog init` is config-only — it writes `.cartog.toml`, never touches the database. No writer contention possible.
- `cartog index .` writes to the same SQLite file the MCP server has open. SQLite WAL mode lets it queue behind any active writer, and cartog's migration writes use a busy-retry loop on `SQLITE_BUSY` — so concurrent index + serve is safe.
- After indexing, an MCP server **already using the on-disk DB** picks up the new symbols on its next call (no restart needed). The exception is a **degraded** server (started config-less, holding only an in-memory placeholder — see "MCP server while config-less" above): it has no on-disk DB to re-read, so it keeps serving empty results until Claude Code is relaunched, even though `cartog index` / `cartog init` already built the index on disk.

If MCP runs with `--watch`, the watcher will also re-index on file changes (and re-embed when the repo already has embeddings). A manual `cartog index .` is still safe; it just shares the write-queue.

When two `cartog serve` instances run against the same DB (e.g. two Claude Code windows on the same project), single-writer election picks one as **primary** and the others attach **read-only**. Read-only secondaries refuse `cartog_index` / `cartog_rag_index` with a clear message but serve the other 14 MCP tools normally (the 2 write tools are gated; the remaining 14 include `cartog_update`, which arms a machine-level deferred update rather than a DB write, so it is served even though only 13 tools carry `readOnlyHint = true`). The secondary auto-promotes to primary within ~10s if the primary process dies.

If `cartog_index` or `cartog_rag_index` fails with a read-only error, call `cartog_stats` and check `role` (`primary` vs `read-only`) and `watcher_active` (whether the primary is auto-reindexing). That tells you whether to wait for promotion (~10s) or whether the primary's watcher will pick up changes on its own.

After the index is ready:

1. **Explore an unfamiliar codebase** — `cartog map` gives a file tree + top symbols ranked by centrality. Start here when onboarding or orienting.
2. **Search for anything** — `cartog rag search "your query"` is the default entry point. It handles keywords, natural language, and concept queries in a single call.
3. **Recent commits broke something?** — `cartog changes` first, then `cartog impact <symbol>` on the touched symbols.

## When to Use

Use cartog **before** reaching for grep, cat, or file reads when you need to:
- **Orient in a codebase** → `cartog map [--tokens N]` (start here for unfamiliar projects)
- Find code by name, concept, or behavior → `cartog rag search "query"`
- Search project documentation → `cartog rag search "query" --kind document`
- Understand the structure of a file → `cartog outline <file>`
- Find who references a symbol → `cartog refs <name>` (or `--kind calls` for just callers)
- See what a function calls → `cartog callees <name>`
- Assess refactoring impact → `cartog impact <name> --depth 3`
- Understand class hierarchies → `cartog hierarchy <class>`
- See file dependencies → `cartog deps <file>`
- See what changed recently → `cartog changes [--commits N]`
- **Triage a regression** ("which of my recent commits broke this?") → `cartog changes` then `cartog impact <symbol>` on the symbols listed

## How to Run

cartog has two modes — **CLI** (via Bash) and **MCP server**. **Prefer CLI by default.**

| Situation | Mode | Why |
|---|---|---|
| Single command or simple query | **CLI** | No server overhead, direct output |
| Multi-step workflow (e.g. search → refs → impact) | **MCP** | Warm DB connection, parallel tool calls |
| Subagent | **CLI** | MCP tools are only available to the main agent |

**Do not** fall back to grep/glob if MCP tools are missing — use the Bash tool instead.

**Rules for both modes:**
- Run independent `cartog` commands as **parallel tool calls** — whether MCP or Bash
- **Subagents**: always use CLI mode via Bash. Skip setup — assume the parent has already indexed

**CLI mode:** every `cartog` command in the examples below must be run via a **Bash tool call**. Do not chain with `&&` or `|` — use separate Bash calls.

All examples below use CLI syntax. MCP tool names and parameters:

| CLI command | MCP tool | Parameters |
|---|---|---|
| `cartog index .` | `cartog_index` | `path`, `force` |
| `cartog map` | `cartog_map` | `tokens?` |
| `cartog search <name>` | `cartog_search` | `query`, `kind?`, `file?`, `limit?` |
| `cartog rag search "<query>"` | `cartog_rag_search` | `query`, `kind?`, `limit?` |
| `cartog rag index .` | `cartog_rag_index` | `path`, `force` |
| `cartog outline <file>` | `cartog_outline` | `file` |
| `cartog refs <name>` | `cartog_refs` | `name`, `kind?` |
| `cartog callees <name>` | `cartog_callees` | `name` |
| `cartog impact <name>` | `cartog_impact` | `name`, `depth?` |
| `cartog hierarchy <class>` | `cartog_hierarchy` | `name` |
| `cartog deps <file>` | `cartog_deps` | `file` |
| `cartog trace <from> <to>` | `cartog_trace` | `from`, `to`, `depth?` |
| `cartog context "<task>"` | `cartog_context` | `task`, `tokens?` |
| `cartog changes` | `cartog_changes` | `commits?`, `kind?` |
| `cartog stats [--savings]` | `cartog_stats` | — |
| `cartog savings` | — (CLI only — alias for `cartog stats --savings`) | — |
| `cartog doctor` | — (CLI only) | — |
| `cartog init` | — (CLI only) | — |
| `cartog ide` (interactive picker) / `cartog install <client>...` (positional) | — (CLI only) | — |
| `cartog config` | — (CLI only) | — |

## Setup

Before first use, ensure cartog is installed and indexed.

If the project uses a remote embedding provider (`.cartog.toml` has `[embedding] provider = "ollama"` or `provider = "openai"`), that provider supplies the **embedding** model — but `cartog rag setup` still downloads the cross-encoder **reranker** (~150MB default, provider-agnostic; there is no remote reranker). Skip `rag setup` only if you also disable the reranker (`[reranker] provider = "none"`); otherwise run it once. The `openai` provider reads its API key from an env var (`api_key_env`, default `OPENAI_API_KEY`), never from `.cartog.toml`.

The plugin's SessionStart hook handles install + indexing automatically:

- **Missing binary**: install runs in the background; MCP tools become available on the next session. The user can type `/cartog-install` to install synchronously, or to retry on failure.
- **Binary present, no `.cartog.toml` and no existing index**: on an interactive session, the hook prints a hint pointing at `cartog init`. On a non-interactive session it exits silently. It does **not** index — the consent gate keeps a config-less, never-indexed repo untouched. See "Fresh-repo handling" above for the agent flow.
- **Binary present, `.cartog.toml` OR an existing `.cartog/db.sqlite`**: the hook runs `cartog index .` (foreground, typically <1s incremental) then forks `cartog rag setup` + `cartog rag index` in the background. An existing index is itself consent, so re-indexing keeps working even without a config.

If `cartog --version` fails (binary missing, broken install, wrong architecture), tell the user to run `/cartog-install` and explain it installs the cartog binary that matches the plugin version.

If `cartog --version` shows the installed binary is **older** than the plugin's pinned version, suggest `/cartog-install` to upgrade (or `cartog self update` for users on 0.14.0+). Ask before running either — the user may have pinned an older version deliberately.

### Search quality tiers

`cartog rag search` works at three quality levels depending on setup state:

| Tier | After | FTS5 | Reranker | Vector | Quality |
|---|---|---|---|---|---|
| 1 | `cartog index .` | Yes | No | No | Keyword matching only |
| 2 | `+ cartog rag setup` | Yes | **Yes** | No | Keyword + neural reranking |
| 3 | `+ cartog rag index .` | Yes | Yes | **Yes** | Full hybrid (best) |

The setup script runs tier 1 blocking (foreground), then tiers 2 + 3 in the background
so Claude is responsive immediately. `cartog rag search` works at FTS5-only quality
the moment the index is ready, and upgrades transparently to tier 2 then tier 3 as
the background pipeline completes.

**How to tell which tier you're on**: inspect the result tags in `cartog rag search` output. `[fts5+vector]` means tier 3, `[fts5]` means tier 1 or 2, `rerank=...` scores appear from tier 2 onward. See `references/query_cookbook.md` → "Interpreting results" for the full decoder.

> **First run**: tier 2 downloads ~230MB of ONNX models (cached in `~/.cache/cartog/models/`)
> in the background. Search keeps working at tier 1 in the meantime; logs go to
> `~/.cache/cartog/session.log`. Subsequent runs are instant.

## Database Location

The index is stored in a SQLite database. cartog resolves the path automatically:

| Priority | Source |
|----------|--------|
| 1 | `--db <path>` flag or `CARTOG_DB` env var |
| 2 | `.cartog.toml` → `[database] path = "..."` (looked up in cwd then at git root; `~/...` is expanded against `$HOME`) |
| 3 | Auto git-root: prefers `<root>/.cartog/db.sqlite`, falls back to legacy `<root>/.cartog.db` if only it exists |
| 4 | `.cartog/db.sqlite` in the current directory (fallback — used when not in a git repo) |

For most projects, no configuration is needed — running `cartog index .` from any subdirectory will place the DB at the git root automatically.

**Non-git projects work too.** When `git rev-parse` finds no repo, cartog falls through to the cwd-based path (priority 4), or honours an explicit `--db` / `CARTOG_DB` / `.cartog.toml [database] path` if set. There's no requirement to be inside a git working tree.

```bash
# Override examples
cartog --db /tmp/myproject.db index .
CARTOG_DB=~/.local/share/cartog/proj.db cartog index .
```

## Why cartog Over grep/glob

cartog pre-computes a code graph (symbols + edges) with tree-sitter and stores it in SQLite. Compared to grep/glob:
- **Fewer tool calls**: 1 command vs 3-6 grep/read cycles
- **Transitive analysis**: `impact --depth 3` traces callers-of-callers — grep can't do this
- **Structured results**: symbols with types, signatures, and line ranges — not raw text matches

## Workflow Rules

1. **Before you grep or read a file to understand structure**, query cartog first.

2. **Search routing** — pick the right strategy based on the query:

   **A. Semantic search** (`cartog rag search "<query>"`) — **default for all searches**. Returns code only by default; use `--kind document` for docs or `--kind all` for both. Handles keyword matching (FTS5), vector similarity, and cross-encoder reranking in a single call. Works for both natural language and keyword-style queries. Always use ONE call with the full query — never split a query into multiple rag search calls.
   ```
   cartog rag search "authentication token validation"
   cartog rag search "contract management and timesheet signing"
   cartog rag search "config"
   ```

   **B. Structural search** (`cartog search <name>`) — use **only** when you need a symbol name to feed into `refs`, `callees`, `impact`, or `hierarchy`. These commands require exact symbol names, not search results.
   ```
   cartog search validate_token
   cartog search AuthService --kind class
   ```

   **Routing rules**:
   - Need to find code? → **A** (rag search) — always
   - Need a symbol name for `refs`/`callees`/`impact`? → **B** (search) first, then the structural command
   - User already gave an exact symbol name? → call `refs`/`callees`/`impact` directly — skip `search`

3. **When using `cartog search`** to locate a symbol before `refs`/`callees`/`impact`:
   - Exactly one result → use that symbol name and file, proceed.
   - Multiple results, same name, different files → add `--file <path>` to disambiguate.
   - Multiple results, different names → add `--kind <kind>` to filter, then re-evaluate.
   - Never pass an ambiguous name to `refs`/`callees`/`impact` — the result will be wrong.

4. **Use `cartog outline <file>`** instead of `cat <file>` when you need structure, not content. Then use `Read` (with offset/limit) for the specific lines you need — this is more efficient than reading entire files.

5. **Before refactoring**, run `cartog impact <symbol>` to see the blast radius.

6. **Only fall back to grep/read** when cartog doesn't have what you need (e.g., reading actual implementation logic, string literals, config values).

7. **After making code changes**, run `cartog index .` to update the graph (LSP auto-detected for accurate edges); add `--no-lsp` only when you need a faster heuristic-only pass. If MCP is running with `--watch`, a *heuristic* re-index already happened automatically — when you need LSP-accurate edges, call the `cartog_index` MCP tool (its warm servers also catch up the heuristic backlog). In CLI-only sessions run the manual index yourself; while a `cartog serve` peer is running, `cartog index .` defers its LSP pass to the peer's warm servers (use `cartog index . --force` to run LSP locally anyway).

8. **If `refs`/`callees`/`impact` look incomplete** (fewer results than you expect), re-index *with* LSP to resolve more edges, then re-run the query. In an MCP session call the `cartog_index` tool — its warm servers also catch up any deferred backlog. In CLI-only sessions run `cartog index .` without `--no-lsp` (add `--force` if a `cartog serve` peer is running, since a plain index defers LSP to it).

## Do / Don't

**DO:**
- Use `cartog rag search` as your default search — it combines FTS5 keyword + vector + reranking in one call
- Use `cartog search` only to get a symbol name for structural commands (`refs`, `callees`, `impact`, `hierarchy`)
- Trust that `rag search` degrades gracefully — FTS5 works even without vector embeddings

**DON'T:**
- Run `cartog search` and `cartog rag search` in parallel for the same query — this wastes a tool call. `rag search` already includes FTS5 keyword matching internally
- Split one query into multiple `rag search` calls with rephrased variants — one call is enough. The hybrid search (FTS5 + vector + reranker) handles synonyms and related terms internally
- Block on RAG embedding at setup — background indexing is fine, `rag search` works immediately with FTS5 + reranker
- Assume `rag search` requires `rag index` — it works (at reduced quality) with just `cartog index .`
- Chain multiple `cartog` CLI commands with `&&` or `|` — each invocation opens a fresh SQLite connection with full initialization overhead (PRAGMAs, schema checks, cold cache). Run them as **separate tool calls** instead
- Pipe `cartog` output through `grep` — cartog already returns focused, structured results. Filtering with grep discards context (line numbers, kinds, file paths) and can break `&&` chains when grep finds no match (exit code 1)
- Combine unrelated cartog queries in one bash command — this creates false dependencies and hides failures. See `references/query_cookbook.md` → "Anti-patterns to avoid" for examples

## Commands Reference

### Index (build/rebuild)
```bash
cartog index .                    # Index current directory (with LSP if available)
cartog index . --no-lsp           # Fast heuristic-only index (~1-4s)
cartog index src/                 # Index specific directory
cartog index . --force            # Re-index all files (ignore cache)
```

By default, `cartog index .` auto-detects language servers on PATH and uses them to resolve additional edges. LSP results are **persisted in the database** — subsequent queries benefit without re-running LSP. Use `--no-lsp` for fast day-to-day indexing. LSP can be omitted entirely at build time with `--no-default-features`. When a `cartog serve` for the same database is running, a plain `cartog index .` skips its own LSP pass (no cold start) and defers resolution to the serve peer; `--force` always runs LSP locally.

### Search (find symbols by partial name)
```bash
cartog search parse                          # prefix + substring match
cartog search parse --kind function          # filter by symbol kind
cartog search config --file src/db.rs        # filter to one file
cartog search parse --limit 10               # cap results
```
Returns symbols ranked: exact match → prefix → substring. Case-insensitive. Max 100 results.

Valid `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `type-alias`, `trait`, `module`, `document`, plus `all` (code + docs).

### RAG Search (hybrid keyword + semantic)
```bash
cartog rag search "authentication token validation"
cartog rag search "error handling" --kind function
cartog rag search "database schema setup" --limit 20
cartog rag search "deployment architecture" --kind document
```

By default, returns code only. Use `--kind document` for docs or `--kind all` for both.
Uses hybrid retrieval: FTS5 keyword matching + vector KNN, merged via Reciprocal Rank Fusion.
When the cross-encoder model is available, results are re-ranked for better precision.

### Outline (file structure)
```bash
cartog outline src/auth/tokens.py
```
Output shows symbols with types, signatures, and line ranges — no need to read the file.

### Refs (who references this?)
```bash
cartog refs validate_token               # all reference types
cartog refs validate_token --kind calls  # only call sites
```
Available `--kind` values: `calls`, `imports`, `inherits`, `references`, `raises`, `implements`, `type-of`.

### Callees (what does this call?)
```bash
cartog callees authenticate
```

### Impact (transitive blast radius)
```bash
cartog impact SessionManager --depth 3
```
Shows everything that transitively depends on a symbol up to N hops.

### Trace (call path between two symbols)
```bash
cartog trace handle_request escalate_to_family   # shortest call path, bodies inline
cartog trace handle_request escalate_to_family --depth 4
```
Returns the shortest `calls` path from the first symbol to the second, each hop carrying the caller's body inline. Only statically-resolved `calls` edges are followed (dynamic dispatch is not traced). Use for "how does A reach B?"; use `cartog impact` for blast radius and `cartog refs` for all callers.

### Hierarchy (inheritance tree)
```bash
cartog hierarchy BaseService
cartog hierarchy BaseService --mermaid   # paste-into-PR diagram
```

### Deps (file imports)
```bash
cartog deps src/routes/auth.py
cartog deps src/routes/auth.py --mermaid # graph LR with file as root
```

### Map (codebase overview)
```bash
cartog map                               # default 4000 tokens
cartog map --tokens 2000                 # compact
cartog map --tokens 8000                 # detailed
cartog map --mermaid                     # graph TD rooted at "Repo"
```
File tree + top symbols ranked by reference count (centrality). Use at the start of a session for context loading. `--mermaid` honors the token budget by stopping before it overflows.

### Changes (recently modified symbols)
```bash
cartog changes                           # last 5 commits + working tree
cartog changes --commits 10              # last 10 commits
cartog changes --kind function           # only functions
```
Shows symbols affected by recent git changes, grouped by file.

### Context (one-shot task bundle)
```bash
cartog context "escalate an alert to family when a senior misses check-in"
cartog context "add OAuth refresh token rotation" --tokens 8000   # default 6000
```
The primary tool for "how does X work?" and understanding an area: one call returns the most relevant symbols (semantic + keyword search), their 1-hop call neighbors, and high-centrality definitions in the same files — bodies inline, budgeted to fit. Read-equivalent: answer from its bundle instead of a chain of `search`/`refs`/`outline`/Read; drill in with the granular tools only for follow-ups it doesn't cover. Raise `--tokens` (default 6000) for a whole subsystem. Use `cartog search` for a single known symbol and `cartog trace` for a specific call path.

### Doctor (environment health check)
```bash
cartog doctor                            # check all requirements
cartog --json doctor                     # structured JSON output
```
Validates git repo, config, database, embedding provider, and reranker. Returns OK / Warn / Error per check and exits with code 1 if any error. Run this when commands fail unexpectedly or after first setup to verify everything is working.

### Stats (index summary, savings retention hook)
```bash
cartog stats                # files, symbols, edges, languages
cartog stats --savings      # tokens-saved breakdown (see below)
cartog savings              # alias for `cartog stats --savings`
```

`cartog savings` is the retention hook. Output shape:

```
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
```

Key facts an agent should know:

- **With vs without** — `Without cartog` is an equivalent grep+read flow, baselined at ~1,700 tokens/query. `With cartog` uses the measured ~280 tokens/query.
- **By tool** = **call counts**, not per-tool token savings (the multiplier is flat across all tools). The breakdown shows which navigation patterns the user actually relies on.
- **Empty-result queries don't count.** A `cartog search nonexistent` that returns `[]` is not logged — savings reflect real work, not exploratory pings.
- Only writable databases can log. Secondary read-only MCP attaches skip the write, so multi-MCP setups under-report secondary traffic rather than double-counting it.

### Watch (auto re-index on file changes)
```bash
cartog watch .                           # watch CWD; auto-embeds if the repo already has embeddings
cartog watch . --rag                     # force re-embed (even on a not-yet-embedded repo)
cartog watch . --debounce 3 --rag-delay 30  # custom timings
```

Under `--watch`, embeddings auto-refresh when the repo already has them (run `cartog rag index` once to opt in) — no `--rag` needed. Editing a symbol's body invalidates its old embedding so it re-embeds on the next pass. Watch always uses heuristic-only indexing (no LSP) for speed. Previously LSP-resolved edges are preserved in the DB.

### Serve (MCP server)
```bash
cartog serve                    # MCP server over stdio
cartog serve --watch            # watcher; auto-embeds if the repo has embeddings (all clients get --watch by default)
cartog serve --watch --rag      # force auto-embed even on a not-yet-embedded repo
```

When an agent calls `cartog_index` via MCP, LSP servers are started once and **kept warm** for the session. Subsequent index calls reuse warm servers (~2s instead of a cold 2-15s startup). Background watch re-indexing stays heuristic-only.

### Self (upgrade / inspect / rollback / migrate)
```bash
cartog self update              # upgrade in place to latest stable
cartog self update --check      # report whether an update exists; exit 1 if outdated
cartog self version             # version + target + install source + last check
cartog self rollback            # restore the previous binary saved at <bin>.old
cartog self migrate-db          # move legacy .cartog.db (+ -wal/-shm/.bak) into .cartog/
cartog self migrate-db --dry-run  # preview the planned moves
```

User-facing maintenance commands. If the agent observes a "new cartog version available" hint or a stale binary, it can suggest the user run `cartog self update`. `cargo install cartog` users get an exit-3 refusal pointing at `cargo install cartog --force` instead. If the agent sees a one-shot deprecation warning about a legacy `.cartog.db`, suggest `cartog self migrate-db`.

### Init (scaffold project config — user-facing)
```bash
cartog init                     # scaffold .cartog.toml in the current project
cartog init --dry-run           # preview without writing
```

`cartog init` is config-only: it writes a commented `.cartog.toml` template if absent and prints next-steps hints. It does NOT index, does NOT wire MCP, and never overwrites an existing `.cartog.toml`.

**When to suggest it**: the user is starting cartog on a fresh repo (no `.cartog.toml` at the git root). The agent should mention it once, then continue with `cartog index` to build the graph. CLI-only users stop there; users on Claude Code / Cursor / VS Code / etc. can then run `cartog ide`.

### Wire cartog into editors

Two shapes for the same operation:

```bash
# `cartog install` — positional, brew/npm/pip convention. Always non-interactive.
cartog install cursor                         # one editor
cartog install cursor vscode codex            # several editors at once
cartog install                                # all detected editors (no positional = "all")
cartog install cursor --dry-run               # preview without writing
cartog install claude-code --no-watch         # drop --watch from Claude Code args

# `cartog ide` — original, supports the interactive multi-select picker.
cartog ide --yes                              # all detected clients, non-interactive
cartog ide --client cursor --yes              # one client (long-form)
cartog ide --scope project --yes              # only .mcp.json / .cursor/mcp.json / .vscode/mcp.json
cartog ide --dry-run                          # preview without writing
```

**Agent gotcha**: `cartog ide` (bare) runs an interactive multi-select picker. **An agent calling it via Bash MUST pass `--yes` (or `--client X` / `--dry-run`)**, otherwise the command blocks waiting for input. `cartog install` is always non-interactive, so it's the safe default for agents.

Supported clients: `claude-code`, `claude-desktop`, `codex`, `cursor`, `gemini`, `opencode`, `vscode`, `windsurf`, `zed`. User-scope clients whose config dir is missing are reported as "not installed" and skipped. Existing MCP entries for other servers are preserved (idempotent merge).

**When to suggest it**: the user explicitly asks to wire cartog into an editor, or mentions one of the supported clients. Do not run it speculatively; the answer "yes" should come from the user before the agent invokes this command.

### Config (print resolved configuration)
```bash
cartog config                   # human-readable resolved config
cartog config --json            # JSON for parsing
```

Prints the merged config (defaults + `.cartog.toml` + env overrides). Useful for the agent to verify which database path, embedding provider, or reranker is active before diagnosing a search-quality issue. Read-only — does not modify anything.

### Dev-only utilities

Cartog also ships `cartog completions <shell>` (shell completion script generator for bash/zsh/fish/powershell/elvish) and `cartog manpage` (emits a troff-formatted man page on stdout). Agents don't normally need these; surface them only if the user explicitly asks for shell completions or installation of a man page. See `docs/usage.md` for details.

## Token Budget

Use `--tokens N` to limit output to approximately N tokens (human-readable only, ignored with `--json`):
```bash
cartog --tokens 500 search validate
cartog --tokens 200 outline src/db.rs
cartog --tokens 1000 changes --commits 10
```

## JSON Output

All commands support `--json` for structured output:
```bash
cartog --json refs validate_token
cartog --json outline src/auth/tokens.py
cartog --json rag search "authentication"
```

Add `--compact` to strip heavy fields from `--json` and save tokens: symbol bodies
(`content`), docstrings, and cache hashes are dropped; ids, names, kinds, locations,
signatures, and scores are kept (`context` keeps its budgeted bodies). No-op without `--json`.
```bash
cartog --json --compact rag search "authentication"   # ~60% smaller, still locatable
```

Edge results (`refs`, `callees`, `impact`, `deps`, `trace`) carry a `provenance` field naming which tier resolved the edge: a heuristic tier (`same_file`, `import_path`, `same_dir`, `parent_scope`, `unique_global`, `kind_disambig`) or an LSP outcome (`lsp`, `lsp_external`, `lsp_unresolvable`). Treat `lsp`/`same_file`/`import_path` as high-confidence; `unique_global`/`kind_disambig` as best-effort guesses. Omitted for unresolved edges and indexes built before provenance tracking.

## Refactoring Workflow

Before changing any symbol (rename, extract, move, delete):

1. `cartog changes` — quick check: was this symbol touched in recent commits? Pair with `cartog impact <symbol>` on anything listed to triage regressions before refactoring.
2. `cartog search <name>` — confirm exact symbol name and file
3. `cartog refs <name>` — find every usage
4. `cartog impact <name> --depth 3` — transitive blast radius
5. `cartog hierarchy <name>` — if it's a class, check subclasses too
6. Apply changes, then `cartog index .` to update the graph (add `--no-lsp` for a faster heuristic-only pass). **Skip this step if MCP is running with `--watch`** — the watcher has already re-indexed; only run it in CLI-only sessions or to force LSP edges on demand.
7. Re-run `cartog refs <name>` to confirm no stale references remain

For the full 3-phase workflow (heuristic → LSP upgrade → verify), see `references/query_cookbook.md` → "Assess refactoring scope".

## Decision Heuristics

| I need to... | Use |
|---|---|
| Orient in an unfamiliar codebase | `cartog map` (`--tokens N` for budget control) — **start here** |
| Find code by name, concept, or behavior | `cartog rag search "query"` |
| Search project documentation | `cartog rag search "query" --kind document` |
| Search both code and docs | `cartog rag search "query" --kind all` |
| Get a symbol name for structural commands | `cartog search <name>` |
| Know what's in a file | `cartog outline <file>` |
| Find usages of a function | `cartog refs <name>` (`--kind calls` for just callers) |
| See what a function calls | `cartog callees <name>` |
| Check if a change is safe | `cartog impact <name> --depth 3` |
| Trace how one symbol reaches another | `cartog trace <from> <to>` |
| Gather everything to start a task | `cartog context "<task>"` |
| Understand class hierarchy | `cartog hierarchy <class>` |
| See file dependencies | `cartog deps <file>` |
| Render a diagram to paste into a PR / doc | append `--mermaid` to `hierarchy`, `deps`, or `map` |
| See what changed recently | `cartog changes` (`--commits N` for more history) |
| Improve graph precision for a refactoring | `cartog index .` (with LSP auto-detected) |
| Fast re-index after code changes | `cartog index . --no-lsp` |
| Diagnose why something is broken | `cartog doctor` |
| Inspect resolved config (DB path, provider, etc.) | `cartog config` |
| Triage which recent commit broke something | `cartog changes` then `cartog impact <symbol>` |
| Set up cartog in a fresh repo (no `.cartog.toml` yet) | Suggest `cartog init` to the user, then `cartog index` |
| Wire cartog into an editor (user asked) | Suggest `cartog ide --yes` (or `--client <name> --yes`) — never bare |
| Read actual implementation logic | `cat <file>` (cartog indexes structure, not content) |
| Search for string literals / config | `grep` (cartog doesn't index these) |
| Nothing from search or rag | Fall back to `grep` |

## Limitations

- Heuristic resolution is name-based (~25% of edges resolved). With LSP enabled, ~42-81% resolved depending on language. Remaining unresolved edges are mostly calls to external libraries.
- Code languages: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, C, C++, C#, PHP, Dart, Swift, Kotlin.
- Frontend SFCs: Vue (`.vue`), Svelte (`.svelte`), Astro (`.astro`) — symbols/edges from the `<script>`/frontmatter block; template markup is not indexed.
- Frameworks: React/JSX and SFC component usage (`<Counter/>`) becomes a `Calls` edge (uppercase/dotted tags only; lowercase HTML elements are skipped).
- Documents: Markdown (`.md`) — indexed by heading sections for semantic search.
- Does not index string literals, comments (except docstrings), or config values.
- Method resolution is name-based without LSP — `foo.bar()` resolves `bar`, not `Foo.bar` specifically. LSP resolves to the exact type when a language server is available.

### LSP limitations

- **Default feature**: shipped by default. Installs with `--no-default-features` omit LSP entirely (equivalent to `--no-lsp` at runtime).
- **Auto-detected**: if language servers are on PATH, they are used automatically during `cartog index`. Use `--no-lsp` to skip.
- **Startup latency**: language servers typically reach ready in 2-15s on cold cache. The default ready-timeout is 20s — override via `CARTOG_LSP_READY_TIMEOUT_SECS` for very large projects. Reach for `--no-lsp` when that startup cost isn't worth it (a quick re-index in a tight edit loop); keep LSP on when edge accuracy matters (refactoring, incomplete refs).
- **CLI vs MCP**: each `cartog index .` via Bash spawns and kills LSP servers (cold start). Use `cartog serve` (MCP mode) for sessions with multiple index calls — it keeps servers warm across tool calls.
- **Supported servers**: rust-analyzer, pyright-langserver, typescript-language-server, gopls, ruby-lsp, solargraph, jdtls, csharp-ls, intelephense (phpactor fallback), dart, sourcekit-lsp, kotlin-language-server, clangd, vue-language-server, svelteserver, astro-ls. Install hints shown when servers are missing.
- **External crate edges stay unresolved**: LSP resolves definitions within the project. Calls to std/external crates remain unresolved regardless.

### RAG search limitations

- **No substring matching**: `"valid"` does NOT match `validate_token`. FTS5 is token-based. If `rag search` returns no results for a known symbol name, fall back to `cartog search` which supports substring matching.
- **Graceful degradation**: `rag search` works without `rag setup` or `rag index` (FTS5-only). Quality improves with each setup tier (see Search quality tiers above).
- **Scores are relative**: `rrf_score` and `rerank_score` values are only meaningful for ranking within a single query — don't compare scores across different queries.
- **Re-ranking latency**: cross-encoder scores all candidates in a single batch ONNX call (up to 50 candidates). Expect ~50-300ms total overhead depending on candidate count (the default jina-turbo reranker is ~3x faster than the older bge-reranker-base).
- **Auto re-embed**: when cartog upgrades its embedding format (e.g., AST-aware chunking), `cartog rag index` automatically detects the change and re-embeds all symbols. No `--force` needed.
