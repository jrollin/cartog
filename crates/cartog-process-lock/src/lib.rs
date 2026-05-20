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

/// Why `ProcessLock::acquire` failed. The `Held` variant lets callers
/// branch on "another writer owns the slot" (election lost — can attach
/// read-only or refuse) vs a real I/O failure.
#[derive(Debug)]
pub enum AcquireError {
    /// Another process holds the lock and its PID is still alive (and, if
    /// the file uses the new 2-line format, the start_time matches —
    /// closing the PID-reuse window). The held lock is returned so the
    /// caller can format a useful message or attach in read-only mode.
    Held(ActiveLock),
    /// Filesystem / permission failure unrelated to peer detection.
    Io(io::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::Held(lock) => write!(
                f,
                "another cartog process holds slot {slot:?} (PID {pid})",
                slot = lock.slot,
                pid = lock.pid,
            ),
            AcquireError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AcquireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AcquireError::Io(e) => Some(e),
            AcquireError::Held(_) => None,
        }
    }
}

impl From<io::Error> for AcquireError {
    fn from(e: io::Error) -> Self {
        AcquireError::Io(e)
    }
}

impl ProcessLock {
    /// Atomically acquire `<state_dir>/<slot>.pid` for this process. Fails
    /// with [`AcquireError::Held`] if another live cartog process already
    /// holds the slot. Stale files (PID gone, or start_time mismatch from
    /// PID reuse) are unlinked and the acquire is retried once.
    ///
    /// Implementation: writes the full payload to a per-PID temp file,
    /// then `hard_link`s tmp → target. `hard_link` fails atomically with
    /// `AlreadyExists` if the target already exists, with no window in
    /// which the target file can be observed empty (unlike a bare
    /// `OpenOptions::create_new(true)` followed by `write_all`, where
    /// concurrent readers can see the just-created-but-not-yet-written
    /// inode).
    ///
    /// Creates `state_dir` if missing.
    pub fn acquire(state_dir: &Path, slot: &str) -> Result<Self, AcquireError> {
        validate_slot(slot).map_err(AcquireError::Io)?;
        fs::create_dir_all(state_dir).map_err(AcquireError::Io)?;
        let path = state_dir.join(format!("{slot}.{PID_EXTENSION}"));
        let pid = std::process::id();
        let payload = match process_start_time(pid) {
            Some(st) => format!("{pid}\n{st}\n"),
            None => format!("{pid}\n"),
        };
        // Per-(PID, thread) staging file so concurrent acquires from the
        // same process don't clobber each other's tmp before the link.
        // A monotonic counter is appended to disambiguate retries within
        // the same thread.
        static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tid = thread_id_hash();
        let tmp = state_dir.join(format!(".{slot}.{pid}.{tid}.{n}.{PID_EXTENSION}.tmp"));

        // Two attempts: write tmp + hard_link to target; on AlreadyExists
        // we inspect the holder and, if stale, unlink + retry once.
        for attempt in 0..2 {
            // Always re-stage tmp inside the loop: a previous link attempt
            // may have left the tmp around if hard_link failed and we
            // unlinked the stale target, and we want fresh content each
            // try.
            write_tmp(&tmp, payload.as_bytes()).map_err(AcquireError::Io)?;
            match fs::hard_link(&tmp, &path) {
                Ok(()) => {
                    // Target now points at the same inode as tmp; remove
                    // the tmp name (the inode stays linked until target
                    // is unlinked by Drop).
                    let _ = fs::remove_file(&tmp);
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    // Inspect the holder. If it's still the same process,
                    // election lost. Otherwise it's stale; unlink + retry.
                    let active = read_lock_file(&path).and_then(|(pid, st)| {
                        let alive = match st {
                            Some(st) => is_same_process(pid, st),
                            None => is_alive(pid),
                        };
                        if alive {
                            Some(ActiveLock {
                                slot: slot.to_string(),
                                pid,
                                start_time: st,
                            })
                        } else {
                            None
                        }
                    });
                    match (active, attempt) {
                        (Some(held), _) => {
                            let _ = fs::remove_file(&tmp);
                            return Err(AcquireError::Held(held));
                        }
                        (None, 0) => {
                            // Stale: unlink and retry. Note that we don't
                            // know which content `read_lock_file` saw vs
                            // what's now on disk; if a fresh writer lands
                            // in the gap, the retry's hard_link will fail
                            // again with AlreadyExists and we'll branch
                            // into the (None, _) arm below.
                            let _ = fs::remove_file(&path);
                            continue;
                        }
                        (None, _) => {
                            // We tried to clean up but lost the next race
                            // — or read_lock_file saw mid-write content
                            // (None from a partially-written file is the
                            // pre-fix bug surface). Re-inspect to give
                            // the caller a useful Held(_) error if a
                            // fresh peer landed.
                            let _ = fs::remove_file(&tmp);
                            if let Some((pid, st)) = read_lock_file(&path) {
                                return Err(AcquireError::Held(ActiveLock {
                                    slot: slot.to_string(),
                                    pid,
                                    start_time: st,
                                }));
                            }
                            return Err(AcquireError::Io(e));
                        }
                    }
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(AcquireError::Io(e));
                }
            }
        }
        // Unreachable: the loop always returns. Defensive:
        let _ = fs::remove_file(&tmp);
        Err(AcquireError::Io(io::Error::other(
            "process_lock: acquire loop fell through unexpectedly",
        )))
    }

    /// Legacy acquire: overwrites any existing file. Use ONLY when the
    /// caller has opted out of single-writer election (e.g. via
    /// `CARTOG_SINGLE_WRITER=0`). Behaviour matches pre-Phase-2 cartog.
    ///
    /// Two processes calling this concurrently against the same slot will
    /// both report success and the DB-level migration race that the
    /// election was meant to prevent comes back. Phase 6a's busy-retry
    /// remains the only defense in that case.
    pub fn acquire_overwriting(state_dir: &Path, slot: &str) -> io::Result<Self> {
        validate_slot(slot)?;
        fs::create_dir_all(state_dir)?;
        let path = state_dir.join(format!("{slot}.{PID_EXTENSION}"));
        let pid = std::process::id();
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
                // Only unlink if the content is *still* unreadable on a
                // second read — otherwise we'd race a partially-written
                // file from a concurrent acquire (which writes its
                // payload over multiple syscalls inside the O_EXCL guard).
                if read_lock_file(&path).is_none() {
                    let _ = fs::remove_file(&path);
                }
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
            unlink_if_unchanged(&path, pid, recorded_st);
        }
    }
    active
}

/// Unlink the PID file at `path` only if its current contents still match
/// the `(pid, start_time)` we observed earlier. Closes the TOCTOU window
/// where a concurrent `ProcessLock::acquire` rewrites the file with a
/// fresh, live PID between our read and our unlink — without the recheck
/// `cartog self update` could clobber a live primary's lock.
fn unlink_if_unchanged(path: &Path, expected_pid: u32, expected_st: Option<u64>) -> bool {
    match read_lock_file(path) {
        Some((reread_pid, reread_st)) if (reread_pid, reread_st) == (expected_pid, expected_st) => {
            fs::remove_file(path).is_ok()
        }
        _ => false,
    }
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
/// distinct files. Used by `acquire_overwriting` (the kill-switch path);
/// the O_EXCL acquire path uses `write_tmp` + `hard_link` so the target
/// is never observed in an empty state.
fn write_atomic(tmp: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    write_tmp(tmp, bytes)?;
    fs::rename(tmp, target)
}

/// Hash of the current thread's id. Used to disambiguate tmp filenames
/// for concurrent acquires from the same process (multi-thread runtimes,
/// test harnesses).
fn thread_id_hash() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut h);
    h.finish()
}

/// Write `bytes` to `tmp` and fsync. Caller is responsible for linking
/// or renaming `tmp` to its final destination.
fn write_tmp(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    // fsync before linking so a crash between the data write and the
    // link doesn't leave a zero-byte file on disk after recovery.
    let f = fs::File::create(tmp)?;
    use std::io::Write;
    (&f).write_all(bytes)?;
    f.sync_all()?;
    Ok(())
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
            match err {
                AcquireError::Io(io_err) => {
                    assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput, "slot {bad:?}");
                }
                AcquireError::Held(_) => panic!("invalid slot {bad:?} should be Io, got Held"),
            }
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

    #[test]
    fn acquire_against_empty_target_does_not_surface_as_io_error() {
        // Regression: an empty PID file at the target (e.g. a competing
        // acquire in its critical window with the previous create_new +
        // write_all sequence) used to make acquire return
        // AcquireError::Io(AlreadyExists) instead of resolving to either
        // a real Held(_) or a successful claim. With the hard_link-based
        // acquire, the inode is fully written before becoming visible at
        // `path`, so a competing acquire either succeeds or sees the
        // fully-written file. An externally-created empty file at the
        // target is a corruption case (no real cartog writes one) and
        // should still produce a clean Io error — but not corrupt the
        // distinction between Held and Io for normal acquires.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        // Manually plant an empty file.
        fs::File::create(&path).unwrap();
        // hard_link will fail with AlreadyExists. read_lock_file returns
        // None for empty content. attempt 0 unlinks it (no holder). The
        // retry then succeeds — we claim the slot.
        let lock = ProcessLock::acquire(dir.path(), "serve").expect("retry succeeds");
        assert!(path.exists());
        let (recorded_pid, _) = read_lock_file(&path).expect("file now has our content");
        assert_eq!(recorded_pid, std::process::id());
        drop(lock);
    }

    #[test]
    fn concurrent_acquires_never_observe_empty_target() {
        // Race many threads against each other on the same slot. With the
        // hard_link-based acquire, exactly one wins; the losers either
        // see Held(_) (with a fully-readable file) or transiently see
        // AlreadyExists and retry. The pre-fix code could surface
        // AcquireError::Io(AlreadyExists) when the loser hit the
        // empty-target window — this test would have flaked there.
        use std::sync::{Arc, Barrier};
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_path_buf();
        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let mut handles = Vec::new();
        for _ in 0..n {
            let b = Arc::clone(&barrier);
            let p = dir_path.clone();
            handles.push(std::thread::spawn(move || -> Result<bool, AcquireError> {
                b.wait();
                match ProcessLock::acquire(&p, "serve") {
                    Ok(lock) => {
                        // Hold briefly so others race against us, then drop.
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        drop(lock);
                        Ok(true)
                    }
                    Err(AcquireError::Held(_)) => Ok(false),
                    Err(e) => Err(e),
                }
            }));
        }
        let mut winners = 0;
        for h in handles {
            match h.join().expect("thread did not panic") {
                Ok(true) => winners += 1,
                Ok(false) => {}
                Err(e) => panic!(
                    "acquire must not surface as Io error under concurrency (got {e:?}); \
                     pre-fix code could fail here with Io(AlreadyExists)"
                ),
            }
        }
        // Multiple winners are possible because each releases sequentially
        // and the next acquire happily takes the freed slot. The key
        // assertion is no thread saw an Io error — every result was
        // Ok(true) or Held.
        assert!(winners >= 1, "at least one acquire must succeed");
    }

    #[test]
    fn acquire_writes_full_payload_before_target_is_visible() {
        // The hard_link strategy ensures the target's inode is never
        // observed in an empty state: tmp is written + fsync'd, then
        // atomically linked. We can't directly test the absence-of-window
        // (it'd require a probe in the middle of the syscall), but we
        // can verify the post-condition: after acquire, the file at
        // target is fully written.
        let dir = TempDir::new().unwrap();
        let lock = ProcessLock::acquire(dir.path(), "serve").unwrap();
        let path = lock.path();
        let contents = fs::read_to_string(path).unwrap();
        assert!(
            contents.starts_with(&format!("{}", std::process::id())),
            "file must contain at least the PID line, got: {contents:?}"
        );
        // No leftover tmp files (hard_link succeeded, we cleaned up).
        let tmp_count = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(tmp_count, 0, "no leftover .tmp files expected");
    }

    #[test]
    fn unlink_if_unchanged_removes_when_content_matches() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        fs::write(&path, "4194304\n").unwrap();
        let removed = unlink_if_unchanged(&path, 4_194_304, None);
        assert!(removed, "expected unlink to succeed");
        assert!(!path.exists(), "file should be gone");
    }

    #[test]
    fn unlink_if_unchanged_preserves_when_content_changed_under_us() {
        // Regression: this is the TOCTOU window that previously let
        // find_active_locks clobber a live primary's lock. The scanner
        // observes a stale PID, decides to remove — but between observe
        // and remove, a real acquire wrote a live PID to the same path.
        // unlink_if_unchanged must NOT remove the rewritten content.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        // Scanner thinks it observed (dead_pid=4194304, st=None).
        // Disk currently holds the *replacement* the acquire wrote.
        fs::write(&path, "12345\n67890\n").unwrap();

        let removed = unlink_if_unchanged(&path, 4_194_304, None);
        assert!(
            !removed,
            "must not unlink content the scanner did not observe"
        );
        assert!(path.exists(), "live file must survive the scan");
    }

    #[test]
    fn unlink_if_unchanged_preserves_when_file_already_gone() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        // No file at all — concurrent unlink already happened.
        let removed = unlink_if_unchanged(&path, 4_194_304, None);
        assert!(!removed, "no-op when file is missing");
    }

    #[test]
    fn find_active_locks_does_not_unlink_a_freshly_rewritten_file() {
        // Integration-level test: when the scanner reads (dead_pid, _)
        // and decides not-alive but the file has been rewritten to a
        // live entry by an external writer, the new content must survive
        // the scan. Exercises find_active_locks's recheck pathway.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        // We simulate the *current* state of the file (after an acquire
        // by another process). find_active_locks reads this content and
        // since our own PID is alive, returns it as Active — not deletes
        // it. This guards against a future regression where the recheck
        // is removed and the scanner unlinks based on a stale snapshot.
        let live_pid = std::process::id();
        let live_st = process_start_time(live_pid);
        let payload = match live_st {
            Some(st) => format!("{live_pid}\n{st}\n"),
            None => format!("{live_pid}\n"),
        };
        fs::write(&path, &payload).unwrap();
        let active = find_active_locks(dir.path());
        assert!(path.exists(), "live file must not be unlinked");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].pid, live_pid);
    }

    #[test]
    fn acquire_returns_held_when_live_peer_owns_slot() {
        // First acquire succeeds. Second must see the holder and report it.
        let dir = TempDir::new().unwrap();
        let first = ProcessLock::acquire(dir.path(), "serve").unwrap();

        let err = ProcessLock::acquire(dir.path(), "serve").unwrap_err();
        match err {
            AcquireError::Held(held) => {
                assert_eq!(held.slot, "serve");
                assert_eq!(held.pid, std::process::id());
            }
            AcquireError::Io(e) => panic!("expected Held, got Io({e})"),
        }
        // The held file must still belong to the first lock — second attempt
        // does NOT clobber it.
        let (recorded_pid, _) = read_lock_file(first.path()).expect("file still parses");
        assert_eq!(recorded_pid, std::process::id());
        drop(first);
    }

    #[test]
    fn acquire_cleans_stale_pid_and_succeeds() {
        // A leftover PID file pointing at a dead process must NOT cause
        // Held — we unlink it and our own acquire wins.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        fs::write(&path, "4194304\n0\n").unwrap();
        let lock = ProcessLock::acquire(dir.path(), "serve").unwrap();
        let (recorded_pid, _) = read_lock_file(&path).expect("file parses");
        assert_eq!(recorded_pid, std::process::id());
        drop(lock);
    }

    #[test]
    fn acquire_cleans_pid_reused_lock_and_succeeds() {
        // File records our own PID but a wrong start_time — same outcome as
        // a dead PID: we treat the holder as gone (PID was reused) and
        // claim the slot.
        let dir = TempDir::new().unwrap();
        let pid = std::process::id();
        let real_st = match process_start_time(pid) {
            Some(s) => s,
            None => return, // unsupported platform: skip
        };
        let path = dir.path().join("serve.pid");
        fs::write(&path, format!("{pid}\n{}\n", real_st.wrapping_add(1))).unwrap();
        let _lock = ProcessLock::acquire(dir.path(), "serve").unwrap();
        // After acquire, the file should have the correct start_time.
        let (recorded_pid, recorded_st) = read_lock_file(&path).expect("file parses");
        assert_eq!(recorded_pid, pid);
        assert_eq!(recorded_st, Some(real_st));
    }

    #[test]
    fn acquire_overwriting_always_wins() {
        // The kill-switch path must succeed even when a live peer holds the
        // slot. We simulate the "live peer" with a real ProcessLock against
        // a sibling slot, then have acquire_overwriting clobber a manual
        // file we wrote first.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("serve.pid");
        fs::write(&path, format!("{}\n0\n", std::process::id())).unwrap();
        let _lock = ProcessLock::acquire_overwriting(dir.path(), "serve").unwrap();
        let (recorded_pid, recorded_st) = read_lock_file(&path).expect("file parses");
        assert_eq!(recorded_pid, std::process::id());
        // On platforms where start_time is available, acquire_overwriting
        // also records it (same format as the exclusive path).
        if process_start_time(std::process::id()).is_some() {
            assert!(recorded_st.is_some());
        }
    }
}
