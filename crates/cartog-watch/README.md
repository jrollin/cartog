# cartog-watch

File system watcher with auto-reindexing for cartog.

## Overview

Watches a directory for source file changes and triggers incremental re-indexing. Optionally defers RAG embedding to batch changed symbols after a quiet period.

## How it works

### Debounced file watching

Uses `notify-debouncer-mini` to batch rapid filesystem events into a single re-index call. Default debounce window is 5 seconds.

Events are filtered to **relevant paths only**: the file must have a supported extension (code or Markdown, via `detect_language`) and not be under an ignored directory (`.git`, `node_modules`, `target`, etc.).

### RAG timer

When RAG embedding is enabled (`rag = true`):

1. After each re-index, check if any symbols need embedding
2. If yes, record `Instant::now()` and set `rag_pending = true`
3. Poll every 500ms; when `elapsed >= rag_delay` (default: 30s), trigger embedding
4. On graceful shutdown (Ctrl+C), flush any pending embeddings before exiting

The delay avoids re-embedding after every single file save during active editing.

### Execution modes

- **`spawn_watch(config, db_path)`** — runs the watch loop on a background thread, returns a `WatchHandle` for stop/drop
- **`run_watch(config, db_path)`** — runs the watch loop in the foreground (blocking), with Ctrl+C handler for graceful shutdown

Both modes open their own `Database` connection (SQLite WAL allows concurrent readers).

## Public API

| Export | Description |
|--------|-------------|
| `WatchConfig` | Configuration: `root`, `debounce`, `rag` toggle, `rag_delay`, `rag_config` (provider config for embedding + reranker, threaded from `.cartog.toml`), `json_events` (emit machine-readable event lines), `pid_lock_dir` + `pid_lock_slot` (PID-lock tracking; both must be set together or neither), `skip_migrations` |
| `WatchHandle` | Handle to stop a background watcher (via `stop()` or `Drop`) |
| `WATCH_LOCK_SLOT` | Conventional PID-lock slot name (`"watch"`) |
| `spawn_watch(config, db_path)` | Start watching on a background thread |
| `run_watch(config, db_path)` | Start watching in the foreground (blocking) |

## Crate dependencies

`cartog-core`, `cartog-db`, `cartog-indexer`, `cartog-rag`, `cartog-process-lock`
