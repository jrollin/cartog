# Feature: `cartog watch`

## Overview

A long-running CLI command that watches filesystem events, debounces changes, and automatically re-indexes the code graph + RAG embeddings. Keeps the `.cartog.db` fresh without manual `cartog index` / `cartog rag index` cycles, especially useful during active development before commits.

## Architecture

```
cartog watch [path] [--debounce 2s] [--rag] [--rag-delay 30s]
     |
     v
notify crate (kqueue macOS / inotify Linux / ReadDirectoryChangesW Windows)
     |
     v
Debounce filter (2s default) ──> code graph index (incremental, ~1-3s)
     |                                    |
     |                            symbols_needing_embeddings > 0?
     |                                    |
     v                                    v
                              Deferred RAG batch (30s timer)
                                    |
                                    v
                              rag::indexer::index_embeddings(force=false)
```

### Key Design Decisions

1. **Debounced file watcher** (not git polling): Uses `notify` crate with a configurable debounce window (default 2s). Responds to actual file saves, not periodic checks. The existing `is_ignored()` filter + `detect_language()` already handle skipping irrelevant files.

2. **Deferred RAG embedding batch**: After each code graph re-index, if there are symbols needing embeddings (`symbols_needing_embeddings().len() > 0`), a timer starts. Resets on each new index cycle. When the timer fires (default 30s of inactivity), embeddings are generated in bulk. This amortizes the ~200ms model load and batches embedding calls.

3. **Single DB connection**: The watcher holds the `Database` handle for its lifetime. No concurrent access concerns since `cartog watch` is the sole writer. MCP server reads are safe because SQLite WAL mode allows concurrent readers.

4. **Graceful shutdown**: Ctrl+C (SIGINT) handler flushes any pending embeddings before exit.

## Functional Requirements

### FR-001: File Change Detection
When a supported source file (Python, TypeScript/JavaScript, Rust, Go, Ruby) is created, modified, or deleted within the watched directory, the system shall queue a re-index after the debounce window expires.

### FR-002: Debounce Window
While file change events are arriving within the debounce window (default 2s, configurable via `--debounce`), the system shall reset the timer and not trigger re-indexing until the window elapses without new events.

### FR-003: Incremental Code Graph Re-index
When the debounce window expires, the system shall run `indexer::index_directory(db, root, false)` (incremental mode) and log the result (files indexed, skipped, removed, symbols, edges).

### FR-004: Ignored Paths
The system shall not trigger re-indexing for changes in ignored directories (`.git`, `node_modules`, `target`, `__pycache__`, etc.) — reusing the existing `is_ignored()` filter.

### FR-005: Deferred RAG Embedding
While `--rag` flag is set and symbols needing embeddings exist after a code graph re-index, when the RAG delay timer (default 30s, configurable via `--rag-delay`) expires without new index cycles, the system shall run `rag::indexer::index_embeddings(db, false)` to embed all pending symbols.

### FR-006: RAG Timer Reset
While embedding is pending and a new code graph re-index occurs, the system shall reset the RAG delay timer to avoid embedding during active editing.

### FR-007: Startup Index
When `cartog watch` starts, the system shall run one initial incremental index to ensure the DB is current before entering watch mode.

### FR-008: Status Logging
The system shall log to stderr: startup info (path, debounce, rag mode), each re-index summary, each RAG embedding summary, and shutdown.

### FR-009: Graceful Shutdown
When SIGINT (Ctrl+C) is received, the system shall flush any pending RAG embeddings, then exit cleanly.

## Non-Functional Requirements

### Performance
- Code graph re-index latency: < 3s for incremental (< 50 changed files)
- Memory: embedding engine loaded lazily on first RAG batch, ~300MB resident
- CPU: idle when no changes (epoll/kqueue, not polling)

### Reliability
- File watcher errors (e.g., too many open files) shall be logged and retried, not fatal
- Index errors on individual files shall be logged and skipped (existing behavior)
- Embedding errors shall be logged per-symbol (existing fallback behavior in `flush_embedding_batch`)

### Platform
- macOS: kqueue via `notify` crate
- Linux: inotify via `notify` crate
- Windows: ReadDirectoryChangesW via `notify` crate

## CLI Interface

```
cartog watch [OPTIONS] [PATH]

Arguments:
  [PATH]  Directory to watch (defaults to ".")

Options:
      --debounce <DURATION>   Debounce window for file changes [default: 2s]
      --rag                   Enable automatic RAG embedding after index
      --rag-delay <DURATION>  Delay before batch embedding after last index [default: 30s]
      --json                  Output events as JSON (global flag)
```

## Acceptance Criteria

### AC-001: Basic file change triggers re-index
Given `cartog watch` is running on a directory
When I save a Python/TS/Rust/Go/Ruby file
Then the code graph is re-indexed within debounce window + index time
And the log shows files indexed count > 0

### AC-002: Rapid saves are debounced
Given `cartog watch --debounce 2s` is running
When I save a file 5 times in 1 second
Then re-indexing occurs only once (after the 2s debounce)

### AC-003: Ignored directories don't trigger
Given `cartog watch` is running
When a file changes inside `node_modules/` or `.git/`
Then no re-indexing occurs

### AC-004: File deletion removes stale data
Given `cartog watch` is running and `foo.py` is indexed
When I delete `foo.py`
Then after debounce, re-indexing removes `foo.py` symbols/edges from the DB

### AC-005: RAG embedding batches after inactivity
Given `cartog watch --rag --rag-delay 10s` is running
When I save a file and wait 10+ seconds without further changes
Then RAG embeddings are generated for new/changed symbols
And the log shows symbols embedded count > 0

### AC-006: RAG timer resets on new changes
Given `cartog watch --rag --rag-delay 10s` is running
When I save a file, wait 5s, then save another file
Then RAG embedding does not start at 10s from first save
And instead starts at 10s from the second save

### AC-007: Ctrl+C graceful shutdown
Given `cartog watch --rag` is running with pending embeddings
When I press Ctrl+C
Then pending embeddings are flushed before exit

### AC-008: Startup initial index
Given `.cartog.db` exists but is stale
When I run `cartog watch`
Then an initial incremental index runs before entering watch mode

## Error Handling

| Error Condition | Behavior |
|---|---|
| Watched directory doesn't exist | Exit with error message |
| File watcher init fails (permissions) | Exit with error message |
| Individual file read error | Log warning, skip file, continue watching |
| Tree-sitter parse error | Log warning, skip file, continue watching |
| DB write error | Log error, continue watching (next cycle may succeed) |
| Embedding model not found (--rag) | Log warning, skip RAG embedding, continue code graph indexing |
| ONNX inference error on symbol | Log warning, skip symbol (existing fallback) |

## Implementation TODO

All items completed.

### Dependencies
- [x] Add `notify = "7"` to `Cargo.toml` (debounced file watcher)
- [x] Add `notify-debouncer-mini = "0.5"` to `Cargo.toml` (debounce layer)
- [x] Add `ctrlc = "3"` to `Cargo.toml` (graceful Ctrl+C handling)
- [x] ~~`humantime`~~ — dropped in favor of `u64` seconds args (simpler, no extra dep)

### CLI
- [x] Add `Watch` variant to `Command` enum in `cli.rs`
- [x] Add `--watch` and `--rag` flags to `Serve` variant in `cli.rs`
- [x] Wire up both in `main.rs` match arm

### Core: `src/watch.rs`
- [x] Create `watch.rs` module with `WatchConfig`, `WatchHandle`, `run_watch()`, `spawn_watch()`
- [x] `notify_debouncer_mini` for debounced file events
- [x] `is_relevant_path()` filter using `detect_language()` + `is_ignored_dirname()`
- [x] On debounce fire: `indexer::index_directory(&db, root, false)`
- [x] RAG deferred timer: checks `symbols_needing_embeddings()`, fires after `rag_delay`
- [x] Ctrl+C handler via `ctrlc` crate: flush pending RAG, exit cleanly

### MCP Server Integration
- [x] `cartog serve --watch` spawns background watcher via `spawn_watch()`
- [x] `cartog serve --watch --rag` also enables deferred RAG embedding
- [x] `WatchHandle` dropped on server shutdown, signaling watcher thread to stop
- [x] Watcher opens its own DB connection (SQLite WAL allows concurrent readers)

### Commands
- [x] `cmd_watch()` in `commands.rs`
- [x] `run_server(watch, rag)` updated in `mcp.rs`

### Public API for reuse
- [x] `indexer::is_ignored_dirname()` — extracted from private `is_ignored()` for sharing

### Testing (14 new tests)
- [x] 13 `is_relevant_path()` tests (language filtering + ignored dirs)
- [x] 1 `WatchConfig` defaults test

## Out of Scope

- **Git hook integration**: Could be added separately as `cartog hook install`.
- **Selective RAG**: No per-file embedding control. `--rag` is all-or-nothing.
- **Remote file watching**: Only local filesystem events.
- **`--no-initial-index` flag**: Not needed — initial index is fast (incremental).
