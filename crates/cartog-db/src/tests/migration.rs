use crate::*;

// ── Embedding dimension migration tests ──

#[test]
fn test_open_stores_embedding_dimension() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let db = Database::open(&db_path, 384).unwrap();
    let stored: String = db
        .get_metadata("embedding_dimension")
        .unwrap()
        .expect("dimension should be stored");
    assert_eq!(stored, "384");
}

#[test]
fn test_open_with_different_dimension_clears_embeddings() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // First open with 384-dim
    {
        let db = Database::open(&db_path, 384).unwrap();
        let sym = Symbol::new("foo", SymbolKind::Function, "a.py", 1, 10, 0, 100, None);
        db.insert_symbol(&sym).unwrap();
        db.upsert_symbol_content(&sym.id, "foo", "def foo():", "header")
            .unwrap();
        let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
        let bytes = vec![0u8; 384 * 4];
        db.insert_embeddings(&[(eid, bytes)]).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 1);
    }

    // Reopen with 768-dim — should auto-wipe embeddings
    {
        let db = Database::open(&db_path, 768).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 0);
        let stored: String = db
            .get_metadata("embedding_dimension")
            .unwrap()
            .expect("dimension should be updated");
        assert_eq!(stored, "768");
    }
}

#[test]
fn test_open_same_dimension_preserves_embeddings() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // First open
    {
        let db = Database::open(&db_path, 384).unwrap();
        let sym = Symbol::new("bar", SymbolKind::Function, "b.py", 1, 10, 0, 100, None);
        db.insert_symbol(&sym).unwrap();
        db.upsert_symbol_content(&sym.id, "bar", "def bar():", "header")
            .unwrap();
        let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
        let bytes = vec![0u8; 384 * 4];
        db.insert_embeddings(&[(eid, bytes)]).unwrap();
    }

    // Reopen with same dimension — embeddings preserved
    {
        let db = Database::open(&db_path, 384).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 1);
    }
}

#[test]
fn test_default_dim_preserves_stored_non_default() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // First open with non-default dimension (e.g. Ollama auto-detected 768)
    {
        let db = Database::open(&db_path, 768).unwrap();
        let sym = Symbol::new("baz", SymbolKind::Function, "c.py", 1, 10, 0, 100, None);
        db.insert_symbol(&sym).unwrap();
        db.upsert_symbol_content(&sym.id, "baz", "def baz():", "header")
            .unwrap();
        let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
        let bytes = vec![0u8; 768 * 4];
        db.insert_embeddings(&[(eid, bytes)]).unwrap();
    }

    // Reopen with DEFAULT_EMBEDDING_DIM (384) — must preserve 768-dim embeddings
    {
        let db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 1);
        let stored: i64 = db
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, 768);
    }
}

#[test]
fn test_explicit_non_default_dim_wipes_different_stored() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // First open with 768
    {
        let db = Database::open(&db_path, 768).unwrap();
        let sym = Symbol::new("qux", SymbolKind::Function, "d.py", 1, 10, 0, 100, None);
        db.insert_symbol(&sym).unwrap();
        db.upsert_symbol_content(&sym.id, "qux", "def qux():", "header")
            .unwrap();
        let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
        let bytes = vec![0u8; 768 * 4];
        db.insert_embeddings(&[(eid, bytes)]).unwrap();
    }

    // Reopen with explicit 1536 — this IS a real dimension change, must wipe
    {
        let db = Database::open(&db_path, 1536).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 0);
    }
}

#[test]
fn test_reopen_same_dim_does_not_rewrite_metadata() {
    // True early-return guarantee: when stored dim already matches the
    // requested dim, `handle_embedding_dimension` should not touch the
    // metadata table. We assert this by snapshotting the row's content
    // before and after re-open and verifying no write occurred (rowid
    // would advance on INSERT OR REPLACE).
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let _db = Database::open(&db_path, 384).unwrap();

    let rowid_before: i64 = {
        let conn = Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT rowid FROM metadata WHERE key = 'embedding_dimension'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };

    let _db = Database::open(&db_path, 384).unwrap();

    let rowid_after: i64 = {
        let conn = Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT rowid FROM metadata WHERE key = 'embedding_dimension'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };

    // INSERT OR REPLACE assigns a new rowid; identity here proves we
    // skipped the write entirely.
    assert_eq!(
        rowid_before, rowid_after,
        "same-dim reopen should not rewrite the embedding_dimension row"
    );
}

#[test]
fn test_retry_busy_returns_on_non_busy_error() {
    // A non-busy error should propagate immediately, no retries.
    let attempts = std::cell::Cell::new(0);
    let result = retry_busy(|| -> std::result::Result<(), rusqlite::Error> {
        attempts.set(attempts.get() + 1);
        Err(rusqlite::Error::InvalidQuery)
    });
    assert!(matches!(result, Err(rusqlite::Error::InvalidQuery)));
    assert_eq!(attempts.get(), 1, "non-busy errors must not retry");
}

#[test]
fn test_retry_busy_succeeds_after_transient_busy() {
    // Simulate a writer that returns BUSY on the first call and Ok on the second.
    let attempts = std::cell::Cell::new(0);
    let result = retry_busy(|| -> std::result::Result<u32, rusqlite::Error> {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ErrorCode::DatabaseBusy,
                    extended_code: 5,
                },
                Some("database is locked".to_string()),
            ))
        } else {
            Ok(42)
        }
    });
    assert_eq!(result.unwrap(), 42);
    assert_eq!(attempts.get(), 2);
}

#[test]
fn test_retry_busy_exhausts_and_propagates() {
    // After backoff schedule is exhausted, the original BUSY error must surface.
    let attempts = std::cell::Cell::new(0);
    let result = retry_busy(|| -> std::result::Result<(), rusqlite::Error> {
        attempts.set(attempts.get() + 1);
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy,
                extended_code: 5,
            },
            Some("database is locked".to_string()),
        ))
    });
    assert!(matches!(
        result,
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy,
                ..
            },
            _
        ))
    ));
    // 1 initial call + MIGRATION_RETRY_BACKOFF_MS.len() retries
    assert_eq!(attempts.get(), MIGRATION_RETRY_BACKOFF_MS.len() + 1);
}

// ── Embedding fingerprint tests (Phase 6b) ──

fn fp(provider: &str, model: &str, dim: usize) -> EmbeddingFingerprint {
    EmbeddingFingerprint {
        provider: provider.to_string(),
        model: model.to_string(),
        dimension: dim,
    }
}

fn seed_embedding(db: &Database, dim: usize, sym_name: &str) {
    let sym = Symbol::new(sym_name, SymbolKind::Function, "f.py", 1, 10, 0, 100, None);
    db.insert_symbol(&sym).unwrap();
    db.upsert_symbol_content(&sym.id, sym_name, "def f():", "header")
        .unwrap();
    let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
    let bytes = vec![0u8; dim * 4];
    db.insert_embeddings(&[(eid, bytes)]).unwrap();
}

#[test]
fn test_fingerprint_match_is_noop() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path, 384).unwrap();
    let f = fp("local", "BGE-small-en-v1.5", 384);
    db.reconcile_embedding_fingerprint(&f).unwrap();
    seed_embedding(&db, 384, "foo");
    // Reconciling identical fingerprint must preserve embeddings.
    db.reconcile_embedding_fingerprint(&f).unwrap();
    assert_eq!(db.embedding_count().unwrap(), 1);
}

#[test]
fn test_fingerprint_provider_swap_wipes() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path, 384).unwrap();
    let f1 = fp("local", "BGE-small-en-v1.5", 384);
    db.reconcile_embedding_fingerprint(&f1).unwrap();
    seed_embedding(&db, 384, "bar");
    assert_eq!(db.embedding_count().unwrap(), 1);

    // Same dim + model name, different provider class → wipe.
    let f2 = fp("ollama", "BGE-small-en-v1.5", 384);
    db.reconcile_embedding_fingerprint(&f2).unwrap();
    assert_eq!(db.embedding_count().unwrap(), 0);
    assert_eq!(
        db.get_metadata("embedding_provider").unwrap().as_deref(),
        Some("ollama")
    );
}

#[test]
fn test_fingerprint_model_swap_wipes() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path, 384).unwrap();
    let f1 = fp("local", "BGE-small-en-v1.5", 384);
    db.reconcile_embedding_fingerprint(&f1).unwrap();
    seed_embedding(&db, 384, "baz");
    assert_eq!(db.embedding_count().unwrap(), 1);

    // Same provider + dim, different model → still a swap, must wipe.
    let f2 = fp("local", "AllMiniLML6V2", 384);
    db.reconcile_embedding_fingerprint(&f2).unwrap();
    assert_eq!(db.embedding_count().unwrap(), 0);
    assert_eq!(
        db.get_metadata("embedding_model").unwrap().as_deref(),
        Some("AllMiniLML6V2")
    );
}

#[test]
fn test_fingerprint_backfill_does_not_wipe() {
    // Simulate an older cartog DB: dimension recorded, provider/model not yet.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path, 384).unwrap();
    seed_embedding(&db, 384, "qux");
    assert!(db.get_metadata("embedding_provider").unwrap().is_none());
    assert_eq!(db.embedding_count().unwrap(), 1);

    // First reconcile after upgrade: backfill provider/model without wiping.
    let f = fp("local", "BGE-small-en-v1.5", 384);
    db.reconcile_embedding_fingerprint(&f).unwrap();
    assert_eq!(
        db.embedding_count().unwrap(),
        1,
        "backfill must preserve existing embeddings"
    );
    assert_eq!(
        db.get_metadata("embedding_provider").unwrap().as_deref(),
        Some("local")
    );
    assert_eq!(
        db.get_metadata("embedding_model").unwrap().as_deref(),
        Some("BGE-small-en-v1.5")
    );
}

#[test]
fn test_fingerprint_dim_change_wipes() {
    // A real dimension change must wipe even if provider/model also change.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path, 384).unwrap();
    let f1 = fp("local", "BGE-small-en-v1.5", 384);
    db.reconcile_embedding_fingerprint(&f1).unwrap();
    seed_embedding(&db, 384, "quux");
    assert_eq!(db.embedding_count().unwrap(), 1);

    let f2 = fp("local", "BGELargeENV15", 1024);
    db.reconcile_embedding_fingerprint(&f2).unwrap();
    assert_eq!(db.embedding_count().unwrap(), 0);
    let stored_dim: i64 = db
        .conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_dim, 1024);
    // A successful wipe must also recreate symbol_vec at the new dim.
    // Without this assertion, an early return between the DROP and the
    // CREATE in reconcile_embedding_fingerprint would pass the count +
    // metadata checks above while leaving the DB unusable for RAG.
    assert!(
        symbol_vec_exists(&db.conn).unwrap(),
        "successful reconcile must recreate symbol_vec"
    );
}

// ── Read-only attach tests (Phase 3) ──

#[test]
fn test_open_readonly_succeeds_and_marks_read_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // Primary creates and writes a fingerprint.
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.reconcile_embedding_fingerprint(&fp("local", "BGE-small-en-v1.5", 384))
            .unwrap();
        seed_embedding(&db, 384, "foo");
    }

    // Reader attaches read-only.
    let reader = Database::open_readonly(&db_path).unwrap();
    assert!(reader.is_read_only(), "open_readonly must set the flag");
    let pinned = reader.pinned_attach().expect("read-only attach pins state");
    assert_eq!(pinned.schema_version, SCHEMA_VERSION);
    assert_eq!(
        pinned.embedding,
        Some(fp("local", "BGE-small-en-v1.5", 384))
    );
}

#[test]
fn test_open_readonly_can_query_existing_data() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    {
        let db = Database::open(&db_path, 384).unwrap();
        let sym = Symbol::new(
            "callable",
            SymbolKind::Function,
            "a.py",
            1,
            10,
            0,
            100,
            None,
        );
        db.insert_symbol(&sym).unwrap();
    }

    let reader = Database::open_readonly(&db_path).unwrap();
    let count: i64 = reader
        .conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "reader sees primary's data");
}

#[test]
fn test_open_readonly_refuses_writes() {
    // SQLITE_OPEN_READ_ONLY must turn any INSERT into SQLITE_READONLY at
    // runtime — defense-in-depth for the higher-level tool gating in
    // Phase 4.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _db = Database::open(&db_path, 384).unwrap();
    }

    let reader = Database::open_readonly(&db_path).unwrap();
    let err = reader
        .conn
        .execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES ('x', 'y')",
            [],
        )
        .unwrap_err();
    // The specific code is SQLITE_READONLY (8); rusqlite surfaces it as
    // SqliteFailure with the matching error code variant. We just check
    // that some error came back rather than match on the FFI integer.
    let msg = err.to_string();
    assert!(
        msg.contains("read") || msg.contains("readonly") || msg.contains("write"),
        "read-only DB write should fail with a read-only-flavored error, got: {msg}"
    );
}

#[test]
fn test_open_readonly_detects_schema_drift() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        // Simulate a future cartog: bump schema_version on disk.
        db.set_metadata("schema_version", "9999").unwrap();
    }

    let err = Database::open_readonly(&db_path).unwrap_err();
    match err {
        DbError::SchemaDrift { expected, stored } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(stored, 9999);
        }
        other => panic!("expected SchemaDrift, got {other:?}"),
    }
}

#[test]
fn test_open_readonly_does_not_run_migrations() {
    // After open_readonly returns, no PRAGMAs or writes should have
    // landed beyond what was there before. We test the visible
    // consequence: an existing user-set metadata key is unchanged.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.set_metadata("user_marker", "untouched").unwrap();
    }
    let _reader = Database::open_readonly(&db_path).unwrap();
    // Re-open writable to verify the marker is still there and the
    // schema didn't get rewritten.
    let primary = Database::open(&db_path, 384).unwrap();
    assert_eq!(
        primary.get_metadata("user_marker").unwrap().as_deref(),
        Some("untouched")
    );
}

#[test]
fn test_open_default_is_not_read_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path, 384).unwrap();
    assert!(!db.is_read_only());
    assert!(db.pinned_attach().is_none());
}

// ── Promotion path: open_existing_rw (Phase 5) ──

#[test]
fn test_open_existing_rw_opens_writable_and_skips_migrations() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    // Materialize with a user-set metadata marker we can re-read.
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.set_metadata("marker", "preserved").unwrap();
    }

    let promoted = Database::open_existing_rw(&db_path).unwrap();
    assert!(!promoted.is_read_only(), "open_existing_rw is RW");
    assert!(promoted.pinned_attach().is_none(), "RW opens have no pin");
    // The marker survives (we didn't wipe anything).
    assert_eq!(
        promoted.get_metadata("marker").unwrap().as_deref(),
        Some("preserved")
    );
    // We can write — confirming it's a real RW handle.
    promoted.set_metadata("write_check", "ok").unwrap();
}

#[test]
fn test_open_existing_rw_detects_schema_drift() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.set_metadata("schema_version", "9999").unwrap();
    }
    let err = Database::open_existing_rw(&db_path).unwrap_err();
    match err {
        DbError::SchemaDrift { expected, stored } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(stored, 9999);
        }
        other => panic!("expected SchemaDrift, got {other:?}"),
    }
}

#[test]
fn test_database_open_alone_does_not_change_fingerprint() {
    // Regression for the cartog rag search path: opening the DB (which
    // every CLI command does) must not touch the embedding fingerprint
    // unless reconcile_embedding_fingerprint is explicitly called.
    // Pre-fix, cmd_rag_search called reconcile on every invocation,
    // which could race a primary serve's writes if the user changed
    // provider in .cartog.toml since last index. After the fix,
    // cmd_rag_search opens RW but does NOT reconcile.
    //
    // This test asserts the invariant at the layer below: Database::open
    // does not, by itself, alter provider/model metadata. Combined with
    // the production code change (no reconcile call in cmd_rag_search),
    // a CLI search invocation cannot wipe symbol_vec.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let original_fp = fp("local", "BGE-small-en-v1.5", 384);
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.reconcile_embedding_fingerprint(&original_fp).unwrap();
        seed_embedding(&db, 384, "guard");
    }
    // Re-open as cmd_rag_search would (RW, no reconcile). Same dim,
    // so handle_embedding_dimension early-returns; nothing rewrites.
    {
        let _db = Database::open(&db_path, 384).unwrap();
    }
    // Fingerprint and embeddings intact.
    let db = Database::open(&db_path, 384).unwrap();
    assert_eq!(
        db.get_metadata("embedding_provider").unwrap().as_deref(),
        Some("local")
    );
    assert_eq!(
        db.get_metadata("embedding_model").unwrap().as_deref(),
        Some("BGE-small-en-v1.5")
    );
    assert_eq!(db.embedding_count().unwrap(), 1);
}

#[test]
fn test_open_readonly_missing_schema_version_is_schema_drift() {
    // Regression: pre-fix, a metadata table without a schema_version
    // row surfaced as DbError::Sqlite(QueryReturnedNoRows) instead of
    // the actionable SchemaDrift. Callers (cartog serve) print
    // different messages for the two — drift is the right one ("the
    // primary upgraded cartog; restart this session"), the raw
    // rusqlite error is opaque.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    // Create a DB with our schema, then delete the schema_version row.
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.conn
            .execute("DELETE FROM metadata WHERE key = 'schema_version'", [])
            .unwrap();
    }
    let err = Database::open_readonly(&db_path).unwrap_err();
    match err {
        DbError::SchemaDrift { expected, stored } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(stored, 0, "missing row should surface as stored=0");
        }
        other => panic!("expected SchemaDrift, got {other:?}"),
    }
}

#[test]
fn test_open_readonly_missing_metadata_table_is_schema_drift() {
    // Regression: a non-cartog SQLite file at the path (or a
    // partially-initialised DB where the `metadata` table is missing
    // entirely) used to surface as a raw rusqlite "no such table:
    // metadata" error instead of the actionable SchemaDrift. Fix:
    // read_schema_version catches that specific SqliteFailure and
    // returns stored=0.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    // Build a SQLite file that's NOT a cartog DB: empty schema.
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE unrelated (x INTEGER);")
            .unwrap();
    }
    let err = Database::open_readonly(&db_path).unwrap_err();
    match err {
        DbError::SchemaDrift { expected, stored } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(stored, 0, "missing metadata table should be stored=0");
        }
        other => panic!("expected SchemaDrift, got {other:?}"),
    }
}

#[test]
fn test_open_existing_rw_missing_schema_version_is_schema_drift() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.conn
            .execute("DELETE FROM metadata WHERE key = 'schema_version'", [])
            .unwrap();
    }
    let err = Database::open_existing_rw(&db_path).unwrap_err();
    match err {
        DbError::SchemaDrift { expected, stored } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(stored, 0);
        }
        other => panic!("expected SchemaDrift, got {other:?}"),
    }
}

#[test]
fn test_reconcile_rebuilds_when_metadata_matches_but_symbol_vec_missing() {
    // Defensive regression: if `symbol_vec` is missing for any reason
    // (external corruption, pre-C4 cartog that crashed mid-migration)
    // but metadata still claims the matching fingerprint, the fast-
    // path early return previously skipped the rebuild, leaving the
    // DB stuck. After the fix, the symbol_vec_exists() check forces
    // a rebuild.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let f = fp("local", "BGE-small-en-v1.5", 384);

    // 1. Establish a normal state.
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.reconcile_embedding_fingerprint(&f).unwrap();
    }

    // 2. Drop the vector table out-of-band, simulating corruption.
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.conn
            .execute("DROP TABLE IF EXISTS symbol_vec", [])
            .unwrap();
        // Metadata unchanged: still claims (local, BGE-small-en-v1.5, 384).
        assert_eq!(
            db.get_metadata("embedding_dimension").unwrap().as_deref(),
            Some("384")
        );
    }

    // 3. Re-reconcile with the same fingerprint. Pre-fix: early-return
    //    skipped rebuild → symbol_vec stayed missing forever. Post-fix:
    //    the symbol_vec_exists() check forces the rebuild.
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.reconcile_embedding_fingerprint(&f).unwrap();
        let exists: bool = db
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE name='symbol_vec'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_some();
        assert!(
            exists,
            "reconcile must rebuild symbol_vec when missing, even on metadata match"
        );
    }
}

#[test]
fn test_handle_embedding_dimension_rebuilds_when_symbol_vec_missing() {
    // Same defensive guarantee for the lower-level handle_embedding_dimension
    // fast-path. Open a DB, drop symbol_vec, re-open: the table must come
    // back even though stored_dim == requested_dim.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.conn
            .execute("DROP TABLE IF EXISTS symbol_vec", [])
            .unwrap();
    }
    let db = Database::open(&db_path, 384).unwrap();
    let exists: bool = db
        .conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name='symbol_vec'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .unwrap()
        .is_some();
    assert!(
        exists,
        "Database::open must rebuild symbol_vec when missing, even on metadata match"
    );
}

#[test]
fn test_reconcile_fingerprint_rolls_back_on_midsequence_failure() {
    // Regression: pre-fix, each metadata write in
    // reconcile_embedding_fingerprint ran outside any transaction.
    // If the busy-retry on a later write exhausted (or any other
    // failure), the DB was left with partial state — e.g.
    // symbol_vec dropped, provider rewritten, dimension stale. The
    // next open would see (stored_dim != fp.dimension) → "wipe and
    // rebuild" but the embeddings would already be gone, and the
    // primary writer would silently keep operating against the
    // damaged DB.
    //
    // With the transaction wrapper, a mid-sequence failure rolls
    // back the entire reconcile. We exercise this by capping
    // max_page_count so a write in the middle of the sequence
    // fails with SQLITE_FULL.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // 1. Establish a known state with our own embedding rows.
    let initial_fp = fp("local", "BGE-small-en-v1.5", 384);
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.reconcile_embedding_fingerprint(&initial_fp).unwrap();
        seed_embedding(&db, 384, "seed");
    }

    // 2. Force a deterministic mid-sequence failure via the
    //    RECONCILE_FAIL_AFTER_MODEL fault-injection hook (gated by
    //    #[cfg(test)]). Page-cap tricks don't reliably trigger
    //    SQLITE_FULL: SQLite reuses freed pages after DROP TABLE.
    let new_fp = fp("ollama", "nomic-embed-text-v2", 384);
    let outcome = {
        let db = Database::open(&db_path, 384).unwrap();
        RECONCILE_FAIL_AFTER_MODEL.with(|b| b.store(true, std::sync::atomic::Ordering::SeqCst));
        db.reconcile_embedding_fingerprint(&new_fp)
    };
    assert!(outcome.is_err(), "injected SQLITE_FULL must surface as Err");

    // 3. Failure path: the DB on disk must still reflect the INITIAL
    //    fingerprint, not a partial state.
    let post = Database::open(&db_path, 384).unwrap();
    let stored_provider = post.get_metadata("embedding_provider").unwrap();
    let stored_model = post.get_metadata("embedding_model").unwrap();
    let stored_dim_str = post.get_metadata("embedding_dimension").unwrap();
    let symbol_vec_exists = post
        .conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='symbol_vec'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .unwrap()
        .is_some();
    assert_eq!(
        stored_provider.as_deref(),
        Some("local"),
        "failed reconcile must roll back provider"
    );
    assert_eq!(
        stored_model.as_deref(),
        Some("BGE-small-en-v1.5"),
        "failed reconcile must roll back model"
    );
    assert_eq!(
        stored_dim_str.as_deref(),
        Some("384"),
        "failed reconcile must roll back dimension"
    );
    assert!(
        symbol_vec_exists,
        "failed reconcile must roll back symbol_vec drop"
    );
    assert_eq!(
        post.embedding_count().unwrap(),
        1,
        "failed reconcile must roll back the symbol_embedding_map DELETE"
    );
}

#[test]
fn test_default_embedding_dim_constant() {
    assert_eq!(DEFAULT_EMBEDDING_DIM, 384);
}

#[test]
fn test_destructive_migration_creates_backup() {
    // Build a legacy v2 database file: pre-hash-columns, with indexed data.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("legacy.db");

    {
        register_sqlite_vec();
        let conn = Connection::open(&db_path).unwrap();
        // Minimal legacy schema that the wipe code will operate on.
        conn.execute_batch(
            "CREATE TABLE symbols (
                id TEXT PRIMARY KEY, name TEXT, kind TEXT, file_path TEXT,
                start_line INTEGER, end_line INTEGER, start_byte INTEGER, end_byte INTEGER,
                parent_id TEXT, signature TEXT, visibility TEXT,
                is_async BOOLEAN, docstring TEXT, in_degree INTEGER DEFAULT 0
             );
             CREATE TABLE edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT, target_name TEXT,
                target_id TEXT, kind TEXT, file_path TEXT, line INTEGER
             );
             CREATE TABLE files (path TEXT PRIMARY KEY, last_modified REAL, hash TEXT,
                                 language TEXT, num_symbols INTEGER);
             CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO symbols (id, name, kind, file_path) VALUES ('s1', 'foo', 'function', 'a.py');
             INSERT INTO metadata (key, value) VALUES ('schema_version', '2');",
        )
        .unwrap();
    }

    // Opening via the real entry point should back up the legacy file before wiping.
    let _db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

    let backups: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("legacy.db.pre-v")
        })
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "expected exactly one pre-migration backup, found {}",
        backups.len()
    );
}

#[test]
fn test_no_backup_for_fresh_database() {
    // A fresh DB should never produce a backup file.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("fresh.db");
    let _db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

    let backups: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".pre-v"))
        .collect();
    assert!(
        backups.is_empty(),
        "fresh DB should not create a backup file"
    );
}

#[test]
fn fresh_db_stamps_version_without_running_ladder() {
    // A fresh DB takes the fast-path stamp and skips the destructive v2→3
    // wipe. Regression guard for the duplicate-column WARN: the ladder must
    // not re-fire the additive ALTERs against the bootstrapped v6 shape.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("fresh.db");
    let db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

    // The destructive branch deletes the 'last_commit' row; the fast path
    // never enters it, so a marker written before re-open survives.
    db.set_metadata("last_commit", "deadbeef").unwrap();
    drop(db);
    let db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
    let last_commit: Option<String> = db
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'last_commit'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(
        last_commit,
        Some("deadbeef".to_string()),
        "fresh re-open must not run the v2→3 wipe"
    );

    let version: String = db
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION.to_string());
}

#[test]
fn populated_v1_db_runs_full_ladder_to_current() {
    // Negative guard for the fresh-DB fast path: a real pre-versioning v1 DB
    // (no schema_version row, narrow v1 columns, but SEEDED with rows) must
    // NOT be misclassified as fresh. It runs the full v1→current ladder
    // including the intentional v2→3 stable-id wipe and lands at the current
    // schema version with every later column present.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("v1.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        // True v1 shape: symbols end at docstring, edges end at line, no
        // query_log, no schema_version row.
        conn.execute_batch(
            "CREATE TABLE symbols (
                id TEXT PRIMARY KEY, name TEXT, kind TEXT, file_path TEXT,
                start_line INTEGER, end_line INTEGER, start_byte INTEGER, end_byte INTEGER,
                parent_id TEXT, signature TEXT, visibility TEXT, is_async BOOLEAN, docstring TEXT);
             CREATE TABLE edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id TEXT NOT NULL, target_name TEXT NOT NULL, target_id TEXT,
                kind TEXT NOT NULL, file_path TEXT NOT NULL, line INTEGER);
             CREATE TABLE files (path TEXT PRIMARY KEY);
             CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO symbols (id, name, kind, file_path) VALUES ('s:1', 'foo', 'function', 'a.py');
             INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line)
               VALUES ('s:1', 'foo', 's:1', 'calls', 'a.py', 1);",
        )
        .unwrap();
    }

    let db = Database::open(&path, DEFAULT_EMBEDDING_DIM).unwrap();

    // Ladder reached the current version.
    let version: String = db
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION.to_string());

    // v5→6 column exists (the fast path would have skipped this ALTER).
    assert!(
        db.conn
            .prepare("SELECT resolution_source FROM edges LIMIT 0")
            .is_ok(),
        "resolution_source must be added by the real upgrade"
    );

    // The intentional v2→3 wipe cleared the seeded rows for the stable-id
    // rebuild — proving the fresh-DB fast path was NOT taken.
    let symbol_count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap();
    assert_eq!(symbol_count, 0, "v2→3 wipe must run for a populated v1 DB");
}

#[test]
fn test_busy_timeout_pragma_is_set() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("timeout.db");
    let db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

    let timeout: i64 = db
        .conn
        .query_row("PRAGMA busy_timeout;", [], |row| row.get(0))
        .unwrap();
    assert_eq!(timeout, BUSY_TIMEOUT_MS as i64);
}

#[test]
fn test_busy_timeout_makes_second_writer_retry_instead_of_aborting() {
    // Regression for #42. A second writer blocked by a held write lock
    // should *wait* (bounded by busy_timeout) rather than abort instantly.
    // Proven deterministically: against the same held lock, a connection
    // with busy_timeout=0 fails immediately, one with a non-zero timeout
    // only fails after waiting that long. No inter-thread timing race.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("concurrent.db");
    let _ = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

    // Holder keeps an exclusive write lock for the whole test.
    let holder = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
    holder
        .conn
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO metadata (key, value) VALUES ('a', '1');")
        .unwrap();

    let attempt_write = |timeout_ms: u32| -> std::time::Duration {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!("PRAGMA busy_timeout={timeout_ms};"))
            .unwrap();
        let start = std::time::Instant::now();
        let res = conn.execute("INSERT INTO metadata (key, value) VALUES ('b', '2');", []);
        assert!(res.is_err(), "write must fail while the lock is held");
        start.elapsed()
    };

    // busy_timeout=0: SQLite aborts immediately, no retry.
    assert!(
        attempt_write(0) < std::time::Duration::from_millis(150),
        "with busy_timeout=0 the writer must fail immediately"
    );
    // busy_timeout=300ms: SQLite retries for the full window before failing.
    assert!(
        attempt_write(300) >= std::time::Duration::from_millis(250),
        "with a non-zero busy_timeout the writer must retry, not abort"
    );

    holder.conn.execute_batch("COMMIT;").unwrap();
}
