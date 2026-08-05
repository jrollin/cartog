//! Tests for update orchestration, the defer/apply flow, and breadcrumbs.

use serial_test::serial;
use std::path::PathBuf;
use std::time::Duration;

use crate::commands::self_cmd::*;

#[test]
fn update_mode_from_flags_maps_each_combination() {
    assert_eq!(
        UpdateMode::from_flags(true, false, None, false, false),
        UpdateMode::Check
    );
    assert_eq!(
        UpdateMode::from_flags(false, true, None, false, false),
        UpdateMode::Defer(None)
    );
    assert_eq!(
        UpdateMode::from_flags(false, true, Some("0.20.0".to_string()), false, false),
        UpdateMode::Defer(Some("0.20.0".to_string())),
        "--to pins an explicit target"
    );
    assert_eq!(
        UpdateMode::from_flags(false, false, None, true, false),
        UpdateMode::ApplyPending { at_startup: false }
    );
    assert_eq!(
        UpdateMode::from_flags(false, false, None, true, true),
        UpdateMode::ApplyPending { at_startup: true },
        "--at-startup propagates into the apply-pending variant"
    );
    assert_eq!(
        UpdateMode::from_flags(false, false, None, false, false),
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

#[cfg(not(target_os = "windows"))]
#[test]
fn wait_for_no_peer_excluding_ignores_listed_slot() {
    // A live peer whose slot is excluded (this project's own serve at startup)
    // must be treated as not-a-peer → immediate Ok.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("serve-abc123.pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    let excluded = vec!["serve-abc123".to_string()];
    assert!(
        wait_for_no_peer_excluding(dir.path(), Duration::from_millis(50), &excluded).is_ok(),
        "an excluded slot must not block the wait"
    );
}

/// This project's own slots, as the budget/count helpers expect them.
fn own_slots() -> Vec<String> {
    vec!["serve-abc".to_string(), "watch-abc".to_string()]
}

#[test]
fn peer_wait_budget_is_full_when_nothing_is_blocking() {
    // Nothing blocking yet: keep the budget that absorbs a peer arriving mid-wait.
    assert_eq!(peer_wait_budget(&own_slots(), &[], &[]), APPLY_PEER_WAIT);
}

#[test]
fn peer_wait_budget_caps_own_project_peer_at_the_hook_safe_grace() {
    // Own teardown is worth waiting for, but a same-repo window hashes to the
    // same slot and never clears, so the wait is capped (#154).
    let active = vec!["serve-abc".to_string()];
    assert_eq!(
        peer_wait_budget(&own_slots(), &active, &[]),
        APPLY_OWN_PEER_GRACE
    );
}

#[test]
fn peer_wait_budget_does_not_wait_for_foreign_peer() {
    // Another project's peer holds its lock for as long as that session lives.
    let active = vec!["serve-other".to_string(), "watch-other".to_string()];
    assert_eq!(
        peer_wait_budget(&own_slots(), &active, &[]),
        APPLY_FOREIGN_PEER_WAIT
    );
}

#[test]
fn peer_wait_budget_does_not_wait_when_a_foreign_peer_is_among_own_peers() {
    // The #154 regression: a co-live own peer must not buy a budget the foreign
    // lock then consumes.
    let active = vec!["serve-other".to_string(), "watch-abc".to_string()];
    assert_eq!(
        peer_wait_budget(&own_slots(), &active, &[]),
        APPLY_FOREIGN_PEER_WAIT,
        "a co-live own peer must not resurrect the full wait"
    );
}

#[test]
fn peer_wait_budget_ignores_excluded_slots_like_the_wait_does() {
    // --at-startup excludes own slots from the wait, so they must not drive the
    // budget either.
    let active = vec!["serve-abc".to_string(), "serve-other".to_string()];
    let excluded = own_slots();
    assert_eq!(
        peer_wait_budget(&own_slots(), &active, &excluded),
        APPLY_FOREIGN_PEER_WAIT,
        "an excluded own slot must not buy a budget the wait cannot use"
    );
}

#[test]
fn peer_wait_budget_is_full_when_every_blocker_is_excluded() {
    // All blockers excluded → nothing can block → keep the generous budget.
    let active = own_slots();
    let excluded = own_slots();
    assert_eq!(
        peer_wait_budget(&own_slots(), &active, &excluded),
        APPLY_PEER_WAIT
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn foreign_peer_wait_returns_promptly_via_the_real_budget_path() {
    // The "Hook cancelled" regression: composes the budget as run_apply_pending
    // does and asserts real elapsed time, not just the chosen Duration.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("serve-other999.pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    let own = own_slots();
    let budget = peer_wait_budget(&own, &active_peer_slots(dir.path()), &[]);

    let start = std::time::Instant::now();
    let result = wait_for_no_peer_excluding(dir.path(), budget, &[]);
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "a live foreign peer must still report exit 6"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "foreign peer must not wait; took {elapsed:?}"
    );
}

#[test]
fn foreign_peer_count_counts_pids_not_slots() {
    // A session's serve+watch share one PID: must count once, not twice.
    let locks = vec![
        ("serve-other".to_string(), 700u32),
        ("watch-other".to_string(), 700u32),
    ];
    let diag = peer_diagnostic_from(&locks, &own_slots(), &[], ("serve-other", 700));
    assert_eq!(diag.foreign_processes, 1);
}

#[test]
fn foreign_peer_count_excludes_this_projects_own_peers() {
    // Our own server is not "another session".
    let locks = vec![("serve-abc".to_string(), 700u32)];
    let diag = peer_diagnostic_from(&locks, &own_slots(), &[], ("serve-abc", 700));
    assert_eq!(
        diag.foreign_processes, 0,
        "own-project locks must not be reported as other sessions"
    );
}

#[test]
fn foreign_peer_count_excludes_excluded_slots() {
    // Excluded slots must not be counted as foreign peers.
    let locks = vec![("serve-abc".to_string(), 700u32)];
    let excluded = own_slots();
    let diag = peer_diagnostic_from(&locks, &own_slots(), &excluded, ("serve-abc", 700));
    assert_eq!(diag.foreign_processes, 0);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn peer_lock_snapshot_excludes_the_apply_coordination_lock() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(format!("{APPLY_LOCK_SLOT}.pid")),
        std::process::id().to_string(),
    )
    .unwrap();
    assert!(peer_lock_snapshot(dir.path()).is_empty());
}

#[test]
fn peer_diagnostic_names_a_foreign_lock_when_one_exists() {
    // Reporting "N other sessions" while naming our own lock points the user at
    // the wrong process, so a foreign lock wins.
    let locks = vec![
        ("watch-abc".to_string(), 111u32),
        ("serve-other".to_string(), 222u32),
    ];
    let diag = peer_diagnostic_from(&locks, &own_slots(), &[], ("watch-abc", 111));
    assert_eq!(diag.slot, "serve-other", "must name the foreign lock");
    assert_eq!(diag.pid, 222);
    assert_eq!(diag.foreign_processes, 1);
}

#[test]
fn peer_diagnostic_falls_back_to_the_blocking_lock_when_all_are_ours() {
    // No foreign lock to name: keep the one the wait timed out on.
    let locks = vec![("serve-abc".to_string(), 111u32)];
    let diag = peer_diagnostic_from(&locks, &own_slots(), &[], ("serve-abc", 111));
    assert_eq!(diag.slot, "serve-abc");
    assert_eq!(diag.pid, 111);
    assert_eq!(diag.foreign_processes, 0);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn active_peer_slots_excludes_apply_coordination_lock() {
    // The apply lock is not a peer; counting it invents a phantom blocker.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(format!("{APPLY_LOCK_SLOT}.pid")),
        std::process::id().to_string(),
    )
    .unwrap();
    assert!(active_peer_slots(dir.path()).is_empty());
}

#[test]
#[serial]
fn test_seam_overrides_every_tier_rather_than_capping_it() {
    // Override, not cap: a value above a tier must still apply.
    let _guard = EnvVarGuard::set("CARTOG_TEST_APPLY_PEER_WAIT_MS", "3000");
    let active = vec!["serve-other".to_string()];
    assert_eq!(
        effective_peer_wait(&own_slots(), &active, &[]),
        Duration::from_millis(3000),
        "override must win over the foreign tier, not be clamped by it"
    );
}

#[test]
#[serial]
fn effective_peer_wait_falls_back_to_the_tier_without_the_seam() {
    let _guard = EnvVarGuard::unset("CARTOG_TEST_APPLY_PEER_WAIT_MS");
    let active = vec!["serve-other".to_string()];
    assert_eq!(
        effective_peer_wait(&own_slots(), &active, &[]),
        APPLY_FOREIGN_PEER_WAIT
    );
}

#[test]
#[serial]
fn effective_peer_wait_ignores_a_malformed_seam_value() {
    // A garbage override must defer to the tier, not to some fixed budget.
    let _guard = EnvVarGuard::set("CARTOG_TEST_APPLY_PEER_WAIT_MS", "not-a-number");
    let active = vec!["serve-other".to_string()];
    assert_eq!(
        effective_peer_wait(&own_slots(), &active, &[]),
        APPLY_FOREIGN_PEER_WAIT,
        "malformed override must defer to the tier"
    );
    let own_only = vec!["serve-abc".to_string()];
    assert_eq!(
        effective_peer_wait(&own_slots(), &own_only, &[]),
        APPLY_OWN_PEER_GRACE,
        "malformed override must not collapse the own-peer tier either"
    );
}

#[test]
fn peer_running_message_reports_other_sessions_without_naming_a_product() {
    let msg = peer_running_message("serve-abc", 4242, 3);
    assert!(msg.contains('3'), "should report how many sessions: {msg}");
    assert!(
        msg.contains("serve-abc") && msg.contains("4242"),
        "must keep the blocking slot+PID — the only exit-6 diagnostic: {msg}"
    );
    assert!(
        !msg.contains("Claude"),
        "a lock holder can be any MCP client or a terminal watch: {msg}"
    );
    assert!(
        !msg.contains("no action needed"),
        "a wedged or long-lived peer may never exit — must not promise that: {msg}"
    );
}

#[test]
fn peer_running_message_uses_singular_for_one_other_session() {
    // One other session is exactly one PID: the case #154 is about.
    let msg = peer_running_message("serve-xyz", 99, 1);
    assert!(msg.contains("1 other session"), "expected singular: {msg}");
    assert!(msg.contains("serve-xyz") && msg.contains("99"));
}

#[test]
fn peer_running_message_describes_own_shutdown_when_no_foreign_peer() {
    // Own teardown only: must not claim another session exists.
    let msg = peer_running_message("serve-abc", 4242, 0);
    assert!(
        !msg.contains("other session"),
        "no foreign peer — must not invent one: {msg}"
    );
    assert!(msg.contains("serve-abc") && msg.contains("4242"));
}

#[cfg(not(target_os = "windows"))]
#[test]
fn wait_for_no_peer_excluding_still_blocks_other_slot() {
    // A live peer in a DIFFERENT slot (another project) must still block, even
    // when this project's own slot is excluded.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("serve-other999.pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    let excluded = vec!["serve-abc123".to_string()];
    let result = wait_for_no_peer_excluding(dir.path(), Duration::from_millis(150), &excluded);
    assert!(result.is_err(), "a non-excluded peer must still time out");
    assert_eq!(result.unwrap_err().slot, "serve-other999");
}
