//! Loom model-checking harnesses for cartog's concurrent protocols.
//!
//! Loom exhaustively explores every thread interleaving AND every memory
//! reordering the C11 model permits, then checks the invariant on each. A
//! green run is a proof (over the model) that no schedule violates it — the
//! in-process memory-ordering layer that `specs/tla/Election.tla` cannot see.
//!
//! These harnesses MIRROR the production order rather than instrumenting the
//! real types, so the production crates stay untouched. This crate lives apart
//! from `cartog-mcp` on purpose: building under `--cfg loom` makes tokio gate
//! off `tokio::signal` (which `cartog-mcp` uses), so the loom cfg must not
//! reach that crate's dependency graph.
//!
//! Run: `RUSTFLAGS="--cfg loom" cargo test -p cartog-loom-models`
//! Without `--cfg loom` this crate is empty (everything is `#[cfg(loom)]`).

#![cfg(loom)]

#[cfg(test)]
mod single_writer {
    use loom::sync::atomic::{AtomicU8, Ordering};
    use loom::sync::{Arc, Mutex};
    use loom::thread;

    // Mirrors `AtomicRole`'s encoding in cartog-mcp (Primary = 0, ReadOnly = 1).
    // Hand-copied, not imported, to keep `--cfg loom` out of cartog-mcp's
    // (tokio::signal-using) graph; if that encoding ever changes, update here.
    const PRIMARY: u8 = 0;
    const READ_ONLY: u8 = 1;

    // Stand-in for "which connection the DB mutex holds": 0 = read-only attach,
    // 1 = RW (post-promotion). The production cell is `Arc<Mutex<Database>>`; we
    // model only the bit the handler invariant depends on.
    const DB_RO: u8 = 0;
    const DB_RW: u8 = 1;

    /// Promoter commit vs a concurrent write-tool handler.
    ///
    /// Faithful to (by symbol, not line number — line numbers rot):
    /// - promoter commit tail (`single_writer.rs::promoter_task`): swap the DB
    ///   mutex to RW first; `role.store(Primary)` happens LAST.
    /// - handler gate (`CartogServer::refuse_if_read_only`): `role.load()` then
    ///   `db.lock()`.
    /// - `AtomicRole`: store = Release, load = Acquire.
    ///
    /// SCOPE: this model refines ONE conjunct of Election.tla's
    /// `LockMatchesPrimary` / `PrimaryStateConsistent` — that a handler which
    /// observes `Role::Primary` finds the DB cell already holding the RW
    /// connection. It does NOT model `lock_cell`/`watch_cell`/`stale_cell`, so
    /// it does not cover the lock-vs-primary or watcher parts those TLA
    /// invariants also constrain (justified: no `refuse_if_read_only`-gated
    /// write handler reads those cells after the role check — verified).
    ///
    /// WHAT IT GUARDS: the COMMIT ORDER — the DB swap before the role store.
    /// Verified to discriminate: reordering them makes loom fail. It does NOT
    /// isolate the role atomic's Release/Acquire, because the DB Mutex's own
    /// unlock(Release)/lock(Acquire) already carries the happens-before here, so
    /// weakening the role atomic to Relaxed still passes. The Release/Acquire on
    /// `AtomicRole` is therefore defensive in today's code (load-bearing only if
    /// the DB cell ever stops being a Mutex).
    #[test]
    fn handler_seeing_primary_finds_rw_db() {
        loom::model(|| {
            let role = Arc::new(AtomicU8::new(READ_ONLY));
            let db = Arc::new(Mutex::new(DB_RO));

            // --- promoter commit (single_writer.rs::promoter_task tail) ---
            let pr_role = Arc::clone(&role);
            let pr_db = Arc::clone(&db);
            let promoter = thread::spawn(move || {
                // (1) swap DB to RW; the mutex guard drops at the end of this
                //     block — exactly as the production `match args.db.lock()`
                //     guard drops before the role store.
                {
                    let mut guard = pr_db.lock().unwrap();
                    *guard = DB_RW;
                }
                // (2)-(3) install lock_cell / watch_cell: not read by the handler
                //         gate, so omitted from this model.
                // (4) flip role LAST, Release (AtomicRole::store).
                pr_role.store(PRIMARY, Ordering::Release);
            });

            // --- write-tool handler (lib.rs refuse_if_read_only + db.lock) ---
            let h_role = Arc::clone(&role);
            let h_db = Arc::clone(&db);
            let handler = thread::spawn(move || {
                // refuse_if_read_only: load role with Acquire (AtomicRole::load).
                if h_role.load(Ordering::Acquire) == PRIMARY {
                    // Proceeds to write -> must observe the RW connection.
                    let conn = *h_db.lock().unwrap();
                    assert_eq!(
                        conn, DB_RW,
                        "handler observed Role::Primary but the DB cell was still \
                     read-only: the promoter commit order regressed (the DB swap \
                     must happen before the role store)"
                    );
                }
                // else ReadOnly -> refuse_if_read_only returns the error, no
                // write happens. Always safe.
            });

            promoter.join().unwrap();
            handler.join().unwrap();
        });
    }
} // mod single_writer
