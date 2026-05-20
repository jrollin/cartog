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

    let err =
        run_watch(config, ":memory:").expect_err("run_watch should fail when lock dir is unusable");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("watch PID lock"),
        "error should mention the lock context, got: {msg}"
    );
}
