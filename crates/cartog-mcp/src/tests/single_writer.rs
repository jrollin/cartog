//! Single-writer election: PID-file locks and role / read-only attach.

use super::test_provider;
use crate::*;

// ── PID-file lock tests ──

#[test]
fn pid_file_acquired_when_lock_dir_set() {
    let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::TempDir::new().unwrap();
    let opts = ServerOptions {
        pid_lock_dir: Some(dir.path().to_path_buf()),
        pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
    };
    let outcome = acquire_serve_lock(&opts).expect("acquire");
    let lock = match outcome {
        ServeLockOutcome::Primary(l) => l,
        other => panic!("expected Primary, got {other:?}"),
    };
    let path = dir.path().join(format!("{SERVE_LOCK_SLOT}.pid"));
    assert!(path.exists(), "PID file should exist while lock is held");
    // File is now two lines (pid + start_time); only the first line is the PID.
    let contents = std::fs::read_to_string(&path).unwrap();
    let pid: u32 = contents.lines().next().unwrap().trim().parse().unwrap();
    assert_eq!(pid, std::process::id());
    drop(lock);
    assert!(
        !path.exists(),
        "PID file should be removed once the lock is dropped"
    );
}

#[test]
fn pid_file_skipped_when_lock_dir_unset() {
    let opts = ServerOptions::default();
    let outcome = acquire_serve_lock(&opts).expect("noop");
    assert!(
        matches!(outcome, ServeLockOutcome::Untracked),
        "no lock dir → Untracked"
    );
}

/// Serialize tests that read or mutate the `CARTOG_SINGLE_WRITER` env
/// var. The variable is process-global; cargo test runs cases in
/// parallel by default, so without this mutex a concurrent setter
/// flips the value mid-read on another thread.
fn env_mutex() -> &'static std::sync::Mutex<()> {
    static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(()))
}

#[test]
fn acquire_serve_lock_rejects_dir_without_slot() {
    // Regression: a half-configured ServerOptions (pid_lock_dir set,
    // pid_lock_slot None) used to silently fall back to the global
    // SERVE_LOCK_SLOT, letting an embedder collide with — or be hidden
    // from — a CLI peer that derives a DB-scoped slot. The mixed-scope
    // hazard must surface as a hard error.
    let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::TempDir::new().unwrap();
    let opts = ServerOptions {
        pid_lock_dir: Some(dir.path().to_path_buf()),
        pid_lock_slot: None,
    };
    let err = acquire_serve_lock(&opts).unwrap_err();
    assert!(
        err.to_string().contains("pid_lock_slot is None"),
        "error must explain the misconfiguration, got: {err}"
    );
}

#[test]
fn acquire_serve_lock_rejects_slot_without_dir() {
    // Inverse half-config: pid_lock_slot set but pid_lock_dir is
    // None. Pre-fix the slot was silently ignored and the function
    // returned Ok(Untracked), losing the caller's intent. Must
    // surface as a hard error.
    let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
    let opts = ServerOptions {
        pid_lock_dir: None,
        pid_lock_slot: Some("serve-deadbeef".to_string()),
    };
    let err = acquire_serve_lock(&opts).unwrap_err();
    assert!(
        err.to_string().contains("pid_lock_dir is None"),
        "error must explain the misconfiguration, got: {err}"
    );
}

#[test]
fn distinct_slots_for_different_dbs_do_not_collide() {
    // Two cartog peers serving different DBs in the same per-user state
    // dir must coexist. Pre-PR, both fought over a single `serve.pid`
    // slot; with DB-scoped slots they each claim their own
    // `serve-<hash>.pid`.
    let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::TempDir::new().unwrap();
    let opts_a = ServerOptions {
        pid_lock_dir: Some(dir.path().to_path_buf()),
        pid_lock_slot: Some("serve-aaaa1111".to_string()),
    };
    let opts_b = ServerOptions {
        pid_lock_dir: Some(dir.path().to_path_buf()),
        pid_lock_slot: Some("serve-bbbb2222".to_string()),
    };
    let _a = match acquire_serve_lock(&opts_a).expect("acquire A") {
        ServeLockOutcome::Primary(l) => l,
        other => panic!("expected Primary for A, got {other:?}"),
    };
    let _b = match acquire_serve_lock(&opts_b).expect("acquire B") {
        ServeLockOutcome::Primary(l) => l,
        other => panic!("expected Primary for B, got {other:?}"),
    };
    // Both PID files present on disk, no Held collision.
    assert!(dir.path().join("serve-aaaa1111.pid").exists());
    assert!(dir.path().join("serve-bbbb2222.pid").exists());
}

#[test]
fn serve_to_watch_slot_preserves_db_scope() {
    // The watcher slot is derived from the serve slot so both PID files
    // for the same DB share their scope suffix.
    assert_eq!(serve_to_watch_slot("serve").unwrap(), "watch");
    assert_eq!(serve_to_watch_slot("serve-abc123").unwrap(), "watch-abc123");
    // Off-pattern inputs that start with the bytes "serve" but are NOT
    // a serve-family slot must be REJECTED — silently folding them to
    // the global watch slot would let distinct embedders collide on
    // `watch.pid` while their serve slots stay distinct.
    for bad in [
        "unknown-prefix",
        "server",
        "serverless",
        "servefoo",
        "Serve",
        "",
        "serve-", // trailing-dash with empty hex
    ] {
        assert!(
            serve_to_watch_slot(bad).is_err(),
            "expected off-pattern slot {bad:?} to be rejected"
        );
    }
}

proptest::proptest! {
    /// `serve-<nonempty>` maps to `watch-<same suffix>`, preserving the DB
    /// scope verbatim.
    #[test]
    fn serve_to_watch_slot_round_trips_suffix(suffix in "[0-9a-f]{1,16}") {
        let got = serve_to_watch_slot(&format!("serve-{suffix}")).unwrap();
        proptest::prop_assert_eq!(got, format!("watch-{suffix}"));
    }

    /// Total contract over arbitrary input, checked against the OUTPUT rather
    /// than a re-implemented accept rule: the only accepted slots are `serve`
    /// (→ `watch`) and `serve-<nonempty>` (→ `watch-<same>`); every other
    /// input is rejected. Folding an off-pattern slot to the global watch slot
    /// would let distinct embedders collide on `watch.pid`.
    #[test]
    fn serve_to_watch_slot_contract(s in ".{0,20}") {
        match serve_to_watch_slot(&s) {
            Ok(out) if s == "serve" => proptest::prop_assert_eq!(out, "watch"),
            Ok(out) => {
                // The only other accepted form: the suffix is carried verbatim.
                let suffix = s.strip_prefix("serve-");
                proptest::prop_assert!(
                    suffix.is_some_and(|r| !r.is_empty()),
                    "accepted off-pattern {s:?}"
                );
                proptest::prop_assert_eq!(out, format!("watch-{}", suffix.unwrap()));
            }
            Err(_) => {
                proptest::prop_assert_ne!(&s, "serve");
                proptest::prop_assert!(
                    s.strip_prefix("serve-").map_or(true, str::is_empty),
                    "rejected a valid serve-<nonempty> slot: {s:?}"
                );
            }
        }
    }
}

#[test]
fn second_acquire_for_same_dir_reports_held() {
    // Two acquire_serve_lock calls against the same dir: the first wins,
    // the second must surface Held(_) with the first's PID so the caller
    // can branch.
    let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::TempDir::new().unwrap();
    let opts = ServerOptions {
        pid_lock_dir: Some(dir.path().to_path_buf()),
        pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
    };
    let _first = match acquire_serve_lock(&opts).expect("first acquire") {
        ServeLockOutcome::Primary(l) => l,
        other => panic!("expected Primary, got {other:?}"),
    };
    let second = acquire_serve_lock(&opts).expect("second acquire returns ok");
    match second {
        ServeLockOutcome::Held(held) => {
            assert_eq!(held.slot, SERVE_LOCK_SLOT);
            assert_eq!(held.pid, std::process::id());
        }
        other => panic!("expected Held, got {other:?}"),
    }
}

#[test]
fn kill_switch_disables_election() {
    let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
    // CARTOG_SINGLE_WRITER=0 must let a second acquire_overwriting-style
    // call succeed despite a live first holder. Restoring the env var
    // afterwards is best-effort; tests in a single binary share env so
    // we set + unset around the call site.
    let dir = tempfile::TempDir::new().unwrap();
    let opts = ServerOptions {
        pid_lock_dir: Some(dir.path().to_path_buf()),
        pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
    };
    let _first = match acquire_serve_lock(&opts).expect("first acquire") {
        ServeLockOutcome::Primary(l) => l,
        other => panic!("expected Primary, got {other:?}"),
    };

    // SAFETY: tests in `cargo test` run in threads, but env mutation is
    // process-global. Other tests in this file don't depend on this var,
    // and we restore it before returning.
    let prev = std::env::var(SINGLE_WRITER_ENV).ok();
    // SAFETY: env vars are inherently process-wide and tests share them.
    // We restore the prior value before this test returns so adjacent
    // tests aren't affected.
    unsafe {
        std::env::set_var(SINGLE_WRITER_ENV, "0");
    }
    let result = acquire_serve_lock(&opts);
    // SAFETY: same reason — restoring prior state regardless of outcome.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(SINGLE_WRITER_ENV, v),
            None => std::env::remove_var(SINGLE_WRITER_ENV),
        }
    }
    match result.expect("kill switch acquire") {
        ServeLockOutcome::Primary(_) => {} // expected
        other => panic!("expected Primary with kill switch, got {other:?}"),
    }
}

// ── Role / read-only attach tests (Phase 4) ──

#[test]
fn primary_server_reports_primary_role() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let server = CartogServer::new_with_provider(
        &db_path,
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::Primary,
    )
    .expect("primary server constructs");
    assert_eq!(server.role(), Role::Primary);
}

#[test]
fn read_only_server_reports_read_only_role() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    // First open writable to materialize the file with current schema.
    {
        let _primary = CartogServer::new_with_provider(
            &db_path,
            test_provider(),
            indexer::RedactionConfig::disabled(),
            indexer::WalkFilter::unrestricted(),
            Role::Primary,
        )
        .expect("primary server constructs");
    }
    let reader = CartogServer::new_with_provider(
        &db_path,
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::ReadOnly,
    )
    .expect("read-only server constructs");
    assert_eq!(reader.role(), Role::ReadOnly);
}

#[test]
fn promoter_validate_pinned_state_matches_when_unchanged() {
    let _serial = test_validate_call_counter::SERIAL.blocking_lock();
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        // Materialize a DB so open_readonly can later read its state.
        let _primary = Database::open(&db_path, 384).unwrap();
    }
    let pinned = Database::open_readonly(&db_path)
        .unwrap()
        .pinned_attach()
        .cloned();
    validate_pinned_state(&db_path, pinned.as_ref()).expect("matching pin must validate");
}

#[test]
fn promoter_validate_pinned_state_detects_schema_bump() {
    let _serial = test_validate_call_counter::SERIAL.blocking_lock();
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pinned = {
        let db = Database::open(&db_path, 384).unwrap();
        drop(db);
        Database::open_readonly(&db_path)
            .unwrap()
            .pinned_attach()
            .cloned()
    };
    // Simulate another writer upgrading the schema underneath us.
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.set_metadata("schema_version", "9999").unwrap();
    }
    let result = validate_pinned_state(&db_path, pinned.as_ref());
    // open_readonly returns SchemaDrift; validate_pinned_state wraps as anyhow.
    assert!(result.is_err(), "schema bump under us must fail validation");
}

#[test]
fn atomic_role_round_trip() {
    let r = AtomicRole::new(Role::ReadOnly);
    assert_eq!(r.load(), Role::ReadOnly);
    r.store(Role::Primary);
    assert_eq!(r.load(), Role::Primary);
    r.store(Role::ReadOnly);
    assert_eq!(r.load(), Role::ReadOnly);
}

#[test]
fn read_only_server_refuses_write_tools() {
    // refuse_if_read_only is the helper gating cartog_index and
    // cartog_rag_index. Verify both call sites get an error in
    // ReadOnly mode and pass through silently in Primary mode.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _primary = CartogServer::new_with_provider(
            &db_path,
            test_provider(),
            indexer::RedactionConfig::disabled(),
            indexer::WalkFilter::unrestricted(),
            Role::Primary,
        )
        .expect("primary server constructs");
    }
    let reader = CartogServer::new_with_provider(
        &db_path,
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::ReadOnly,
    )
    .expect("read-only server constructs");

    let err = reader
        .refuse_if_read_only("cartog_index")
        .expect("read-only must refuse");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("read-only") && msg.contains("cartog_index"),
        "error must name the gate and the tool, got: {msg}"
    );
    // cartog_index → suggests `cartog index` (graph), not `cartog rag index`.
    assert!(
        msg.contains("cartog index") && !msg.contains("cartog rag index"),
        "cartog_index refusal must suggest `cartog index`, got: {msg}"
    );
    // Drops the misleading "~5s" promise.
    assert!(
        !msg.contains("~5s"),
        "refusal must not promise an exact pickup latency, got: {msg}"
    );

    let err = reader
        .refuse_if_read_only("cartog_rag_index")
        .expect("read-only must refuse");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("read-only") && msg.contains("cartog_rag_index"),
        "error must name the gate and the tool, got: {msg}"
    );
    // cartog_rag_index → suggests `cartog rag index` (vectors).
    assert!(
        msg.contains("cartog rag index"),
        "cartog_rag_index refusal must suggest `cartog rag index`, got: {msg}"
    );

    let primary = CartogServer::new_with_provider(
        &db_path,
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::Primary,
    )
    .expect("primary reconstructs");
    assert!(
        primary.refuse_if_read_only("cartog_index").is_none(),
        "primary must NOT refuse"
    );
}

#[test]
fn cartog_update_is_registered_and_not_gated_read_only() {
    // cartog_update arms a machine-level deferred update, not a DB write,
    // so it must be available even on a read-only secondary. The router
    // lists it regardless of role, and the handler never consults
    // refuse_if_read_only for it.
    let names: Vec<String> = CartogServer::tool_router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "cartog_update"),
        "cartog_update must be registered, got: {names:?}"
    );

    let writers = ["cartog_index", "cartog_rag_index"];
    assert!(
        !writers.contains(&"cartog_update"),
        "cartog_update must NOT be in the DB-write set that refuse_if_read_only gates"
    );
}
