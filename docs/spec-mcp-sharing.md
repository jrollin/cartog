# Feature: single-writer MCP with read-only attach and promotion

## Overview

When two cartog processes open the same `.cartog/db.sqlite` as writer (for
example, two Claude Code windows on the same project, or a terminal
`cartog serve` plus a separate `cartog watch`), they used to race on the
embedding-dimension migration during `Database::open` and one would crash
with `SQLITE_BUSY` → `MCP error -32000: Connection closed`. This spec
moves cartog to a **single-writer, multi-reader** model: the first
`cartog serve` for a DB wins an O_EXCL PID-file election and owns
indexing; later `cartog serve` instances attach **read-only**, skip
migrations, and serve 11 of 13 MCP tools. If the primary dies, a
background promoter on the secondary takes over.

## Architecture

```text
                          ┌─────────────────────────────────────────────┐
cartog serve   →   acquire_serve_lock(O_EXCL on .cartog/state/serve.pid)
                          │
              ┌───────────┴───────────┐
              │                       │
        ServeLockOutcome::         ServeLockOutcome::
        Primary(ProcessLock)       Held(ActiveLock)
              │                       │
              v                       v
        Role::Primary           Role::ReadOnly
        Database::open()        Database::open_readonly()
                                   pins (schema_version, fingerprint)
        spawn_watch (if --watch)   skip watcher
              │                       │
              │                       └──> spawn promoter task
              │                              (polls primary every 10s;
              │                              on death + pinned match:
              │                              O_EXCL acquire → swap to
              │                              open_existing_rw → spawn
              │                              watcher → role.store(Primary))
              v
        rmcp::serve(stdio()) with select! over
        service.waiting() + SIGINT + SIGTERM
        (Drop on ProcessLock unlinks the PID file on clean exit)
```

### Key Design Decisions

1. **Atomic O_EXCL election.** `ProcessLock::acquire` writes the PID payload to a per-(PID, thread, counter) temp file, then `fs::hard_link`s it into the slot path. `hard_link` fails atomically with `AlreadyExists` if the target exists, and the target inode is fully written before becoming visible — concurrent readers never see an empty file (the pre-fix `create_new(true)` + `write_all` had a transient empty-target window). Two simultaneous acquires by different live processes see exactly one winner. The loser's `AlreadyExists` triggers a holder inspection — if the holder is the same live process (`is_same_process`), election lost; otherwise the file is stale and we retry once.

2. **PID-reuse safety via start_time.** PID files are now two lines (`pid\n<start_time>`). `start_time` is platform-native (`/proc/<pid>/stat` field 22 on Linux, `proc_pidinfo PROC_PIDTBSDINFO` on macOS, `GetProcessTimes` on Windows). `is_same_process` rejects a recycled PID whose start time differs. Old single-line files still parse and fall back to `is_alive`-only checks; the next acquire rewrites them in the new format.

3. **Read-only attach via `SQLITE_OPEN_READ_ONLY`.** `Database::open_readonly(path)` skips all migrations and the embedding fingerprint reconcile, captures `metadata` snapshot as `PinnedAttach`, and validates `schema_version`. Writes against the read-only handle fail with `SQLITE_READONLY` as defense-in-depth.

4. **Tool gating, not transport split.** Same MCP stdio transport for both roles. The two write tools (`cartog_index`, `cartog_rag_index`) early-return a clear error when role is `ReadOnly`. The other 11 (search, outline, refs, callees, impact, hierarchy, deps, stats, map, changes, rag_search) work unchanged. `cartog_stats` JSON includes `"role": "primary" | "read-only"` for introspection.

5. **Promotion under the existing `Mutex<Database>`.** When the secondary's promoter detects the primary died, validates pinned state, and wins an O_EXCL acquire, it swaps the inner `Database` value under the mutex. No reader can be mid-query through the same guard (it must release before the promoter can claim it), so SQLITE_MISUSE from a connection-mid-use is impossible. The promoter then spawns the watcher (with `skip_pid_lock=true` so the watcher doesn't re-race the slot we already own, and `skip_migrations=true` via `Database::open_existing_rw` so we don't re-trigger the migration race the election prevents) and flips `Arc<AtomicRole>` to `Primary`.

6. **Migration-race safety net.** `handle_embedding_dimension` writes wrap in `retry_busy` on `SQLITE_BUSY` / `SQLITE_LOCKED` with 50/100/250/500/1000ms backoff. A true early-return on already-matching dimension means same-dim reopens never take a write lock at all. Layered defense for any case where two writers reach migration despite election (TOCTOU window, kill switch enabled).

7. **Embedding fingerprint, not just dimension.** Metadata records `embedding_provider` + `embedding_model` + `embedding_dimension`. Switching providers or models at the same dimension (previously silent corruption — searches returned garbage similarity scores against vectors from a different model) now wipes the vector index. Older DBs with only `embedding_dimension` get a one-time backfill on first open.

8. **Lifecycle hardening.** `cartog-mcp::run_server` selects over `service.waiting()`, `tokio::signal::ctrl_c`, and a per-OS `wait_for_sigterm` (Unix `SignalKind::terminate`, Windows `ctrl_close` / `ctrl_shutdown`). All branches return normally so `ProcessLock::Drop` unlinks the PID file. No `std::process::exit` inside signal handlers.

9. **Quieter logs under MCP.** Tracing default level is `warn` when `stderr` is not a TTY (MCP child, piped CI). Foreground TTY users still get `info`-level progress. `RUST_LOG` override still wins. Removes the false `[ERROR]` rendering of `info!` lines in `~/.claude/debug/*.txt`.

10. **Kill switch.** `CARTOG_SINGLE_WRITER=0` falls back to the pre-Phase-2 overwrite-on-acquire behavior. Phase 6a's busy-retry remains the only defense in that mode.

## Functional Requirements

### FR-001: Election
When `cartog serve` starts, the system shall attempt `ProcessLock::acquire` against `<state_dir>/serve.pid` with O_EXCL semantics, returning `Primary` on success, `Held(ActiveLock)` when a live peer (same PID + start_time) owns the slot, and `Io(error)` for filesystem failures.

### FR-002: Stale-lock cleanup
While `ProcessLock::acquire` sees `AlreadyExists` and the recorded holder is no longer alive (PID gone, or start_time mismatch from PID reuse), the system shall unlink the stale file and retry the acquire once.

### FR-003: Read-only attach
When `acquire_serve_lock` returns `Held`, the system shall open the DB via `Database::open_readonly` (no migrations), capture `PinnedAttach` from `metadata`, and continue running as a read-only secondary.

### FR-004: Schema-drift detection at attach
While the binary's compile-time `SCHEMA_VERSION` differs from the stored `schema_version` on disk, the system shall return `DbError::SchemaDrift` from `open_readonly`, exit cleanly, and log an actionable message ("another cartog process upgraded the schema; restart this session").

### FR-005: Tool gating
When a write tool (`cartog_index`, `cartog_rag_index`) is invoked under role `ReadOnly`, the system shall return an MCP error explaining why and pointing at the primary's automatic indexing.

### FR-006: Role surfaced in stats
When `cartog_stats` is invoked, the system shall include `"role": "primary"` or `"role": "read-only"` in the JSON response.

### FR-007: Promoter task
While role is `ReadOnly` and the primary's `ActiveLock` is known, the system shall spawn a background task that polls primary liveness every 10 seconds via `is_same_process(pid, start_time)`.

### FR-008: Promotion
When the promoter detects the primary is gone AND `validate_pinned_state` confirms the on-disk schema and fingerprint match the attach-time `PinnedAttach`, the system shall:
- attempt an atomic O_EXCL acquire of the serve slot;
- on success, open `Database::open_existing_rw` and swap it into the existing `Arc<Mutex<Database>>` under the held guard;
- spawn a watcher with `skip_pid_lock=true` and `skip_migrations=true` if the user invoked `serve --watch`;
- store `Role::Primary` in `AtomicRole`;
- exit the promoter loop.

### FR-009: Promotion abort on drift
While `validate_pinned_state` reports the on-disk schema_version or embedding fingerprint differs from the pin, the system shall log the divergence and exit the promoter loop without acquiring the lock.

### FR-010: Promotion race
While multiple read-only clients reach promotion concurrently, exactly one shall win the O_EXCL acquire; the losers shall stay read-only and re-poll on the next 10-second tick.

### FR-011: Lifecycle hardening
When `cartog-mcp::run_server` is running, the system shall return from `run_server` cleanly on any of (a) rmcp's `service.waiting()` returning, (b) SIGINT, (c) SIGTERM (Unix) / `ctrl_close` / `ctrl_shutdown` (Windows), without calling `std::process::exit` so that `ProcessLock::Drop` unlinks the PID file.

### FR-012: Log level autodetect
When `cartog serve`, `cartog watch`, or `cartog rag index` is invoked and `stderr` is not a TTY, the system shall default the tracing level to `warn` instead of `info`. `RUST_LOG` shall override this default in either direction.

### FR-013: Embedding fingerprint reconcile
When a writer opens the DB and the requested `(provider, model, dimension)` differs from any stored field, the system shall drop `symbol_vec`, clear `symbol_embedding_map`, recreate the vec schema at the new dimension, and write all three fields to `metadata`. When stored has `embedding_dimension` but no provider/model (older DBs), the system shall backfill provider+model without wiping.

### FR-014: Migration busy-retry
While `handle_embedding_dimension` issues a write that returns `SQLITE_BUSY` or `SQLITE_LOCKED`, the system shall retry with exponential backoff (50/100/250/500/1000ms, max ~2s total) before propagating the error.

### FR-015: Migration true early return
When `handle_embedding_dimension` finds the stored dimension already matches the effective dimension, the system shall return without any DB writes (not even idempotent CREATE/INSERT-OR-REPLACE).

### FR-016: Kill switch
While `CARTOG_SINGLE_WRITER=0` is set in the environment, the system shall use `ProcessLock::acquire_overwriting` (legacy write+rename) instead of O_EXCL, restoring pre-Phase-2 behavior. The migration busy-retry remains active.

### FR-017: Watcher refuses on lock-held
While `cartog watch` starts and another live cartog process holds the watch slot, the system shall refuse to start with a clear message and a non-zero exit code (no read-only mode for the watcher).

## Non-Functional Requirements

- **NFR-001: Polling overhead.** Promoter polling shall cost less than 1ms per 10-second tick (one syscall per platform: `kill(pid, 0)` + a stat of /proc/<pid>/stat on Linux, `proc_pidinfo` on macOS, `OpenProcess` + `GetProcessTimes` on Windows).
- **NFR-002: Cross-platform.** Every change shall pass `make check` on Linux, macOS, and Windows targets in CI. `process_start_time` returns `None` on unsupported platforms; PID-reuse detection degrades to "always trust PID" rather than failing.
- **NFR-003: Backwards compatibility.** Older single-line PID files (`pid` only, no start_time) shall parse and be honored via `is_alive`-only checks. Older DBs with only `embedding_dimension` (no provider/model in metadata) shall be backfilled without wiping.
- **NFR-004: Test isolation.** Tests that mutate `CARTOG_SINGLE_WRITER` shall serialize via a process-wide mutex (the env var is process-global; cargo test runs in parallel by default).

## Acceptance Criteria

1. Opening two `cartog serve` instances against the same DB in two terminals (or two Claude Code windows on the same project) results in:
   - First instance: `Role::Primary`, all 13 tools work, watcher running if requested.
   - Second instance: `Role::ReadOnly`, 11 read tools work, both write tools return the gated error message naming the primary's PID.
2. When the primary process is killed (`kill -9` on Unix; close console on Windows), the secondary's promoter detects this within ~10s and:
   - Validates `PinnedAttach` matches on-disk state.
   - Acquires the O_EXCL `serve` lock.
   - Swaps to RW DB via `open_existing_rw` (no migration replay).
   - Spawns the watcher if `serve --watch` was requested.
   - Logs `"promoted to primary"`.
   - All 13 tools then work on what was the secondary.
3. Two readers race for promotion: exactly one wins, the loser stays read-only, no `SQLITE_BUSY` on the new primary's `open_existing_rw`.
4. Promoter aborts cleanly when a third writer upgraded the schema or swapped the embedding stack between the secondary's attach and the primary's death.
5. `kill -INT` or `kill -TERM` on a primary serve unlinks `serve.pid` before exit; the PID file is gone when the process is gone.
6. `cartog serve` stderr in MCP-child mode no longer emits `info!` lines as `[ERROR]` in `~/.claude/debug/*.txt`. `cartog serve` in a foreground terminal still shows `info!` progress.
7. `CARTOG_SINGLE_WRITER=0 cartog serve` reproduces the pre-Phase-2 overwrite-on-acquire behavior; two such processes both report `Primary` and the migration busy-retry catches any race.

## Implementation Checklist

All items completed across the initial 8 implementation commits
(`a75dec0` … `24c5c70`) plus 5 follow-up review-fix commits
(`949ac43`, `1a563f7`, `f66b796`, `9e59f5b`, `4d876b1`) — see the
"Review fixes" section at the end for the per-issue mapping.

### Phase 6a: migration busy-retry (`crates/cartog-db/src/lib.rs`)
- [x] `MIGRATION_RETRY_BACKOFF_MS` schedule constant
- [x] `retry_busy(op)` helper retrying on `SQLITE_BUSY` / `SQLITE_LOCKED`
- [x] `handle_embedding_dimension` writes wrapped in `retry_busy`
- [x] True early-return when stored == effective (no writes)
- [x] 4 unit tests (retry success, non-busy passthrough, exhaustion, no-write reopen)

### Phase 6b: embedding fingerprint (`crates/cartog-rag/`, `cartog-db`)
- [x] `EmbeddingProvider::model_id` trait method, impls in `local`, `ollama`, mocks
- [x] `EmbeddingFingerprint { provider, model, dimension }` type
- [x] `Database::reconcile_embedding_fingerprint(&fp)` wipes on mismatch, backfills on legacy DBs
- [x] `rag::fingerprint_of(&dyn EmbeddingProvider)` builder
- [x] 3 call sites reconcile post-open (watcher, `cartog rag index`, MCP `CartogServer::new`). `cartog rag search` deliberately does NOT reconcile: it's a read path; a fingerprint mismatch yields lower-quality results but never wipes data. See commit `f66b796`.
- [x] 5 unit tests (match no-op, provider swap, model swap, backfill, dim change)

### Phase 1: lifecycle + start_time + log levels
- [x] `cartog-process-lock::start_time` module (Linux `/proc/<pid>/stat`, macOS `proc_pidinfo`, Windows `GetProcessTimes`)
- [x] `process_start_time(pid) -> Option<u64>` cross-platform
- [x] `is_same_process(pid, start_time)`
- [x] 2-line PID file format (`pid\n<start_time>`), backwards-compatible parser
- [x] `ActiveLock` gains `start_time: Option<u64>`
- [x] `find_active_locks` uses `is_same_process` when start_time present
- [x] SIGTERM/SIGINT handler in `run_server` via `tokio::select!`, no `process::exit`
- [x] `wait_for_sigterm` per OS (Unix `SignalKind::terminate`; Windows `ctrl_close`+`ctrl_shutdown`)
- [x] Log level autodetect via `IsTerminal::is_terminal()` on stderr in `main.rs`
- [x] `tokio` workspace `signal` feature added

### Phase 2: O_EXCL election + kill switch
- [x] `AcquireError::{Held(ActiveLock), Io(io::Error)}`
- [x] `ProcessLock::acquire` writes a temp file then `hard_link`s it into the slot path (O_EXCL semantics via `hard_link`'s `AlreadyExists` failure mode — see review-fix C3 below for the migration from the original `OpenOptions::create_new(true)` path). Inspects holder on `AlreadyExists`, unlinks stale + retries once.
- [x] `ProcessLock::acquire_overwriting` (legacy path for kill switch)
- [x] `ServeLockOutcome::{Untracked, Primary, Held}` in `cartog-mcp`
- [x] `SINGLE_WRITER_ENV = "CARTOG_SINGLE_WRITER"` constant; `single_writer_election_enabled()` reads it
- [x] `acquire_serve_lock` branches on outcome
- [x] `cartog watch` refuses on Held with clear message

### Phase 3: read-only DB mode
- [x] `DbError::SchemaDrift { expected, stored }`
- [x] `PinnedAttach { schema_version, embedding: Option<EmbeddingFingerprint> }`
- [x] `Database::open_readonly(path)` using `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`
- [x] Skips migrations + fingerprint reconcile, pins metadata snapshot, errors on schema drift
- [x] `Database::is_read_only()` + `pinned_attach()` accessors
- [x] 6 unit tests (succeeds + flagged, query works, write refused, schema drift, no migrations, RW open not read-only)

### Phase 4: read-only MCP tool gating
- [x] `Role::{Primary, ReadOnly}` enum
- [x] `CartogServer::new_read_only(...)` constructor
- [x] `refuse_if_read_only(tool)` helper returning clear `McpError`
- [x] `cartog_index` and `cartog_rag_index` early-return refusal in ReadOnly
- [x] `cartog_stats` JSON includes `"role": ...`
- [x] `run_server` branches on `ServeLockOutcome::Held` to construct `new_read_only`
- [x] Skips watcher spawn for ReadOnly role
- [x] 3 unit tests (primary role, read-only role, refusal both tools)

### Phase 5: promotion
- [x] `Database::open_existing_rw(path)` (full RW + PRAGMAs + schema-drift check, no migrations)
- [x] `WatchConfig::skip_pid_lock` + `skip_migrations` flags
- [x] `cartog-watch` watch loop honors both flags
- [x] `AtomicRole` wrapping `AtomicU8`; `CartogServer.role: Arc<AtomicRole>`
- [x] `PromoterArgs` struct + `promoter_task` async function
- [x] Polls every 10s, validates pinned state, atomic O_EXCL acquire, swaps DB under Mutex, spawns watcher, flips role
- [x] `validate_pinned_state(path, &PinnedAttach)` re-reads via `open_readonly`
- [x] 3 unit tests (pinned-match validates, schema bump detected, AtomicRole round-trip)

### Cross-cutting
- [x] All 6 phases land as separate commits on the feature branch
- [x] `make check` (fmt + clippy + tests + fixtures) passes after each phase
- [x] Total test count: 812 (was 760 at branch creation; +52 new tests after fixes)

### Review fixes (post-spec, second adversarial review)

Five follow-up commits address issues found by a multi-agent review of
the initial implementation. Each commit ships with at least one
regression test.

- [x] `949ac43 fix(serve,rag)`:
  - C1: `LocalEmbeddingProvider::model_id` now returns `model_info.model_code` (stable HF path, e.g. `Qdrant/bge-small-en-v1.5-onnx-Q`) instead of fastembed's `Display` impl (Debug repr of the enum variant).
  - C2: `find_active_locks` re-reads the PID file via `unlink_if_unchanged` before removal; closes a TOCTOU window where a concurrent acquire could be clobbered by `cartog self update`'s peer scan.
  - C3: `ProcessLock::acquire` uses `write_tmp` + `hard_link` instead of `OpenOptions::create_new(true)` + `write_all`; the target is never observed in an empty state. Per-(PID, thread, counter) tmp filenames prevent intra-process clobbering.
- [x] `1a563f7 fix(db)`:
  - C4: `handle_embedding_dimension` and `reconcile_embedding_fingerprint` wrap their multi-statement writes in `unchecked_transaction`; mid-sequence failures roll back atomically.
  - M-schema: `open_readonly` and `open_existing_rw` route through a `read_schema_version` helper that treats a missing row as `stored=0` → `DbError::SchemaDrift`, not raw `Sqlite(QueryReturnedNoRows)`.
- [x] `f66b796 fix(rag)`: `cmd_rag_search` no longer calls `reconcile_embedding_fingerprint`. Search is read-only by nature; a fingerprint change yields poorer results but never wipes data or races a primary serve.
- [x] `9e59f5b fix(mcp)`: Five promoter improvements:
  - (a) Role flips to Primary right after the DB swap, before spawning the watcher.
  - (b) Transient `open_existing_rw` failures drop the lock and continue the loop (was: return forever).
  - (c) Re-validate pinned state AFTER acquiring the lock (was: only before, leaving a TOCTOU window).
  - (d) Promoter `JoinHandle` is aborted in `run_server`'s shutdown path (was: dropped, which does not cancel).
  - (e) Watcher post-promotion reuses the server's captured `cwd` (was: `std::env::current_dir()`).
  - `poll_interval` becomes a `PromoterArgs` field so tests can shrink it from 10s to milliseconds.
- [x] `4d876b1 docs`: removed unsafe "manually delete `watch.pid`" advice; clarified log-level autodetect; documented per-OS `<state_dir>` paths.

## Out of Scope

- **Unix-socket / TCP MCP transport.** Each cartog writer keeps its own DB connection (RO or RW per role); no proxy/forwarding code.
- **Daemonizing cartog.** No `setsid` / double-fork. Cartog stays attached to its parent's process group; lifecycle hardening relies on parent-driven shutdown signals.
- **`--log-file` / `--log-level` CLI flags.** Log-level autodetect by mode is in scope; explicit flags are a follow-up.
- **`cartog status` command exposing role/lock state.** `cartog_stats` MCP tool surfaces the role; a dedicated CLI command is a follow-up.
- **`cartog index` / `cartog rag index` CLI election.** Only `cartog serve` and `cartog watch` participate in election today. CLI one-shots rely on Phase 6a's busy-retry. Future: every CLI writer could check the lock and refuse / queue.
- **`notify` watcher on `serve.pid` for sub-second handoff.** Current 10-second poll is sufficient given typical primary-death gaps (Cmd-Q, crash, kill). Adding `notify` would let us react faster but doubles the dependency surface for a marginal UX gain.
