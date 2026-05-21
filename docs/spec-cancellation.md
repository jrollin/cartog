# Cooperative Cancellation for MCP Indexing Tools

## Overview

Cooperative cancellation for the long-running MCP tools `cartog_index` and
`cartog_rag_index`. When an MCP client cancels the request (sends
`notifications/cancelled`), the indexer aborts at the next checkpoint
instead of running to completion. For `index_directory` the active
rusqlite transaction rolls back on `Err`, so the run leaves no code-graph
changes. For `index_embeddings`, batches flushed before the cancel point
persist; the in-flight batch is dropped. Either way the DB stays
consistent and the next index run picks up where the cancelled one left
off.

## Motivation

- A `cartog_index` on a large repo can run for minutes. Without cancellation,
  the MCP client is stuck until completion or transport drop.
- `RequestContext.ct` (a `CancellationToken` from rmcp) already flips when the
  client cancels. We were not consuming it.
- The progress callback added in PR #57 fires at coarse phase boundaries — a
  natural place to also poll cancellation.

## Non-goals

- Mid-file cancellation (interrupting a single tree-sitter parse).
- Bespoke transaction handling on cancel. We rely on rusqlite's default
  `Transaction::Drop` rollback for `index_directory`; `index_embeddings`
  inherits whatever was flushed before the cancel point.
- CLI cancellation beyond the existing Ctrl-C behavior.
- Watcher cancellation. `cartog serve --watch` runs outside any request
  lifecycle; nothing to wire `ctx.ct` to. Interaction with a watcher: none.
  A cancelled MCP-driven index leaves the DB consistent (see FR-4); the
  watcher's normal debounced re-index covers anything still missing on the
  next file change.
- Cancellation on the sub-second tools (`cartog_search`, `cartog_rag_search`,
  etc.). Not worth the churn.

## Architecture

```
MCP client ──cancel──▶ RequestContext.ct.cancel()
                                │
                                ▼
                       cartog-mcp tool handler
                                │  (clone CancellationToken into spawn_blocking)
                                ▼
                       indexer / rag-indexer
                                │  (poll at each checkpoint)
                                ▼
                       Err("cancelled") ──▶ McpError surfaces to client
```

Indexer crates stay transport-agnostic: they accept
`pub type CancelProbe<'a> = &'a (dyn Fn() -> bool + Send + Sync)` alongside the
existing `ProgressCallback`. The MCP layer wraps `ctx.ct.is_cancelled()` into
that closure. CLI passes `None`.

## Functional requirements

- **FR-1**: `index_directory` checks cancellation at each phase boundary
  (Walking, Parsing, Storing) and between files in the storing loop.
- **FR-2**: `index_embeddings` checks cancellation at Preparing and between
  embedding batches.
- **FR-3**: On cancellation, the indexer returns
  `Err(anyhow!("cancelled"))` so the MCP error message contains the literal
  `cancelled`, which clients (and tests) can match on.
- **FR-4**: On cancellation, the indexer returns `Err` before reaching
  `tx.commit()`. The `rusqlite::Transaction` drops in its default mode and
  rolls back — `index_directory`'s code-graph writes for the cancelled run
  are discarded. `index_embeddings` does not run inside a single transaction:
  any embedding batches already flushed to SQLite stay, the in-flight batch
  is dropped. The DB stays consistent either way; the next run redoes the
  missing work.
- **FR-5**: CLI behavior unchanged — CLI passes `None` for the probe.
- **FR-6**: With `cancel = None`, behavior is byte-identical to today (one
  closure-call per checkpoint, returns `false`).

## Non-functional requirements

- **NFR-1**: Probe overhead is one closure call per file (Phase 3) or per
  embedding batch (Phase 2 of RAG). Must not regress `make bench`.
- **NFR-2**: No new crate dependency. `CancellationToken` reaches us via the
  existing `rmcp` dep; the indexer crates use a plain `dyn Fn()`.

## Acceptance criteria

- **AC-1**: Unit tests cover the three cases:
  probe-trips → `Err("cancelled")`,
  probe-never-trips → identical to `None`,
  re-run after cancellation completes normally.
  ✓ See `cartog-indexer::cancel_probe_*` and `cartog-rag::cancel_probe_*` tests.
- **AC-2**: `cargo test --workspace` passes; `cargo clippy --all-targets -- -D
  warnings` clean.

## Implementation checklist

- [x] Add `pub type CancelProbe` to `cartog-indexer` and `cartog-rag`.
- [x] Add `cancel: Option<CancelProbe<'_>>` to `index_directory` and
      `index_embeddings`.
- [x] Sprinkle `check_cancel()?` at phase boundaries and per-file/per-batch.
- [x] In `cartog-mcp`: clone `ctx.ct` into `spawn_blocking`, build a probe,
      thread through `index_with_optional_lsp` and `index_embeddings` calls.
- [x] CLI / watcher / tests / benches pass `None`.
- [x] Unit tests for cancellation behavior.
- [x] Update `docs/usage.md` MCP section with a one-line note.

## Resolved questions

1. **Cancellation contract for in-flight transactions** — for
   `index_directory`, rusqlite's default `Transaction::Drop` rolls back the
   indexing tx, so the run leaves no code-graph changes. For
   `index_embeddings`, batches flushed before the cancel point persist; the
   in-flight batch is dropped. Either way the DB stays consistent.
2. **rmcp error mapping** — start with a recognisable `McpError` message
   containing `cancelled`. A first-class `request_cancelled` variant in rmcp
   would be a future polish.
3. **Two-pass edge resolution partial state** — fine. Next run re-resolves.
