//! Verifies the PID-file lifecycle in `cartog watch`.

use std::time::{Duration, Instant};

use cartog_watch::{run_watch, spawn_watch, WatchConfig, WATCH_LOCK_SLOT};

fn wait_for<F: FnMut() -> bool>(mut pred: F, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pred()
}

#[test]
fn pid_file_written_on_start_and_removed_on_stop() {
    let workspace = tempfile::TempDir::new().unwrap();
    let lock_dir = tempfile::TempDir::new().unwrap();

    let mut config = WatchConfig::new(workspace.path().to_path_buf());
    config.pid_lock_dir = Some(lock_dir.path().to_path_buf());
    config.pid_lock_slot = Some(WATCH_LOCK_SLOT.to_string());

    let handle = spawn_watch(config, ":memory:").expect("spawn watch");

    let pid_path = lock_dir.path().join(format!("{WATCH_LOCK_SLOT}.pid"));
    assert!(
        wait_for(|| pid_path.exists(), Duration::from_secs(5)),
        "PID file should appear under {pid_path:?} once the watcher is running"
    );
    // File is now two lines (pid + start_time); only the first line is the PID.
    let contents = std::fs::read_to_string(&pid_path).unwrap();
    let recorded: u32 = contents.lines().next().unwrap().trim().parse().unwrap();
    assert_eq!(
        recorded,
        std::process::id(),
        "PID file should hold the running process ID"
    );

    handle.stop();

    assert!(
        wait_for(|| !pid_path.exists(), Duration::from_secs(5)),
        "PID file should be removed once the watcher exits"
    );
}

#[test]
fn pid_file_run_watch_propagates_acquire_failure() {
    // pid_lock_dir pointed at an existing file makes create_dir_all fail.
    let workspace = tempfile::TempDir::new().unwrap();
    let blocker = tempfile::NamedTempFile::new().unwrap();
    let mut config = WatchConfig::new(workspace.path().to_path_buf());
    config.pid_lock_dir = Some(blocker.path().to_path_buf());
    config.pid_lock_slot = Some(WATCH_LOCK_SLOT.to_string());

    let err =
        run_watch(config, ":memory:").expect_err("run_watch should fail when lock dir is unusable");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("watch PID lock"),
        "error should mention the lock context, got: {msg}"
    );
}

#[test]
fn spawn_watch_rejects_dir_without_slot_synchronously() {
    // Regression: pre-fix, spawn_watch returned Ok(WatchHandle) even when
    // watch_loop bailed inside the thread on the (Some(dir), None) misconfig
    // — the caller got a handle for an already-dead watcher with only a
    // tracing::warn! to indicate the failure. The synchronous validation
    // pass in spawn_watch must surface the error to the caller.
    let workspace = tempfile::TempDir::new().unwrap();
    let lock_dir = tempfile::TempDir::new().unwrap();
    let mut config = WatchConfig::new(workspace.path().to_path_buf());
    config.pid_lock_dir = Some(lock_dir.path().to_path_buf());
    config.pid_lock_slot = None;

    let err = match spawn_watch(config, ":memory:") {
        Ok(_) => panic!("spawn_watch must reject misconfig pre-thread"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("pid_lock_slot is None"),
        "error should explain the misconfiguration, got: {msg}"
    );
}

#[test]
fn pid_file_dir_without_slot_is_rejected() {
    // Regression: the half-configured state (lock_dir set, slot None) used
    // to silently fall back to the global WATCH_LOCK_SLOT, letting an
    // embedder collide with — or be hidden from — a CLI peer that derives a
    // DB-scoped slot. Must surface as a hard error.
    let workspace = tempfile::TempDir::new().unwrap();
    let lock_dir = tempfile::TempDir::new().unwrap();
    let mut config = WatchConfig::new(workspace.path().to_path_buf());
    config.pid_lock_dir = Some(lock_dir.path().to_path_buf());
    config.pid_lock_slot = None;

    let err =
        run_watch(config, ":memory:").expect_err("run_watch must reject lock_dir without slot");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("pid_lock_slot is None"),
        "error should explain the misconfiguration, got: {msg}"
    );
}

#[test]
fn pid_file_slot_without_dir_is_rejected() {
    // Inverse half-config: pid_lock_slot set but pid_lock_dir is None.
    // Pre-fix this was silently ignored — the slot did nothing and the
    // caller's intent was dropped. Must surface as a hard error.
    let workspace = tempfile::TempDir::new().unwrap();
    let mut config = WatchConfig::new(workspace.path().to_path_buf());
    config.pid_lock_dir = None;
    config.pid_lock_slot = Some("watch-deadbeef".to_string());

    let err =
        run_watch(config, ":memory:").expect_err("run_watch must reject slot without lock_dir");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("pid_lock_dir is None"),
        "error should explain the misconfiguration, got: {msg}"
    );
}
