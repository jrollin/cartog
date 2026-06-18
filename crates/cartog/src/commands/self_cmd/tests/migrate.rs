//! Tests for `cartog self migrate-db`.

use std::path::Path;

use crate::commands::self_cmd::*;

// ── migrate-db ──────────────────────────────────────────────────────────

#[test]
fn plan_migration_no_legacy_returns_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let moves = plan_migration(dir.path()).expect("plan succeeds");
    assert!(moves.is_empty());
}

#[test]
fn plan_migration_moves_db_and_wal_siblings() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(".cartog.db"), b"db").unwrap();
    std::fs::write(dir.path().join(".cartog.db-wal"), b"wal").unwrap();
    std::fs::write(dir.path().join(".cartog.db-shm"), b"shm").unwrap();
    std::fs::write(
        dir.path().join(".cartog.db.pre-v3-20260101T000000Z.bak"),
        b"bak",
    )
    .unwrap();

    let moves = plan_migration(dir.path()).expect("plan succeeds");
    let names: std::collections::BTreeSet<_> = moves
        .iter()
        .map(|m| m.to.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let expected: std::collections::BTreeSet<_> = [
        "db.sqlite".to_string(),
        "db.sqlite-wal".to_string(),
        "db.sqlite-shm".to_string(),
        "db.sqlite.pre-v3-20260101T000000Z.bak".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(names, expected);
    for m in &moves {
        assert_eq!(m.to.parent().unwrap(), dir.path().join(".cartog"));
    }
}

#[cfg(unix)]
#[test]
fn plan_migration_refuses_symlinks() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::TempDir::new().unwrap();
    let real_target = dir.path().join("real.db");
    std::fs::write(&real_target, b"real").unwrap();
    symlink(&real_target, dir.path().join(".cartog.db")).unwrap();

    let err = plan_migration(dir.path()).unwrap_err();
    assert!(
        err.to_string().contains("symlink"),
        "expected symlink refusal, got: {err}"
    );
}

#[test]
fn plan_migration_refuses_when_destination_exists() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(".cartog.db"), b"db").unwrap();
    std::fs::create_dir_all(dir.path().join(".cartog")).unwrap();
    std::fs::write(dir.path().join(".cartog").join("db.sqlite"), b"existing").unwrap();

    let err = plan_migration(dir.path()).unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[test]
#[serial_test::serial]
fn cmd_self_migrate_db_dry_run_does_not_move() {
    let dir = tempfile::TempDir::new().unwrap();
    let legacy = dir.path().join(".cartog.db");
    std::fs::write(&legacy, b"db").unwrap();

    // Safety: tests using process env vars are serialised via #[serial].
    unsafe { std::env::set_var(TEST_SKIP_PEER_LOCK_ENV, "1") };
    let result = cmd_self_migrate_db(dir.path(), true, true);
    unsafe { std::env::remove_var(TEST_SKIP_PEER_LOCK_ENV) };
    result.expect("dry run succeeds");

    assert!(legacy.exists(), "dry run must not touch the filesystem");
    assert!(!dir.path().join(".cartog").exists());
}

#[test]
fn migrate_peer_guard_bails_for_real_run_when_peer_present() {
    let active = vec![cartog_process_lock::ActiveLock {
        slot: "serve-abc".to_string(),
        pid: 4242,
        start_time: None,
    }];
    let slots = vec!["serve-abc".to_string()];
    let err = migrate_peer_guard(false, &active, &slots).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("serve-abc"), "names the slot: {msg}");
    assert!(msg.contains("4242"), "names the pid: {msg}");
}

#[test]
fn migrate_peer_guard_allows_dry_run_despite_live_peer() {
    // Same live peer as above, but dry_run=true must bypass the guard.
    let active = vec![cartog_process_lock::ActiveLock {
        slot: "serve-abc".to_string(),
        pid: 4242,
        start_time: None,
    }];
    let slots = vec!["serve-abc".to_string()];
    migrate_peer_guard(true, &active, &slots).expect("dry-run ignores the peer lock");
}

#[test]
fn migrate_peer_guard_allows_real_run_when_no_peer() {
    migrate_peer_guard(false, &[], &["serve-abc".to_string()])
        .expect("no peer → real run proceeds");
}

#[test]
fn migrate_peer_guard_ignores_peer_on_unrelated_db() {
    // A serve running for a different project (different slot) must not
    // block this migration.
    let active = vec![cartog_process_lock::ActiveLock {
        slot: "serve-otherproject".to_string(),
        pid: 4242,
        start_time: None,
    }];
    let slots = vec![
        "serve-thisproject".to_string(),
        "watch-thisproject".to_string(),
    ];
    migrate_peer_guard(false, &active, &slots)
        .expect("a peer on an unrelated DB must not block migration");
}

#[test]
fn target_db_slots_covers_legacy_and_new_paths() {
    let root = Path::new("/tmp/some-project");
    let slots = target_db_slots(root);
    // serve+watch for both legacy and new paths.
    assert_eq!(slots.len(), 4);
    assert!(slots.iter().any(|s| s.starts_with("serve-")));
    assert!(slots.iter().any(|s| s.starts_with("watch-")));
    // The legacy and new paths differ, so their slots differ.
    let unique: std::collections::HashSet<_> = slots.iter().collect();
    assert_eq!(unique.len(), 4, "all four slots are distinct");
}

#[test]
#[serial_test::serial]
fn cmd_self_migrate_db_moves_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let legacy = dir.path().join(".cartog.db");

    // Create a real SQLite database so the WAL checkpoint can run without
    // tripping on a malformed file. The DB content itself is irrelevant.
    {
        let db = cartog_db::Database::open(&legacy, 384).unwrap();
        drop(db);
    }
    assert!(legacy.exists());

    unsafe { std::env::set_var(TEST_SKIP_PEER_LOCK_ENV, "1") };
    let result = cmd_self_migrate_db(dir.path(), false, true);
    unsafe { std::env::remove_var(TEST_SKIP_PEER_LOCK_ENV) };
    result.expect("migrate succeeds");

    assert!(!legacy.exists());
    let new_db = dir.path().join(".cartog").join("db.sqlite");
    assert!(new_db.exists(), "main db moved into .cartog/");
    // The migrated DB still opens cleanly.
    let _ = cartog_db::Database::open(&new_db, 384).expect("migrated db opens");
}
