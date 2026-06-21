# cartog MCP Server Reference

> CLI commands: [cli.md](cli.md) | Configuration: [config.md](config.md) | Editor wiring: [../how-to/wire-editors.md](../how-to/wire-editors.md)

## Overview

`cartog serve` runs cartog as an MCP server over stdio, exposing 16 tools for MCP-compatible clients (Claude Code, Cursor, Windsurf, etc.). Each tool carries a human-readable `title` and a `readOnlyHint` annotation: 13 query tools are read-only (including `cartog_trace` for call paths and `cartog_context` for one-shot task bundles); `cartog_index` and `cartog_rag_index` write the index; and `cartog_update` arms a deferred self-update (`readOnlyHint = false` because it writes the machine-level state file, but it never touches the index). Clients can skip approval prompts for the read-only ones.

When `cartog serve --watch` is running and a file changes (or RAG embeddings are still catching up — including symbols whose body was just edited and not yet re-embedded), affected read-tool responses are prefixed with a `⚠️` staleness banner so the agent knows the answer may be momentarily behind the working tree. Read-only secondaries and `cartog serve` without `--watch` never show the banner.

Read tools also declare an `outputSchema` and return `structuredContent` (the typed result mirrored alongside the human-readable text block) so schema-aware clients get validated, machine-readable output. To keep responses within the caller's context window, the size cap (`CARTOG_MCP_MAX_BYTES`, default 64 KB) counts the text block plus the structured copy: `structuredContent` is dropped when the combined size would exceed the cap (and when the text block itself is truncated, which adds a truncation notice).

For editor-specific recipes (Neovim keymaps, VS Code tasks, Emacs `compile`, Telescope picker, `cartog watch --json` floating buffer), see **[../how-to/wire-editors.md](../how-to/wire-editors.md)**.

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

## Available Tools

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

## Progress notifications

`cartog_index` and `cartog_rag_index` emit standard MCP `notifications/progress` when the client includes a `progressToken` in the request's `_meta`. `cartog_index` reports climbing per-phase counters — `scanning files`, then `parsing M/N files`, `storing M/N files`, and (when the LSP pass runs) `resolving M/N edges with LSP` — so a phase emits many ticks as it advances, not a single event. `cartog_rag_index` emits `preparing`, one `embedding processed/total` per ~512-symbol batch, then `storing embeddings`. The `progress`/`total` values are accumulated across phases, and per the [MCP progress spec](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/progress) each emitted notification **strictly increases** `progress` — duplicate or out-of-order ticks that would not advance the bar are dropped rather than sent equal. The `message` field is human-readable, not a contract. Clients that do not supply a `progressToken` see no notifications and behavior is unchanged. Cold-cache or `force=true` runs report larger totals than warm runs.

**Seeing the progress bar**: the bar is rendered by the **client**, not cartog — the server only emits the notifications described above. Support varies:

| Client | Sends `progressToken` | Renders a bar |
|--------|:---:|:---:|
| Cursor (≥ 1.7.39) | yes | yes |
| GitHub Copilot CLI (≥ 0.0.389) | yes | yes (text) |
| Claude Code | no | no |
| Claude Desktop | — | no (notifications received, not surfaced) |
| opencode | no | no (no progress handler wired today) |

In a client that does not render progress (Claude Code, Claude Desktop today), the index call is silent until it returns its result summary; this is a client limitation, not a cartog one. To watch a live bar, run the indexer from the CLI instead — `cartog index` / `cartog rag index` show the same per-phase counters on the terminal spinner, sourced from the identical `ProgressUpdate` events. To verify the notifications on the wire yourself, drive `cartog serve` over stdio with a `tools/call` whose `params._meta.progressToken` is set and watch for `notifications/progress` frames.

## Cancellation

`cartog_index` and `cartog_rag_index` honor MCP `notifications/cancelled`. The abort lands at the next phase/file/edge-window boundary, and the tool returns an error whose message contains `cancelled`. Latency is usually sub-second, but a cancel arriving while a language server is still loading its project model is only noticed when startup finishes (up to `CARTOG_LSP_READY_TIMEOUT_SECS`, default 20s). The parse/store phase runs in a single rusqlite transaction that rolls back on the error path; on the MCP path LSP resolution runs after that transaction commits, so a cancel during the LSP phase can leave the parse/store output committed with edges only partially LSP-resolved (still correct — re-index with `force` to finish resolving). `cartog_rag_index` keeps embedding batches already flushed to SQLite; a `--force`/format-upgrade run clears up front, so re-run to rebuild. In all cases the next index run redoes the missing work.

## Built-in Workflow Guidance

The MCP server sends workflow instructions to the client at initialization, covering tool chaining order (index → search → refs/callees/impact → re-index) and when to use semantic search. Clients that support the MCP `instructions` field will surface these automatically.

## Logging

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

## Plugin vs MCP vs Skill

| | Claude Plugin | MCP Server | Agent Skill |
|-|--------------|-----------|-------------|
| Install | `/plugin marketplace add jrollin/cartog` then `/plugin install cartog@cartog-plugins` | `claude mcp add cartog -- cartog serve` | `npx skills add jrollin/cartog` |
| Context cost | ~150 lines of prompt | Zero (tools are protocol-level) | ~150 lines of prompt |
| Workflow guidance | Full heuristics | Basic (via `instructions` field) | Full heuristics |
| Compatibility | Claude Code only | MCP clients only | Any LLM with bash |
| Latency | Fork+exec per command | Persistent process | Fork+exec per command |

Use the **plugin** for Claude Code (simplest setup, includes skill + scripts + agents). Use **MCP** when you want lower token cost with an MCP-compatible client. Use the **skill** for non-Claude Code environments.
