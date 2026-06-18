//! Tests for update orchestration, the defer/apply flow, and breadcrumbs.

use serial_test::serial;
use std::path::PathBuf;
use std::time::Duration;

use crate::commands::self_cmd::*;

#[test]
fn update_mode_from_flags_maps_each_combination() {
    assert_eq!(
        UpdateMode::from_flags(true, false, None, false),
        UpdateMode::Check
    );
    assert_eq!(
        UpdateMode::from_flags(false, true, None, false),
        UpdateMode::Defer(None)
    );
    assert_eq!(
        UpdateMode::from_flags(false, true, Some("0.20.0".to_string()), false),
        UpdateMode::Defer(Some("0.20.0".to_string())),
        "--to pins an explicit target"
    );
    assert_eq!(
        UpdateMode::from_flags(false, false, None, true),
        UpdateMode::ApplyPending
    );
    assert_eq!(
        UpdateMode::from_flags(false, false, None, false),
        UpdateMode::Now
    );
}

#[test]
fn decide_apply_proceeds_only_when_target_is_newer() {
    assert_eq!(decide_apply("0.19.0", "0.20.0"), ApplyDecision::Proceed);
    assert_eq!(
        decide_apply("0.20.0", "0.20.0"),
        ApplyDecision::Skip,
        "target == current is a satisfied (stale) intent"
    );
    assert_eq!(
        decide_apply("0.21.0", "0.20.0"),
        ApplyDecision::Skip,
        "target < current is stale"
    );
}

#[test]
fn decide_apply_skips_on_malformed_target() {
    // Hand-edited or foreign state: a non-bare-semver target must be
    // treated as stale, not parsed to 0.0.0 and acted on.
    assert_eq!(
        decide_apply("0.19.0", "v0.20.0"),
        ApplyDecision::Skip,
        "a `v`-prefixed target is not bare semver"
    );
    assert_eq!(decide_apply("0.19.0", ""), ApplyDecision::Skip);
    assert_eq!(decide_apply("0.19.0", "garbage"), ApplyDecision::Skip);
    assert_eq!(decide_apply("0.19.0", "0.20"), ApplyDecision::Skip);
    assert_eq!(decide_apply("0.19.0", "1.0.0-rc.1"), ApplyDecision::Skip);
}

#[test]
fn intent_disposition_keeps_transient_clears_deterministic() {
    assert_eq!(
        intent_disposition(&UpgradeError::Network("x".into())),
        IntentDisposition::Keep
    );
    assert_eq!(
        intent_disposition(&UpgradeError::Filesystem("x".into())),
        IntentDisposition::Keep
    );
    assert_eq!(
        intent_disposition(&UpgradeError::Checksum("x".into())),
        IntentDisposition::Clear
    );
    assert_eq!(
        intent_disposition(&UpgradeError::Smoke("x".into())),
        IntentDisposition::Clear,
        "a smoke failure is deterministic for a fixed target — clear, do not retry-loop"
    );
}

#[test]
fn wait_for_no_peer_returns_ok_when_no_locks() {
    let dir = tempfile::TempDir::new().unwrap();
    // Empty state dir → no peers → immediate Ok.
    assert!(wait_for_no_peer(dir.path(), Duration::from_millis(50)).is_ok());
}

#[test]
fn wait_for_no_peer_ignores_apply_lock_slot() {
    // The apply-update coordination lock must not be seen as a serve/watch
    // peer — a concurrent apply reaches the lock-acquire step instead.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(format!("{APPLY_LOCK_SLOT}.pid")),
        std::process::id().to_string(),
    )
    .unwrap();
    assert!(
        wait_for_no_peer(dir.path(), Duration::from_millis(50)).is_ok(),
        "apply-update lock must be ignored by the peer wait"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn wait_for_no_peer_proceeds_when_peer_clears_mid_wait() {
    // Plant a live peer, then remove it partway through the budget; the
    // poll loop must re-check and return Ok before the deadline.
    let dir = tempfile::TempDir::new().unwrap();
    let pid_file = dir.path().join("serve.pid");
    std::fs::write(&pid_file, std::process::id().to_string()).unwrap();
    let pid_file_clone = pid_file.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::fs::remove_file(&pid_file_clone);
    });
    let result = wait_for_no_peer(dir.path(), Duration::from_secs(2));
    handle.join().unwrap();
    assert!(
        result.is_ok(),
        "must proceed once the peer lock disappears mid-wait"
    );
}

#[test]
#[serial]
fn last_update_breadcrumb_path_honors_cartog_log_dir_first() {
    let _xdg = EnvVarGuard::set("XDG_CACHE_HOME", "/tmp/should-not-be-used");
    let _log = EnvVarGuard::set("CARTOG_LOG_DIR", "/tmp/cartog-logs");
    let path = last_update_breadcrumb_path().expect("path resolves");
    assert_eq!(path, PathBuf::from("/tmp/cartog-logs/last-update"));
}

#[test]
#[serial]
fn last_update_breadcrumb_path_prefers_xdg_then_home() {
    let _log = EnvVarGuard::unset("CARTOG_LOG_DIR");
    let _xdg = EnvVarGuard::set("XDG_CACHE_HOME", "/tmp/xdg-cache");
    let path = last_update_breadcrumb_path().expect("path resolves");
    assert_eq!(path, PathBuf::from("/tmp/xdg-cache/cartog/last-update"));

    let _xdg2 = EnvVarGuard::unset("XDG_CACHE_HOME");
    let _home = EnvVarGuard::set("HOME", "/tmp/fake-home");
    let path = last_update_breadcrumb_path().expect("path resolves");
    assert_eq!(
        path,
        PathBuf::from("/tmp/fake-home/.cache/cartog/last-update")
    );
}

#[test]
#[serial]
fn write_last_update_breadcrumb_writes_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let _log = EnvVarGuard::set("CARTOG_LOG_DIR", dir.path().to_str().unwrap());
    write_last_update_breadcrumb("0.42.0");
    let contents = std::fs::read_to_string(dir.path().join("last-update")).unwrap();
    assert_eq!(contents, "cartog updated to 0.42.0.\n");
}

/// Scoped env-var setter/un-setter that restores the prior value on drop.
/// Used with `#[serial]` so the process-global mutation can't leak across
/// concurrently-running tests.
struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }
    fn unset(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[cfg(not(target_os = "windows"))]
#[test]
fn wait_for_no_peer_times_out_with_live_peer() {
    // Plant our own (live) PID as a serve lock; the wait must give up
    // within roughly the budget and name the peer.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("serve.pid"), std::process::id().to_string()).unwrap();
    let start = std::time::Instant::now();
    let result = wait_for_no_peer(dir.path(), Duration::from_millis(150));
    let elapsed = start.elapsed();
    assert!(result.is_err(), "live peer must cause a timeout error");
    assert_eq!(result.unwrap_err().pid, std::process::id());
    assert!(
        elapsed < Duration::from_secs(2),
        "must not block far beyond the budget, took {elapsed:?}"
    );
}
