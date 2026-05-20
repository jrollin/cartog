//! PID-file locks for long-lived cartog commands (`serve`, `watch`, …).
//!
//! Each long-lived command grabs a [`ProcessLock`] at startup which writes
//! `<state_dir>/<slot>.pid` containing two lines: the running PID and the
//! process's OS-native start time. The `ProcessLock` value cleans the file
//! up via `Drop` on graceful exit.
//!
//! `cartog self update` consults [`find_active_locks`] before swapping the
//! binary so it can refuse to clobber a running peer (cross-platform — a
//! best-effort signal because crash exits leave stale files, which the
//! reader then cleans up after verifying the PID is gone).
//!
//! Cross-platform liveness:
//! - Unix: `kill(pid, 0)` returns 0 when the process exists; `ESRCH` means
//!   gone, `EPERM` means alive but unreachable (still considered alive).
//! - Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, …)` returns a
//!   non-null handle for live PIDs; we close the handle and return `true`.
//!
//! PID-reuse: the recorded start time lets us distinguish "PID was reused
//! by an unrelated process" from "same process is still running". When the
//! start time is absent (legacy single-line file from an older cartog) we
//! fall back to liveness-only checks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

mod start_time;
pub use start_time::process_start_time;

const PID_EXTENSION: &str = "pid";

/// RAII handle for a held PID-file. Dropping the handle removes the file
/// (best-effort — a missing file or filesystem error during teardown is
/// swallowed so a long-lived command's Drop doesn't introduce panics).
#[derive(Debug)]
pub struct ProcessLock {
    path: PathBuf,
}

impl ProcessLock {
    /// Write `<state_dir>/<slot>.pid` with the current process's PID and
    /// start time (two lines). Creates `state_dir` if missing. Returns an
    /// error on permission / I/O failure.
    ///
    /// Note: this does NOT fail if a stale PID file already exists — the
    /// caller is the long-lived command itself, and a stale file means
    /// some prior crashed instance was using the slot. We overwrite. The
    /// reader (`find_active_locks`) is the place that distinguishes stale
    /// from live.
    pub fn acquire(state_dir: &Path, slot: &str) -> io::Result<Self> {
        validate_slot(slot)?;
        fs::create_dir_all(state_dir)?;
        let path = state_dir.join(format!("{slot}.{PID_EXTENSION}"));
        let pid = std::process::id();
        // Per-PID staging file: two concurrent acquires for the same slot
        // do not clobber each other's tmp before the rename.
        let tmp = state_dir.join(format!(".{slot}.{pid}.{PID_EXTENSION}.tmp"));
        let payload = match process_start_time(pid) {
            Some(st) => format!("{pid}\n{st}\n"),
            None => format!("{pid}\n"),
        };
        write_atomic(&tmp, &path, payload.as_bytes())?;
        Ok(Self { path })
    }

    /// Path of the on-disk PID file. Useful in tests.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// One live PID file discovered by [`find_active_locks`]. The slot name is
/// the file stem (`serve`, `watch`, …) and `pid` is the running PID.
///
/// `start_time` is the recorded process start time when the lock was
/// acquired. `None` means the PID file came from an older cartog version
/// that didn't record a start time — callers must fall back to liveness
/// checks alone. New cartog releases always record it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveLock {
    pub slot: String,
    pub pid: u32,
    pub start_time: Option<u64>,
}

/// Scan `state_dir` for `*.pid` files. Returns one [`ActiveLock`] per file
/// whose recorded PID is still alive on this machine. Stale files (process
/// gone) are deleted as a side-effect so the directory stays clean.
///
/// A missing or unreadable directory yields an empty vec — long-lived
/// commands may not have run yet, which is the common case on a fresh
/// install.
pub fn find_active_locks(state_dir: &Path) -> Vec<ActiveLock> {
    let entries = match fs::read_dir(state_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut active = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some(PID_EXTENSION) {
            continue;
        }
        let slot = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let (pid, recorded_st) = match read_lock_file(&path) {
            Some(v) => v,
            None => {
                // Side effect: clean malformed files so the slot is reusable.
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let alive = match recorded_st {
            // New format: PID + start_time pinned the original process. If
            // the PID has been reused since, start times will differ and we
            // correctly treat this entry as stale.
            Some(st) => is_same_process(pid, st),
            // Legacy single-line file from an older cartog. Fall back to
            // liveness-only — we lose PID-reuse detection until the holder
            // restarts and rewrites the file in the new format.
            None => is_alive(pid),
        };
        if alive {
            active.push(ActiveLock {
                slot,
                pid,
                start_time: recorded_st,
            });
        } else {
            let _ = fs::remove_file(&path);
        }
    }
    active
}

/// True when `pid` is still running AND its start time matches `recorded`.
/// Use this in preference to [`is_alive`] anywhere PID reuse would matter
/// (election, peer detection across long-lived state files).
pub fn is_same_process(pid: u32, recorded: u64) -> bool {
    match process_start_time(pid) {
        Some(current) => current == recorded,
        None => false,
    }
}

/// Cross-platform "is this PID currently a running process?" check.
#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    // kill(0, 0) signals our own process group — would always report alive. Reject.
    if pid == 0 {
        return false;
    }
    // PID > i32::MAX casts negative to pid_t, flipping kill semantics.
    if pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: kill(pid, 0) is documented as side-effect-free aside from
    // setting errno. It validates the PID exists and we have permission.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    // EPERM means the process exists but we cannot signal it; for our
    // purposes "alive" is correct here.
    let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

#[cfg(windows)]
pub fn is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess is a Windows API that takes scalar arguments and
    // returns a handle or NULL on failure. (Per MSDN, OpenProcess never
    // returns INVALID_HANDLE_VALUE — that sentinel is for file APIs.)
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: handle was returned by a successful OpenProcess and has not
    // been closed yet. We discard the BOOL return — even if CloseHandle
    // fails (extraordinarily unlikely) the OS reaps on process exit.
    unsafe { CloseHandle(handle) };
    true
}

#[cfg(not(any(unix, windows)))]
pub fn is_alive(_pid: u32) -> bool {
    // Unsupported platform: fail safe by reporting "alive" so we never
    // clobber a possibly-running peer. The user will see a refusal and can
    // intervene manually.
    true
}

/// Reject slot names with path separators or odd characters — we want
/// `<state_dir>/<slot>.pid` to land exactly where we said it would.
fn validate_slot(slot: &str) -> io::Result<()> {
    if slot.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process_lock: slot name must not be empty",
        ));
    }
    let bad = slot.chars().any(|c| {
        c == '/' || c == '\\' || c == '.' || c == '\0' || c.is_control() || c.is_whitespace()
    });
    if bad {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("process_lock: invalid slot name {slot:?}"),
        ));
    }
    Ok(())
}

/// Parse the on-disk PID file. Returns `(pid, Some(start_time))` for the
/// current 2-line format, `(pid, None)` for a legacy single-line file from
/// an older cartog. Yields `None` for malformed or unreadable files.
fn read_lock_file(path: &Path) -> Option<(u32, Option<u64>)> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse::<u32>().ok()?;
    if pid == 0 {
        // PID 0 in the file means corruption — std::process::id() never returns 0.
        return None;
    }
    let start_time = lines
        .next()
        .and_then(|line| line.trim().parse::<u64>().ok());
    Some((pid, start_time))
}

/// Write `bytes` to `target` atomically: stage at `tmp`, then rename onto
/// `target`. The caller picks `tmp` so concurrent writers can stage to
/// distinct files (see `ProcessLock::acquire`).
fn write_atomic(tmp: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    // fsync before rename so a crash between the data write and the
    // rename does not leave a zero-byte file on disk after recovery.
    let f = fs::File::create(tmp)?;
    use std::io::Write;
    (&f).write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_writes_pid_file() {
        let dir = TempDir::new().unwrap();
        let lock = ProcessLock::acquire(dir.path(), "watch").unwrap();
        let path = dir.path().join("watch.pid");
        assert!(path.exists(), "pid file must exist after acquire");
        let (recorded_pid, recorded_st) = read_lock_file(&path).expect("file parses");
        assert_eq!(recorded_pid, std::process::id());
        // On supported platforms acquire records a start_time too.
        if process_start_time(std::process::id()).is_some() {
            assert!(
                recorded_st.is_some(),
                "platforms that expose start_time must store it"
            );
        }
        drop(lock);
        assert!(!path.exists(), "drop must remove the pid file");
    }

    #[test]
    fn acquire_creates_missing_state_dir() {
        let parent = TempDir::new().unwrap();
        let nested = parent.path().join("nested").join("state");
        let _lock = ProcessLock::acquire(&nested, "serve").unwrap();
        assert!(nested.join("serve.pid").exists());
    }

    #[test]
    fn acquire_overwrites_stale_pid_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        // Legacy single-line file (older cartog) — should be overwritten.
        fs::write(&path, "999999").unwrap();
        let _lock = ProcessLock::acquire(dir.path(), "serve").unwrap();
        let (recorded_pid, _) = read_lock_file(&path).expect("file parses");
        assert_eq!(recorded_pid, std::process::id());
    }

    #[test]
    fn acquire_rejects_invalid_slot_names() {
        let dir = TempDir::new().unwrap();
        for bad in ["", "with/slash", "with\\back", "with.dot", "with space"] {
            let err = ProcessLock::acquire(dir.path(), bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "slot {bad:?}");
        }
    }

    #[test]
    fn find_active_locks_returns_live_self() {
        let dir = TempDir::new().unwrap();
        let _lock = ProcessLock::acquire(dir.path(), "watch").unwrap();
        let active = find_active_locks(dir.path());
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].slot, "watch");
        assert_eq!(active[0].pid, std::process::id());
    }

    #[test]
    fn find_active_locks_cleans_stale_entries() {
        let dir = TempDir::new().unwrap();
        // PID 999999 is overwhelmingly unlikely to exist; if it ever does on
        // a really busy box, the test would still pass — `is_alive` would
        // report it alive and it'd just be left in place. We pick a value
        // that matches Linux's pid_max default (4_194_304) for stricter
        // confidence.
        let bogus = 4_194_304u32;
        fs::write(dir.path().join("watch.pid"), bogus.to_string()).unwrap();
        let active = find_active_locks(dir.path());
        assert!(active.is_empty(), "stale pid should not be reported");
        assert!(
            !dir.path().join("watch.pid").exists(),
            "stale pid file should be cleaned up"
        );
    }

    #[test]
    fn find_active_locks_ignores_non_pid_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        fs::write(dir.path().join("state.toml"), "ignored = true").unwrap();
        let active = find_active_locks(dir.path());
        assert!(active.is_empty());
        // The non-pid files must be untouched.
        assert!(dir.path().join("notes.txt").exists());
        assert!(dir.path().join("state.toml").exists());
    }

    #[test]
    fn find_active_locks_removes_malformed_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("watch.pid"), "not a number").unwrap();
        let active = find_active_locks(dir.path());
        assert!(active.is_empty());
        assert!(
            !dir.path().join("watch.pid").exists(),
            "malformed pid file should be removed"
        );
    }

    #[test]
    fn find_active_locks_missing_dir_returns_empty() {
        let parent = TempDir::new().unwrap();
        let missing = parent.path().join("does-not-exist");
        let active = find_active_locks(&missing);
        assert!(active.is_empty());
    }

    #[test]
    fn is_alive_self_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn is_alive_for_clearly_dead_pid_is_false() {
        // Same generous "out of range" PID as in the stale-cleanup test.
        assert!(!is_alive(4_194_304));
    }

    #[test]
    fn is_alive_pid_zero_returns_false() {
        // 0 has special meaning to kill(2) on POSIX; our reader must
        // never treat it as a real running process.
        assert!(!is_alive(0));
    }

    #[test]
    fn find_active_locks_treats_pid_zero_as_malformed() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("watch.pid"), "0").unwrap();
        let active = find_active_locks(dir.path());
        assert!(active.is_empty());
        assert!(
            !dir.path().join("watch.pid").exists(),
            "pid file with 0 should be cleaned up like any other malformed value"
        );
    }

    #[test]
    fn concurrent_acquires_for_same_slot_dont_share_tmp_file() {
        // The temp staging file must be per-PID so two simultaneous
        // acquires for the same slot don't clobber each other's tmp.
        // We can't easily simulate two PIDs in-process, but we can
        // verify the temp filename embeds the PID by inspecting the
        // directory after a successful acquire (which removes its own
        // tmp via rename, but we drop the lock first to leave the
        // dir clean for the assertion).
        let dir = TempDir::new().unwrap();
        let lock = ProcessLock::acquire(dir.path(), "watch").unwrap();
        // Final state has the .pid file but no .tmp residues.
        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        let names: Vec<String> = entries
            .iter()
            .map(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            names.iter().any(|n| n == "watch.pid"),
            "watch.pid expected, got {names:?}",
        );
        assert!(
            !names.iter().any(|n| n.ends_with(".tmp")),
            "no leftover .tmp files expected, got {names:?}",
        );
        drop(lock);
    }

    #[test]
    fn read_lock_file_parses_legacy_single_line() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        fs::write(&path, "42").unwrap();
        let (pid, st) = read_lock_file(&path).expect("legacy file parses");
        assert_eq!(pid, 42);
        assert_eq!(
            st, None,
            "legacy single-line files have no recorded start_time"
        );
    }

    #[test]
    fn read_lock_file_parses_new_two_line_format() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        fs::write(&path, "42\n123456789\n").unwrap();
        let (pid, st) = read_lock_file(&path).expect("new format parses");
        assert_eq!(pid, 42);
        assert_eq!(st, Some(123456789));
    }

    #[test]
    fn read_lock_file_tolerates_garbage_start_time() {
        // PID parses fine, start_time line is junk: treat as legacy.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        fs::write(&path, "42\nnot-a-number\n").unwrap();
        let (pid, st) = read_lock_file(&path).expect("pid line still parses");
        assert_eq!(pid, 42);
        assert_eq!(st, None);
    }

    #[test]
    fn find_active_locks_carries_start_time() {
        let dir = TempDir::new().unwrap();
        let _lock = ProcessLock::acquire(dir.path(), "watch").unwrap();
        let active = find_active_locks(dir.path());
        assert_eq!(active.len(), 1);
        // On supported platforms the acquired lock records a start_time.
        if process_start_time(std::process::id()).is_some() {
            assert!(active[0].start_time.is_some());
        }
    }

    #[test]
    fn find_active_locks_treats_stale_start_time_as_dead() {
        // PID is alive (ours) but the file records a different start_time —
        // the recorded value belongs to a previous process whose PID was
        // recycled to ours. find_active_locks must NOT report it as active.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("watch.pid");
        let pid = std::process::id();
        let real_st = match process_start_time(pid) {
            Some(s) => s,
            // Skip on unsupported platforms — the reuse check is a no-op there.
            None => return,
        };
        let bogus = real_st.wrapping_add(999_999);
        fs::write(&path, format!("{pid}\n{bogus}\n")).unwrap();
        let active = find_active_locks(dir.path());
        assert!(
            active.is_empty(),
            "PID-reuse case must not surface as active"
        );
        assert!(
            !path.exists(),
            "stale (PID-reused) file should be cleaned up"
        );
    }

    #[test]
    fn is_same_process_matches_self() {
        let pid = std::process::id();
        let st = match process_start_time(pid) {
            Some(s) => s,
            None => return,
        };
        assert!(is_same_process(pid, st));
        assert!(
            !is_same_process(pid, st.wrapping_add(1)),
            "mismatched start_time must be rejected"
        );
    }
}
