# cartog-process-lock

Cross-platform PID-file locks for long-lived cartog commands.

## Overview

Long-lived commands (`cartog serve`, `cartog watch`) claim a named *slot* at
startup by writing a PID file under the cartog state directory. Before swapping
the binary, `cartog self update` consults these files to refuse clobbering a
running peer. The lock is best-effort: a crash leaves a stale file behind, which
the next reader cleans up after verifying the recorded PID is gone.

## How it works

### Slot / PID-file format

Each slot maps to `<state_dir>/<slot>.pid`. The file holds two lines:

```
<pid>
<os-native-start-time>
```

The recorded start time lets the reader distinguish "same process still running"
from "PID was recycled by an unrelated process". Legacy single-line files (PID
only) from older cartog versions are still parsed; they fall back to
liveness-only checks until the holder restarts and rewrites the file.

### Atomic acquire

`ProcessLock::acquire(state_dir, slot)` writes the payload to a per-process temp
file, `fsync`s it, then hard-links the temp onto the target. The link fails
atomically with `AlreadyExists` if the slot is taken, with no window where a
reader can observe a created-but-empty target. On `AlreadyExists` the holder is
inspected: a live holder returns `AcquireError::Held`; a stale one is unlinked
(guarding the TOCTOU window) and the acquire retries. Every acquire first sweeps
stale files so crashed-peer leftovers don't accumulate.

`acquire_overwriting` is the kill-switch path (`CARTOG_SINGLE_WRITER=0`): it
overwrites any existing file unconditionally and skips the election.

Dropping a `ProcessLock` removes its file (best-effort; teardown errors are
swallowed so it never panics).

### Cross-platform liveness

- **Unix**: `kill(pid, 0)` — `0` means alive, `ESRCH` means gone, `EPERM` means
  alive but unreachable (still treated as alive).
- **Windows**: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, …)` — a non-null
  handle means alive; the handle is closed immediately.
- **Other platforms**: fail safe by reporting "alive" so a possibly-running peer
  is never clobbered.

### Reaping vs. peer detection

`find_active_locks` is the correctness path for `cartog self update` and the
watch promoter, so missing a real peer would be unsafe. The opportunistic reaper
`sweep_stale_locks` is bounded per call to cap startup cost; entries beyond the
cap are reaped on a later run. Stale removals only delete a file when its content
still matches what was observed, so a fresh writer landing in the gap is preserved.

## Public API

| Export | Description |
|--------|-------------|
| `ProcessLock` | RAII handle for a held slot; `Drop` removes the PID file |
| `ProcessLock::acquire(state_dir, slot)` | Atomically claim a slot; `Err(Held)` if a live peer owns it |
| `ProcessLock::acquire_overwriting(state_dir, slot)` | Kill-switch acquire that always wins (no election) |
| `ProcessLock::path()` | Path of the on-disk PID file |
| `AcquireError` | `Held(ActiveLock)` (live peer owns the slot) or `Io(io::Error)` |
| `ActiveLock` | A discovered live lock: `slot`, `pid`, `start_time: Option<u64>` |
| `find_active_locks(state_dir)` | Scan a state dir for live locks; cleans stale files |
| `sweep_stale_locks(state_dir)` | Opportunistic stale-file reaper |
| `is_alive(pid)` | Cross-platform "is this PID a running process?" check |
| `is_same_process(pid, recorded)` | `is_alive` plus a start-time match (closes the PID-reuse window) |
| `process_start_time(pid)` | OS-native start time for a PID (`None` on unsupported platforms) |

## Crate dependencies

None internal. Platform: `libc` (Unix), `windows-sys` (Windows).
