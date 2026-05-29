# cartog-mcp

MCP server for the cartog code graph.

## Overview

Exposes cartog's graph queries, indexing, and semantic search as MCP tools over stdio transport. Designed for integration with Claude Code, Cursor, and other MCP-compatible clients.

## How it works

### MCP tools

13 tools are exposed via rmcp's `#[tool_router]` macro with auto-generated JSON Schema parameters. Each tool also carries `annotations` (a `title` and `readOnlyHint`: `true` for the 11 query tools, `false` for the two index tools), and the read tools declare an `outputSchema`:

| Tool | Description |
|------|-------------|
| `cartog_map` | Orient in a repo: file list + top symbols by centrality |
| `cartog_index` | Build or rebuild the code graph index |
| `cartog_outline` | Show symbols and structure of a file |
| `cartog_refs` | All references to a symbol |
| `cartog_callees` | Outgoing calls from a symbol |
| `cartog_impact` | Transitive impact analysis (max depth: 10) |
| `cartog_hierarchy` | Inheritance hierarchy for a class |
| `cartog_deps` | File-level import dependencies |
| `cartog_search` | Search symbols by name |
| `cartog_stats` | Index statistics summary |
| `cartog_changes` | Symbols affected by recent git changes |
| `cartog_rag_index` | Build embedding index for semantic search |
| `cartog_rag_search` | Semantic search over code symbols |

### Path validation

All user-supplied paths are validated against the project root:

1. Canonicalize CWD once at server startup (stored in `Arc<Path>`)
2. Resolve user path relative to CWD (or keep absolute)
3. Canonicalize if the path exists; otherwise normalize with a stack-based algorithm (handles `..` without filesystem access)
4. Reject any path that doesn't `starts_with(cwd)` — prevents directory traversal escapes

### Concurrency

- `Arc<Mutex<Database>>` for shared state across async tool handlers
- Tool handlers run on `tokio::task::spawn_blocking` to avoid blocking the async runtime during SQLite queries
- Optional background watch thread via `cartog-watch` for live re-indexing during MCP sessions

### Progress notifications

`cartog_index` and `cartog_rag_index` emit standard MCP `notifications/progress` events when the client supplies a `progressToken` in the request's `_meta`. The bridge lives in `src/progress.rs`: a bounded mpsc channel decouples the blocking indexer (best-effort `try_send`) from an async forwarder that calls `Peer::notify_progress` with a monotonic counter. Clients that don't subscribe see byte-identical behavior to the no-token path.

### Output schemas and structured content

Read tools return both a text block (the original JSON shape, including bare arrays) and `structuredContent` (the typed result), and declare a matching `outputSchema`. MCP requires `structuredContent` to be a JSON object and `schema_for_output` rejects non-object schemas, so list tools wrap their array under a `results` field (`SymbolList`, `EdgeList`, …). `structuredContent` is omitted when a response is truncated past `CARTOG_MCP_MAX_BYTES`, so an oversized payload can't bypass the size cap.

## Public API

| Export | Description |
|--------|-------------|
| `run_server(db_path, watch, rag, rag_config, opts)` | Start the MCP server over stdio (async). `rag_config: EmbeddingProviderConfig` threads through to all RAG operations; `opts: ServerOptions` configures PID-lock tracking. |
| `ServerOptions` | `pid_lock_dir` + `pid_lock_slot` for single-writer election (both set together, or neither for untracked mode). |
| `SINGLE_WRITER_ENV` | Name of the env var (`CARTOG_SINGLE_WRITER`) that disables single-writer election when set to `0`. |

## Crate dependencies

`cartog-core`, `cartog-db`, `cartog-indexer`, `cartog-rag`, `cartog-watch`, optionally `cartog-lsp`
