//! Index + LSP-gate behavior and promoter-task regression tests.

use super::test_provider;
use crate::*;

// ── Index + LSP gate tests ──

#[cfg(feature = "lsp")]
#[test]
fn index_with_optional_lsp_skips_lsp_on_noop_reindex() {
    // Regression guard for the MCP-side gate: when no file changed since
    // the previous index, the LSP pass MUST be skipped — otherwise we
    // re-spawn rust-analyzer / pyright on every cartog_index call.
    //
    // Copies the auth fixture to a tempdir so a real source edit can be
    // applied between calls without touching the repo.
    use cartog_db::Database;
    use cartog_lsp::manager::LspManager;

    let fixtures_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/auth");
    if !fixtures_src.exists() {
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let fixtures = tmp.path().join("auth");
    std::fs::create_dir_all(&fixtures).unwrap();
    for entry in std::fs::read_dir(&fixtures_src).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), fixtures.join(entry.file_name())).unwrap();
    }

    let db = Arc::new(Mutex::new(Database::open_memory().unwrap()));
    let lsp_mgr = Arc::new(Mutex::new(LspManager::new(&fixtures)));

    // First call primes the index. dirty_files > 0 → LSP is allowed (it may
    // resolve nothing if pyright isn't on PATH, but the gate must let it run).
    let r1 = index_with_optional_lsp(
        &db,
        &lsp_mgr,
        &fixtures,
        false,
        None,
        None,
        indexer::RedactionConfig::disabled(),
        &indexer::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert!(
        r1.dirty_files > 0,
        "first index must report dirty files (got {})",
        r1.dirty_files
    );

    // Second call without changes must be a no-op AND must skip LSP.
    let r2 = index_with_optional_lsp(
        &db,
        &lsp_mgr,
        &fixtures,
        false,
        None,
        None,
        indexer::RedactionConfig::disabled(),
        &indexer::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert_eq!(r2.dirty_files, 0);
    assert_eq!(
        r2.edges_lsp_resolved, 0,
        "no-op reindex must skip LSP (MCP-side gate broken)"
    );
    assert_eq!(
        r2.edges_marked_external, 0,
        "no-op reindex must not produce new external marks"
    );
    assert_eq!(r2.files_indexed, 0);
}

/// Tempdir project with one guaranteed-unresolvable Python call, indexed
/// with `lsp=false` so its edge is sealed at state=4.
#[cfg(feature = "lsp")]
fn sealed_py_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Arc<Mutex<cartog_db::Database>>,
) {
    let tmp = tempfile::tempdir().unwrap();
    // Tempdirs are dot-prefixed on macOS; is_ignored() rejects a dotted walk root.
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.py"), "def caller():\n    nowhere('x')\n").unwrap();

    let db = cartog_db::Database::open_memory().unwrap();
    indexer::index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        indexer::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &indexer::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert!(
        db.has_heuristic_exhausted().unwrap(),
        "no-lsp index must seal the unresolvable edge"
    );
    (tmp, root, Arc::new(Mutex::new(db)))
}

/// Manager whose python server is a binary that cannot exist, so the start
/// fails deterministically regardless of the host PATH.
#[cfg(feature = "lsp")]
fn no_server_manager(root: &Path) -> cartog_lsp::manager::LspManager {
    let overrides = std::collections::HashMap::from([(
        "python".to_string(),
        vec!["cartog-test-no-such-binary".to_string()],
    )]);
    cartog_lsp::manager::LspManager::with_overrides(root, overrides)
}

#[cfg(feature = "lsp")]
#[test]
fn warm_lsp_pass_reopens_and_reseals_without_servers() {
    // Regression for the #114 seal: `index_directory(lsp=false)` seals
    // unresolved edges at state=4 and the warm pass queries only state=0 —
    // without the reopen it silently resolved nothing.
    let (_tmp, root, db) = sealed_py_fixture();
    let db = db.lock().unwrap();
    let sealed = db.count_edges_in_state(4).unwrap();
    let mut mgr = no_server_manager(&root);

    let outcome = warm_lsp_pass(&db, &mut mgr, &root, None, None).unwrap();

    assert_eq!(
        outcome.reopened, sealed,
        "seals must reopen before the pass"
    );
    assert!(!outcome.stats.any_server_started);
    assert_eq!(outcome.resealed, sealed, "serverless pass must re-seal");
    assert_eq!(db.count_edges_in_state(4).unwrap(), sealed);
    assert_eq!(db.count_edges_in_state(0).unwrap(), 0);
}

#[cfg(feature = "lsp")]
#[test]
fn warm_lsp_pass_reseals_on_cancel() {
    // A cancelled pass has no surrounding tx on the MCP path: the reopen
    // must be undone or the no-op catch-up loses sight of the backlog.
    let (_tmp, root, db) = sealed_py_fixture();
    let db = db.lock().unwrap();
    let sealed = db.count_edges_in_state(4).unwrap();
    let mut mgr = no_server_manager(&root);

    let tripped: indexer::CancelProbe<'_> = &|| true;
    let err = warm_lsp_pass(&db, &mut mgr, &root, None, Some(tripped)).unwrap_err();

    assert!(
        format!("{err:?}").contains("cancelled"),
        "cancel must surface as an error, got: {err:?}"
    );
    assert_eq!(
        db.count_edges_in_state(4).unwrap(),
        sealed,
        "pre-pass seal must be restored on cancel"
    );
    assert_eq!(db.count_edges_in_state(0).unwrap(), 0);
}

#[cfg(feature = "lsp")]
#[test]
fn catch_up_runs_warm_pass_on_noop_index_with_sealed_backlog() {
    let (_tmp, root, db) = sealed_py_fixture();
    let latch = std::sync::atomic::AtomicBool::new(false);
    let mgr = Arc::new(Mutex::new(no_server_manager(&root)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let noop = indexer::IndexResult::default();
    let out = catch_up_lsp(&db, &mgr, &latch, &root, Some(tx), None, noop).unwrap();

    assert!(rx.try_recv().is_ok(), "catch-up must run the LSP phase");
    assert!(
        latch.load(std::sync::atomic::Ordering::Acquire),
        "serverless pass must latch lsp_unavailable"
    );
    assert!(
        db.lock().unwrap().has_heuristic_exhausted().unwrap(),
        "backlog must be re-sealed after a serverless pass"
    );
    assert_eq!(out.edges_lsp_resolved, 0);
}

#[cfg(feature = "lsp")]
#[test]
fn catch_up_skips_when_latched() {
    let (_tmp, root, db) = sealed_py_fixture();
    let latch = std::sync::atomic::AtomicBool::new(true);
    let mgr = Arc::new(Mutex::new(no_server_manager(&root)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let noop = indexer::IndexResult::default();
    catch_up_lsp(&db, &mgr, &latch, &root, Some(tx), None, noop).unwrap();

    assert!(
        rx.try_recv().is_err(),
        "latched server must not retry the catch-up"
    );
}

#[cfg(feature = "lsp")]
#[test]
fn catch_up_skips_dirty_runs() {
    // Dirty runs already did a global warm pass in index_with_optional_lsp.
    let (_tmp, root, db) = sealed_py_fixture();
    let latch = std::sync::atomic::AtomicBool::new(false);
    let mgr = Arc::new(Mutex::new(no_server_manager(&root)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let dirty = indexer::IndexResult {
        dirty_files: 1,
        ..Default::default()
    };
    catch_up_lsp(&db, &mgr, &latch, &root, Some(tx), None, dirty).unwrap();

    assert!(rx.try_recv().is_err(), "dirty run must skip the catch-up");
    assert!(!latch.load(std::sync::atomic::Ordering::Acquire));
}

#[cfg(feature = "lsp")]
#[test]
fn catch_up_noop_without_sealed_backlog() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let db = Arc::new(Mutex::new(cartog_db::Database::open_memory().unwrap()));
    let latch = std::sync::atomic::AtomicBool::new(false);
    let mgr = Arc::new(Mutex::new(no_server_manager(&root)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let noop = indexer::IndexResult::default();
    catch_up_lsp(&db, &mgr, &latch, &root, Some(tx), None, noop).unwrap();

    assert!(rx.try_recv().is_err(), "nothing sealed → no LSP phase");
    assert!(
        !latch.load(std::sync::atomic::Ordering::Acquire),
        "latch must not be set when the pass never ran"
    );
}

#[test]
fn pid_file_acquire_failure_propagates() {
    // Pointing pid_lock_dir at a regular file makes ProcessLock::acquire
    // fail at create_dir_all; the error must surface to the caller so
    // `cartog serve` aborts rather than silently running unlocked.
    let blocker = tempfile::NamedTempFile::new().unwrap();
    let opts = ServerOptions {
        pid_lock_dir: Some(blocker.path().to_path_buf()),
        pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
        ..Default::default()
    };
    let err = acquire_serve_lock(&opts).unwrap_err();
    assert!(
        err.to_string().contains("serve PID lock"),
        "error should mention the lock context, got: {err}"
    );
}

// ── Promoter regression tests (review fix M-promoter) ──

fn promoter_args_for_test(
    db: Arc<Mutex<Database>>,
    role: Arc<AtomicRole>,
    db_path: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    primary: cartog_process_lock::ActiveLock,
    pinned: Option<PinnedAttach>,
) -> PromoterArgs {
    promoter_args_with_slot(
        db,
        role,
        db_path,
        state_dir,
        primary,
        pinned,
        SERVE_LOCK_SLOT,
    )
}

fn promoter_args_with_slot(
    db: Arc<Mutex<Database>>,
    role: Arc<AtomicRole>,
    db_path: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    primary: cartog_process_lock::ActiveLock,
    pinned: Option<PinnedAttach>,
    serve_slot: &str,
) -> PromoterArgs {
    // Test embedding provider: the reconcile step on promotion needs
    // SOMETHING to fingerprint. The promoter-test DBs are opened fresh and
    // never reconciled, so reconcile takes the harmless backfill branch
    // (stamps mock provider/model) rather than wiping a populated index.
    // Mock avoids loading the real ONNX model.
    PromoterArgs {
        db,
        role,
        lock_cell: Arc::new(Mutex::new(None)),
        watch_cell: Arc::new(Mutex::new(None)),
        stale_cell: Arc::new(Mutex::new(None)),
        watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        embedding_provider: Arc::new(Mutex::new(test_provider())),
        db_path: db_path.clone(),
        state_dir,
        serve_slot: serve_slot.to_string(),
        watch_slot: serve_to_watch_slot(serve_slot).expect("test slot must be valid"),
        cwd: std::env::current_dir().unwrap(),
        primary,
        pinned,
        watch_requested: false,
        // Temp-dir fixtures: never touch the developer's real registry.
        register_on_promotion: false,
        rag_override: Some(false),
        rag_config: rag::EmbeddingProviderConfig::default(),
        redact: indexer::RedactionConfig::disabled(),
        walk_filter: indexer::WalkFilter::unrestricted(),
        // Very short for tests so the loop responds quickly.
        poll_interval: std::time::Duration::from_millis(20),
    }
}

#[tokio::test]
async fn promoter_abort_cancels_the_polling_task() {
    // Regression for review fix M-promoter (d): dropping the JoinHandle
    // does NOT cancel a tokio task — only abort() does. Without the
    // abort in run_server's shutdown path, the promoter could keep
    // polling for up to one poll_interval after run_server returns
    // and even promote during that window. We assert that abort()
    // really terminates the task within a small bounded time.
    let _serial = test_validate_call_counter::SERIAL.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let state_dir = dir.path().join("state");
    // Materialize a DB so open_readonly can attach.
    {
        let _ = Database::open(&db_path, 384).unwrap();
    }
    let db = Arc::new(Mutex::new(Database::open_readonly(&db_path).unwrap()));
    let role = Arc::new(AtomicRole::new(Role::ReadOnly));
    let pinned = db.lock().unwrap().pinned_attach().cloned();
    // Pretend the primary is our own process (so liveness reports
    // true and the promoter just keeps polling forever, never
    // promoting). This isolates the test to the abort behavior.
    let primary = cartog_process_lock::ActiveLock {
        slot: SERVE_LOCK_SLOT.to_string(),
        pid: std::process::id(),
        start_time: cartog_process_lock::process_start_time(std::process::id()),
    };
    let args = promoter_args_for_test(db, role, db_path, state_dir, primary, pinned);
    let handle = tokio::task::spawn(promoter_task(args));

    // Let it poll a few times.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert!(!handle.is_finished(), "promoter must keep polling");
    handle.abort();
    // Allow a brief moment for the runtime to cancel.
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    assert!(
        handle.is_finished(),
        "abort must terminate the promoter task"
    );
}

#[test]
fn validate_pinned_state_returns_ok_when_pin_is_none() {
    let _serial = test_validate_call_counter::SERIAL.blocking_lock();
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _ = Database::open(&db_path, 384).unwrap();
    }
    validate_pinned_state(&db_path, None).expect("no pin must validate trivially");
}

#[test]
fn validate_pinned_state_detects_none_vs_some_drift() {
    // A brand-new DB never reconciled has no provider/model in
    // metadata, so a read-only attach captures `pinned.embedding =
    // None`. If the primary subsequently runs `rag index` and stamps
    // provider/model, the secondary must detect this as drift — pin
    // was None, disk is now Some(...). This path is distinct from
    // `validate_pinned_state_detects_drift`, which seeds provider+
    // model BEFORE the attach so the pin is Some(...) and only the
    // Some-vs-Some inequality is exercised.
    let _serial = test_validate_call_counter::SERIAL.blocking_lock();
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pinned = {
        let _ = Database::open(&db_path, 384).unwrap();
        Database::open_readonly(&db_path)
            .unwrap()
            .pinned_attach()
            .cloned()
    };
    assert!(
        pinned.as_ref().is_some_and(|p| p.embedding.is_none()),
        "pin against an un-reconciled DB must capture embedding = None",
    );
    // Primary stamps provider+model.
    {
        let mutator = Database::open(&db_path, 384).unwrap();
        mutator.set_metadata("embedding_provider", "local").unwrap();
        mutator
            .set_metadata("embedding_model", "BGE-small-en-v1.5")
            .unwrap();
    }
    let err = validate_pinned_state(&db_path, pinned.as_ref())
        .expect_err("None pin vs Some on disk must surface as drift");
    assert!(
        err.to_string().contains("DB metadata changed"),
        "drift error should name the metadata change, got: {err}"
    );
}

#[test]
fn validate_pinned_state_detects_drift() {
    // Drift is exercised via the embedding fingerprint (not
    // schema_version) because `open_readonly` rejects schema_version
    // mismatch *before* this helper's comparison runs.
    //
    // We seed provider+model BEFORE the read-only attach so the pin
    // captures `embedding = Some(local, BGE-..., 384)`, then mutate
    // to a different `Some(ollama, nomic, 384)`. This exercises the
    // Some vs Some inequality directly — not the None vs Some accident
    // that would silently stop firing if `Database::open` ever seeded
    // default provider/model values.
    let _serial = test_validate_call_counter::SERIAL.blocking_lock();
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pinned = {
        let db = Database::open(&db_path, 384).unwrap();
        db.set_metadata("embedding_provider", "local").unwrap();
        db.set_metadata("embedding_model", "BGE-small-en-v1.5")
            .unwrap();
        drop(db);
        Database::open_readonly(&db_path)
            .unwrap()
            .pinned_attach()
            .cloned()
    };
    assert!(
        pinned.as_ref().and_then(|p| p.embedding.as_ref()).is_some(),
        "pin must capture Some(...) so the test exercises Some vs Some drift",
    );
    // Another writer rewrites provider + model under us.
    {
        let mutator = Database::open(&db_path, 384).unwrap();
        mutator
            .set_metadata("embedding_provider", "ollama")
            .unwrap();
        mutator
            .set_metadata("embedding_model", "nomic-embed-text")
            .unwrap();
    }
    let err = validate_pinned_state(&db_path, pinned.as_ref())
        .expect_err("divergent disk state must surface as Err");
    let msg = err.to_string();
    assert!(
        msg.contains("DB metadata changed"),
        "error message should name the drift, got: {msg}"
    );
}

#[tokio::test]
async fn promoter_acquires_db_scoped_slot_from_args() {
    // Regression: promoter_task must acquire the slot carried in
    // `args.serve_slot`, not a hardcoded `SERVE_LOCK_SLOT`. The
    // previous test scaffolding hardcoded the global slot, so a
    // refactor that reverts the acquire call to `SERVE_LOCK_SLOT`
    // would have shipped green. This test passes a non-global serve
    // slot and asserts the on-disk PID filename matches.
    let _serial = test_validate_call_counter::SERIAL.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    {
        let _ = Database::open(&db_path, 384).unwrap();
    }
    let reader_db = Database::open_readonly(&db_path).unwrap();
    let pinned = reader_db.pinned_attach().cloned();
    let db = Arc::new(Mutex::new(reader_db));
    let role = Arc::new(AtomicRole::new(Role::ReadOnly));
    // Dead primary holding the scoped slot we want the promoter to
    // claim. Slot name must match exactly what the promoter acquires
    // so the original `serve-fa11`.pid file (planted below) gets
    // reclaimed.
    let scoped_slot = "serve-fa11ed7e57c0fed5";
    let primary_pid_path = state_dir.join(format!("{scoped_slot}.pid"));
    std::fs::write(&primary_pid_path, "4194304\n0\n").unwrap();
    let primary = cartog_process_lock::ActiveLock {
        slot: scoped_slot.to_string(),
        pid: 4_194_304,
        start_time: None,
    };

    let args = promoter_args_with_slot(
        Arc::clone(&db),
        Arc::clone(&role),
        db_path,
        state_dir.clone(),
        primary,
        pinned,
        scoped_slot,
    );
    // Keep the lock_cell Arc alive past the task so the acquired
    // ProcessLock isn't dropped (and the PID file removed) before we
    // assert on it. The production run_server lifecycle does the
    // same: it holds lock_cell for the whole server lifetime.
    let lock_cell = Arc::clone(&args.lock_cell);

    let handle = tokio::task::spawn(promoter_task(args));
    // Give the promoter time to notice primary-gone and acquire.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert_eq!(
        role.load(),
        Role::Primary,
        "promoter must flip role after acquiring the scoped slot"
    );
    // The on-disk PID file must use the scoped slot, not the global.
    assert!(
        primary_pid_path.exists(),
        "expected promoter to acquire {primary_pid_path:?}"
    );
    let global_path = state_dir.join(format!("{SERVE_LOCK_SLOT}.pid"));
    assert!(
        !global_path.exists(),
        "promoter must NOT acquire the global slot ({global_path:?})"
    );

    // Drop the held lock explicitly so the temp dir teardown is clean.
    {
        let mut guard = lock_cell.lock().unwrap();
        *guard = None;
    }
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn promoter_spawns_watcher_with_db_scoped_watch_slot() {
    // Regression for review finding #2: promoter_task must claim the
    // watch slot carried in `args.watch_slot`, not a hardcoded
    // `WATCH_LOCK_SLOT`. The previous regression test only verified
    // the serve-slot acquire path; a refactor that reverts the
    // post-promotion watcher's `config.pid_lock_slot = Some(args
    // .watch_slot.clone())` to the global constant would have shipped
    // green. This test sets `watch_requested = true`, lets the
    // promoter spawn a watcher, and asserts the watcher claims the
    // scoped slot on disk.
    let _serial = test_validate_call_counter::SERIAL.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    {
        let _ = Database::open(&db_path, 384).unwrap();
    }
    let reader_db = Database::open_readonly(&db_path).unwrap();
    let pinned = reader_db.pinned_attach().cloned();
    let db = Arc::new(Mutex::new(reader_db));
    let role = Arc::new(AtomicRole::new(Role::ReadOnly));
    let scoped_slot = "serve-fa11ed7e57c0fed5";
    let primary_pid_path = state_dir.join(format!("{scoped_slot}.pid"));
    std::fs::write(&primary_pid_path, "4194304\n0\n").unwrap();
    let primary = cartog_process_lock::ActiveLock {
        slot: scoped_slot.to_string(),
        pid: 4_194_304,
        start_time: None,
    };

    let mut args = promoter_args_with_slot(
        Arc::clone(&db),
        Arc::clone(&role),
        db_path,
        state_dir.clone(),
        primary,
        pinned,
        scoped_slot,
    );
    // Enable the watcher-spawn path. RAG stays off and watch root
    // (cwd, captured by the helper) is the cartog crate dir — fine
    // for spawning the watcher; we tear it down immediately.
    args.watch_requested = true;
    let lock_cell = Arc::clone(&args.lock_cell);
    let watch_cell = Arc::clone(&args.watch_cell);

    let handle = tokio::task::spawn(promoter_task(args));

    // The expected watcher PID file uses the SCOPED watch slot derived
    // via `serve_to_watch_slot(scoped_slot)`.
    let expected_watch_slot = serve_to_watch_slot(scoped_slot).expect("scoped slot is valid");
    let expected_watch_pid = state_dir.join(format!("{expected_watch_slot}.pid"));
    let global_watch_pid = state_dir.join(format!("{}.pid", watch::WATCH_LOCK_SLOT));

    // Poll for up to 5s in 50ms increments — the watcher startup walks
    // the cwd, sweeps stale locks, and creates the debouncer; on a
    // loaded CI machine the cumulative cost can exceed any small fixed
    // sleep, so a fixed sleep here was a flake source.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !expected_watch_pid.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        expected_watch_pid.exists(),
        "watcher should have claimed the scoped slot at {expected_watch_pid:?}"
    );
    assert!(
        !global_watch_pid.exists(),
        "watcher must NOT claim the global slot ({global_watch_pid:?})"
    );

    // Clean shutdown so the temp dir teardown succeeds.
    {
        let mut wguard = watch_cell.lock().unwrap();
        if let Some(handle) = wguard.take() {
            handle.stop();
        }
    }
    {
        let mut guard = lock_cell.lock().unwrap();
        *guard = None;
    }
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn promoter_aborts_when_state_diverges_after_acquire() {
    // Integration smoke. The post-acquire branch logic is covered
    // by `validate_pinned_state_detects_drift`; this test verifies
    // the promoter wires drift detection to a clean exit (role stays
    // ReadOnly, task finishes).
    let _serial = test_validate_call_counter::SERIAL.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    {
        let _ = Database::open(&db_path, 384).unwrap();
    }
    let reader_db = Database::open_readonly(&db_path).unwrap();
    let pinned = reader_db.pinned_attach().cloned();
    let db = Arc::new(Mutex::new(reader_db));
    let role = Arc::new(AtomicRole::new(Role::ReadOnly));
    // Pretend primary is dead (no such PID).
    let primary = cartog_process_lock::ActiveLock {
        slot: SERVE_LOCK_SLOT.to_string(),
        pid: 4_194_304,
        start_time: None,
    };
    // Mutate the DB metadata under the reader.
    {
        let mutator = Database::open(&db_path, 384).unwrap();
        mutator.set_metadata("schema_version", "9999").unwrap();
    }

    let args = promoter_args_for_test(
        Arc::clone(&db),
        Arc::clone(&role),
        db_path,
        state_dir,
        primary,
        pinned,
    );
    let handle = tokio::task::spawn(promoter_task(args));
    // Give the promoter one tick.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    // The promoter should have noticed primary-gone, validated, seen
    // drift, and exited (return). Role must NOT be Primary.
    assert!(handle.is_finished(), "promoter must exit on drift");
    let _ = handle.await;
    assert_eq!(
        role.load(),
        Role::ReadOnly,
        "drifted DB must not flip role to Primary"
    );
}

#[tokio::test]
async fn promoter_loops_on_transient_open_failure() {
    // Regression for review fix M-promoter (b): pre-fix, an
    // open_existing_rw failure caused the promoter to `return`,
    // disabling promotion forever even if the next poll would
    // succeed. The fix loops on transient failures.
    //
    // We can exercise the "open fails -> loop" path by deleting the
    // DB file entirely between the validate and the open_existing_rw
    // call. open_existing_rw will fail; the promoter should drop
    // the lock and try again on the next tick (where it'll fail
    // validation, since the DB is missing, and exit cleanly).
    //
    // The key contract is: we don't return on the first
    // open_existing_rw failure — we drop the lock and loop. We
    // assert that by checking the lock file does not persist after
    // a failed promotion attempt.
    let _serial = test_validate_call_counter::SERIAL.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    {
        let _ = Database::open(&db_path, 384).unwrap();
    }
    let reader = Database::open_readonly(&db_path).unwrap();
    let pinned = reader.pinned_attach().cloned();
    let db = Arc::new(Mutex::new(reader));
    let role = Arc::new(AtomicRole::new(Role::ReadOnly));
    let primary = cartog_process_lock::ActiveLock {
        slot: SERVE_LOCK_SLOT.to_string(),
        pid: 4_194_304,
        start_time: None,
    };

    let args = promoter_args_for_test(
        Arc::clone(&db),
        Arc::clone(&role),
        db_path.clone(),
        state_dir.clone(),
        primary,
        pinned,
    );
    let handle = tokio::task::spawn(promoter_task(args));
    // Give the loop a moment to enter its first tick, then yank the DB.
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    std::fs::remove_file(&db_path).unwrap();
    // The promoter should now either: (a) loop on validate-failure
    // and never acquire, or (b) acquire then fail open and drop the
    // lock + loop. Either way the role stays ReadOnly and the lock
    // file is not left behind.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    handle.abort();
    let _ = handle.await;
    assert_eq!(
        role.load(),
        Role::ReadOnly,
        "promoter must not flip role under transient failure"
    );
    let lock_path = state_dir.join("serve.pid");
    assert!(
        !lock_path.exists(),
        "promoter must release the lock on failure (not strand serve.pid)"
    );
}

#[tokio::test]
async fn promoter_runs_validate_pinned_state_both_before_and_after_acquire() {
    // Regression for the M-promoter (c) fix at the test layer: the
    // promoter must call `validate_pinned_state` BOTH before and
    // after `ProcessLock::acquire`. We need an assertion that catches
    // deleting EITHER call site:
    //
    // - `calls >= 2` alone is insufficient: pre-acquire validate
    //   bumps on every tick, so over multiple ticks the count climbs
    //   past 2 even if the post-acquire site is gone.
    // - `role == Primary` alone is insufficient: the role swap
    //   succeeds even if pre-acquire validate is skipped entirely.
    //
    // Asserting BOTH catches either deletion: post-acquire validate
    // gates the role swap (so Primary => post-acquire ran on at
    // least one tick), and we additionally require `calls >= 2` to
    // prove a second validate fired in the promote path.
    let _serial = test_validate_call_counter::SERIAL.lock().await;
    test_validate_call_counter::reset();

    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    {
        let _ = Database::open(&db_path, 384).unwrap();
    }
    let reader = Database::open_readonly(&db_path).unwrap();
    let pinned = reader.pinned_attach().cloned();
    let db = Arc::new(Mutex::new(reader));
    let role = Arc::new(AtomicRole::new(Role::ReadOnly));
    // Pretend the primary is dead so the promoter exits the
    // primary-alive `continue` branch and reaches the validate calls.
    let primary = cartog_process_lock::ActiveLock {
        slot: SERVE_LOCK_SLOT.to_string(),
        pid: 4_194_304,
        start_time: None,
    };

    let args = promoter_args_for_test(
        Arc::clone(&db),
        Arc::clone(&role),
        db_path,
        state_dir,
        primary,
        pinned,
    );
    let handle = tokio::task::spawn(promoter_task(args));

    // Bounded poll instead of a fixed wall-clock sleep: keeps the
    // test fast on a healthy box and tolerates a stalled CI runner
    // up to the 2s deadline. We wait for BOTH conditions before
    // asserting — reading them once after a fixed sleep was the
    // source of the previous CI-flake risk.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if role.load() == Role::Primary && test_validate_call_counter::snapshot() >= 2 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    handle.abort();
    let _ = handle.await;

    let calls = test_validate_call_counter::snapshot();
    assert_eq!(
        role.load(),
        Role::Primary,
        "post-acquire validate gates the role swap; missing role flip implies the post-acquire site never ran"
    );
    assert!(
        calls >= 2,
        "validate_pinned_state must run twice per promotion tick (pre + post acquire), saw {calls}"
    );
}
