# cartog-mcp

MCP server for the cartog code graph.

## Overview

Exposes cartog's graph queries, indexing, and semantic search as MCP tools over stdio transport. Designed for integration with Claude Code, Cursor, and other MCP-compatible clients.

## How it works

### MCP tools

12 tools are exposed via rmcp's `#[tool_router]` macro with auto-generated JSON Schema parameters:

| Tool | Description |
|------|-------------|
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

## Public API

| Export | Description |
|--------|-------------|
| `run_server()` | Start the MCP server over stdio (async). Accepts `EmbeddingProviderConfig` and threads it through to all RAG operations (indexing and search). |

## Crate dependencies

`cartog-core`, `cartog-db`, `cartog-indexer`, `cartog-rag`, `cartog-watch`, optionally `cartog-lsp`
