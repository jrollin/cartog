# cartog-watch

File system watcher with auto-reindexing for cartog.

## Overview

Watches a directory for source file changes and triggers incremental re-indexing. Optionally defers RAG embedding to batch changed symbols after a quiet period.

## How it works

### Debounced file watching

Uses `notify-debouncer-mini` to batch rapid filesystem events into a single re-index call. Default debounce window is 2 seconds.

Events are filtered to **relevant paths only**: the file must have a supported extension (code or Markdown, via `detect_language`) and not be under an ignored directory (`.git`, `node_modules`, `target`, etc.).

### RAG timer

When RAG embedding is enabled (`rag = true`):

1. After each re-index, check if any symbols need embedding
2. If yes, record `Instant::now()` and set `rag_pending = true`
3. Poll every 500ms; when `elapsed >= rag_delay` (default: 30s), trigger embedding
4. On graceful shutdown (Ctrl+C), flush any pending embeddings before exiting

The delay avoids re-embedding after every single file save during active editing.

### Execution modes

- **`spawn_watch()`** — runs the watch loop on a background thread, returns a `WatchHandle` for stop/drop
- **`run_watch()`** — runs the watch loop in the foreground (blocking), with Ctrl+C handler for graceful shutdown

Both modes open their own `Database` connection (SQLite WAL allows concurrent readers).

## Public API

| Export | Description |
|--------|-------------|
| `WatchConfig` | Configuration: root path, debounce window, RAG toggle, RAG delay, `rag_config` (RAG provider configuration for embedding + reranker, threaded from `.cartog.toml`) |
| `WatchHandle` | Handle to stop a background watcher (via `stop()` or `Drop`) |
| `spawn_watch()` | Start watching on a background thread |
| `run_watch()` | Start watching in the foreground (blocking) |

## Crate dependencies

`cartog-core`, `cartog-db`, `cartog-indexer`, `cartog-rag`
