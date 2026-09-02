# Concurrency

> **This file has moved.** The canonical version is now at [../explanation/concurrency.md](../explanation/concurrency.md). This copy is kept so old links do not 404.

## 1. Why this doc

cartog runs work concurrently in several places, using three deliberately
separate models. This page is the single explainer for **what** runs
concurrently, **why**, its **limitations**, and its **impact**. Decision-table
rows in [tech.md](../tech.md) point here for the full picture.

The three models, and the rule that keeps each one safe:

| Model | Used for | The rule |
|-------|----------|----------|
| **rayon** (data-parallel CPU) | Parsing many files at once | Workers operate on **disjoint** files; no shared mutable state on the hot path |
| **tokio async** (`cartog-mcp` only) | The MCP server: request handling + background tasks | Blocking work goes to `spawn_blocking`; **no lock is held across `.await`** |
| **`std::thread`** (long-lived loops) | LSP reader, file watcher, CLI spinner, detached helpers | One owner, shutdown via `Arc<AtomicBool>` / channel close |

Two principles hold everywhere:

1. **CPU parallelism is partitioned, not shared.** Progress is tracked with
   atomics, not mutexes, so workers never contend.
2. **Async never blocks and never holds a lock across an await.** All blocking
   work (DB, embeddings, fs) is offloaded; the one genuinely tricky protocol
   (single-writer promotion) is model-checked (TLA+ + Loom).

The 95% of commands that are plain sync CLI (`index`, `search`, `refs`, …) never
touch tokio at all — the async runtime is created on demand only by `serve`.

---

## 2. File parsing — rayon `par_iter`

**Where:** `crates/cartog-indexer/src/lib.rs` (parse phase; the
`THREAD_EXTRACTORS` thread-local).

**Usage.** During `cartog index`, after the change-detection layers prune the
file set (see [incremental-indexing.md](incremental-indexing.md)), the surviving
candidate files are parsed in parallel with a rayon `par_iter`. Each file is
tree-sitter parsed and its symbols/edges extracted independently; results are
collected into a `Vec` and then written to SQLite in a **single sequential**
phase.

**Why.** Parsing + AST traversal is the dominant CPU cost of indexing, and files
are independent at parse time (cross-file edge resolution happens later, in the
DB). This is the highest-leverage parallelism in cartog.

**Limitations.**
- Only the *parse* phase is parallel. DB writes and edge resolution are
  sequential (SQLite is a single connection; see §5).
- Scaling is bounded by core count and by the slowest single file (a huge
  generated file is one work item).
- rayon uses its global pool — not separately tuned per command.

**Impact.** Scales ~linearly with cores. On the embedding-index benchmark the
parse-heavy pass showed **~6× core parallelism** (131s of CPU time in ~22s wall).
Safety: each rayon worker keeps a **thread-local** extractor cache
(`THREAD_EXTRACTORS`) so a tree-sitter `Parser` is built once per language per
worker, not once per file; out-of-order completion is reported monotonically via
`AtomicU32` counters, so there is zero mutex traffic on the hot path.

---

## 3. Model inference — ONNX intra-op threads

**Where:** `crates/cartog-rag/src/providers/local.rs` (`onnx_intra_threads`,
`EMBED_BATCH_SIZE`).

**Usage.** The local embedding model and the cross-encoder reranker run on ONNX
Runtime via fastembed. The math *inside* a single inference call is parallelized
across cores (intra-op threads). The cap is resolvable:
`CARTOG_ONNX_THREADS` env > `[embedding.local] intra_threads` config > fastembed
default (all cores).

**Why.** Dense tensor ops (matmul, reductions) parallelize well within the
inference graph. This is the parallelism lever for embedding/rerank throughput.

**Limitations.**
- Only **intra-op** is exposed; there is no inter-op / pipeline parallelism.
- RAG *indexing* itself is **sequential** by design — texts are processed in
  fixed chunks (`EMBED_BATCH_SIZE = 64`, larger DB-flush chunks above that) for
  clean progress reporting and Ctrl-C cancellation. Throughput comes from the
  ONNX pool, not from fanning batches across rayon. (Pushing embed throughput
  further — a dedicated rayon pool or pipelined batches — is a known open lever,
  not a shipped feature.)
- Applies to the **local** provider only; Ollama / OpenAI providers are network
  I/O, serialized per request.

**Impact.** The quantized model is ~2–3× faster than full precision at
negligible quality cost. A separate win that compounds with the threads: indexed
texts are **sorted by length** before embedding to minimize padding waste, worth
roughly 30–50% on the inference pass.

---

## 4. MCP tool handlers — `tokio::task::spawn_blocking`

**Where:** `crates/cartog-mcp/src/lib.rs` (all 17 tool handlers); runtime is
`tokio` with `rt-multi-thread`.

**Usage.** `cartog serve` is an async MCP server over stdio. Every tool handler
(`cartog_search`, `cartog_rag_search`, `cartog_index`, …) wraps its blocking
work — DB queries, parsing, embedding — in `spawn_blocking`, runs it on tokio's
blocking thread pool, and `.await`s the result.

**Why.** Tool bodies are synchronous and can take 100 ms–seconds. Running them
directly on the async runtime would stall every other in-flight request. Moving
them to the blocking pool keeps the runtime responsive so concurrent tool calls
overlap.

**Limitations.**
- The DB is a **single** `Arc<Mutex<Database>>` connection, not a pool, so
  concurrent tool calls serialize on DB access. This is fine because an MCP
  server backs one agent session — contention is negligible — but it means a
  slow query does hold the DB mutex for its duration (relevant to any future
  feature that adds a *second* caller of that mutex).
- SQLite WAL gives concurrent **readers** + one writer; reader-vs-checkpoint and
  writer-vs-writer contention is bounded by `busy_timeout` (5 s).

**Impact.** The runtime never blocks; tool calls stay concurrent. Safety: the
mutex is locked **inside** the blocking closure and released before `.await`
returns, so **no lock is ever held across an await** (a project invariant). The
documented lock order is `lsp_manager → db → embedding_provider →
reranker_provider`. `std::sync::Mutex` is used (not `tokio::sync`) precisely
because the lock never spans an await.

---

## 5. Single-writer promoter — `tokio::spawn` background task

**Where:** `crates/cartog-mcp/src/single_writer.rs` (the promoter task and
`AtomicRole`).

**Usage.** When several `cartog serve` instances run on the same DB, an atomic
O_EXCL election picks one **primary** (DB read-write); the rest attach
**read-only**. A read-only secondary spawns a background task that polls the
primary's liveness (~10 s) and promotes itself if the primary dies. Kill switch:
`CARTOG_SINGLE_WRITER=0`.

**Why.** A crashed primary must not strand the DB lock and leave clients without
a writer. Automatic failover restores write capability without user action.

**Limitations.**
- Failover takes up to the poll interval (~10 s); during that window the
  secondary is read-only and may serve slightly stale data.
- Only the primary writes; secondaries are read-only by construction.
- Disabling the election (`CARTOG_SINGLE_WRITER=0`) removes the single-owner
  guarantee — multiple primaries can then run on one DB.

**Impact.** ~10 s automatic failover. This is the one protocol where ad-hoc
reasoning is not trusted: it is specified in TLA+ (`specs/tla/`) and
model-checked with Loom (`cartog-loom-models`, `make loom`). Role flips use
`AtomicRole` with Acquire/Release ordering; mutex guards are held only for the
lock swap, never across `.await`.

---

## 6. MCP progress forwarder — `tokio::spawn` + bounded channel

**Where:** `crates/cartog-mcp/src/progress.rs`.

**Usage.** Long-running tools (`cartog_index`, `cartog_rag_index`) emit phase
events from their blocking task. A spawned async task drains a bounded mpsc
channel and forwards each event to the client as an MCP `notifications/progress`.

**Why.** It decouples the blocking indexer from async event dispatch, gives the
client live progress (`parsing M/N`, `resolving M/N edges`), and lets Ctrl-C
drain cleanly.

**Limitations.**
- Progress is **best-effort**: the channel is bounded (capacity 64) and
  producers use `try_send`, so under a stalled client some progress events are
  dropped (and logged) rather than backpressuring the indexer.
- Notifications are advisory — never rely on them for correctness.

**Impact.** Real-time progress with zero risk of a slow client throttling actual
indexing. The bounded-channel + `try_send` choice is the safety mechanism: a
consumer that can't keep up cannot stall the producer.

---

## 7. LSP client — reader thread + 64-request pipelining

**Where:** `crates/cartog-lsp/src/client.rs` (reader thread),
`crates/cartog-lsp/src/manager.rs` (`DEFINITION_BATCH_WINDOW = 64`).

**Usage.** Each LSP server is a child process. A background `std::thread` reads
JSON-RPC frames from the server's stdout into an mpsc channel. During edge
resolution the main thread writes `textDocument/definition` requests in
**windows of 64**, then collects all 64 replies before the next window.

**Why.** Sending requests one-at-a-time and blocking on each reply wastes the
round-trip latency. Pipelining a window overlaps those round-trips while capping
in-flight memory and stdin backpressure.

**Limitations.**
- The window size (64) is a fixed constant, not adaptive.
- Each batch shares one deadline; a slow/hung server is bounded by a timeout,
  and out-of-order or stale replies are matched by request id and dropped.
- This crate is **not** tokio — it is plain `std::thread` + channels.

**Impact.** ~33% faster edge resolution on large repos (≈5:12 → 3:29 on a 98k-edge
run). Safety: the reader thread exits cleanly on stdout EOF; the child LSP
process is reaped on `Drop`.

---

## 8. File watcher — `std::thread` + debouncer

**Where:** `crates/cartog-watch/src/lib.rs`.

**Usage.** `cartog watch` (and `cartog serve --watch`) spawn a named
`std::thread` running a debounced filesystem-event loop
(`notify-debouncer-mini`). Rapid edits are coalesced into a single re-index pass;
RAG re-embedding is deferred.

**Why.** FS syscalls block and a re-index can take seconds; running it on its own
thread keeps the CLI / daemon responsive. Debouncing avoids re-indexing on every
keystroke-save.

**Limitations.**
- Deliberately **no tokio** — it is pure `std::thread` + an mpsc channel.
- DB writes inside the loop are sequential; concurrent-writer contention is
  handled by SQLite `busy_timeout`, not by the watcher.
- `Drop` signals shutdown and joins with a bounded (~1.5 s) deadline; callers
  wanting a deterministic join call `stop()` explicitly.

**Impact.** Editor-save responsiveness: the index stays warm in the background
without blocking foreground work. Safety: an `Arc<AtomicBool>` shutdown flag plus
channel close coordinate a clean stop.

---

## 9. Fire-and-forget helpers — detached `std::thread`

**Where:** `crates/cartog/src/auto_check.rs` (update check),
`crates/cartog/src/commands/mod.rs` (CLI spinner).

**Usage.** Two small detached threads:
- **Update check** — a background HTTP call to the GitHub releases API so
  `cartog self update --check` adds no latency to normal commands.
- **CLI spinner** — animates progress frames / elapsed time while a foreground
  command runs.

**Why.** Neither should add latency to the main command. Both are pure
side-effects.

**Limitations.**
- The update check is **best-effort**: failures are swallowed; the state file
  update is not guaranteed, and the next eligible run retries.
- The spinner is cosmetic; it never affects results.

**Impact.** Zero added latency for the common path. Safety: each thread owns its
inputs (no shared mutable state beyond an `AtomicBool` stop flag and an
`Arc<Mutex<String>>` phase string for the spinner).

---

## 10. At a glance

| # | Site | Model | Why | Key impact |
|---|------|-------|-----|------------|
| 2 | File parsing | rayon `par_iter` | CPU-bound, files independent | ~6× / linear in cores |
| 3 | Model inference | ONNX intra-op threads | Tensor-op parallelism | 2–3× (quantized) + 30–50% padding-sort |
| 4 | MCP tool handlers | tokio `spawn_blocking` | Don't stall the async runtime | Concurrent tool calls; no lock across await |
| 5 | Single-writer promoter | tokio `spawn` | Auto-failover on primary death | ~10 s, TLA+/Loom verified |
| 6 | Progress forwarder | tokio `spawn` + bounded mpsc | Live progress, decoupled | Best-effort, never backpressures |
| 7 | LSP client | `std::thread` + 64-window | Overlap request round-trips | ~33% faster edge resolution |
| 8 | File watcher | `std::thread` + debouncer | Responsive background re-index | Editor-save latency hidden |
| 9 | Update check / spinner | detached `std::thread` | No latency on the main path | Cosmetic / best-effort |

## See also

- [incremental-indexing.md](incremental-indexing.md) — the change-detection
  pipeline that feeds the parallel parse phase (§2).
- [tech.md](../tech.md) — DB-mutex, WAL, and tokio decision rows (link back here).
- `specs/tla/` and `crates/cartog-loom-models` — formal models of the
  single-writer protocol (§5).
