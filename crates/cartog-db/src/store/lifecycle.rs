//! Database open/create/migrate lifecycle and connection setup.
//!
//! Part of the [`Database`](super::Database) impl, split out of `lib.rs` for navigability.

use super::*;

impl Database {
    /// Open or create the database at the given path.
    ///
    /// `embedding_dim` sets the vector dimension for the sqlite-vec table.
    /// If the stored dimension differs from the requested one, the vector index
    /// is cleared and recreated (a re-index via `cartog rag index` is needed).
    pub fn open(path: impl AsRef<std::path::Path>, embedding_dim: usize) -> DbResult<Self> {
        register_sqlite_vec();
        let db_path = path.as_ref();
        // SQLite::open fails on a missing parent tree, so materialize `.cartog/`.
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| DbError::PrepareDir {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(db_path).map_err(|source| DbError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(&format!(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout={BUSY_TIMEOUT_MS};
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-65536;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=268435456;"
        ))
        .map_err(DbError::Pragma)?;
        conn.execute_batch(SCHEMA).map_err(DbError::Schema)?;
        conn.execute_batch(RAG_SCHEMA).map_err(DbError::RagSchema)?;
        backup_before_destructive_migration(&conn, db_path)?;
        migrate(&conn);
        // Partial index requires resolution_state (added in migration 3→4),
        // so create it after migrate() rather than from SCHEMA.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_edges_unresolved
                 ON edges(file_path) WHERE resolution_state = 0",
        )
        .map_err(DbError::Schema)?;
        handle_embedding_dimension(&conn, embedding_dim).map_err(DbError::EmbeddingDimension)?;
        Ok(Self { conn, pinned: None })
    }

    /// Open an existing on-disk database in **read-write** mode without
    /// running schema migrations or the embedding-fingerprint reconcile.
    /// Used by the Phase 5 promoter: a secondary that detected its primary
    /// died and validated the on-disk schema/fingerprint against its
    /// pinned snapshot before claiming the slot. Re-running the migration
    /// would re-trigger the SQLITE_BUSY race that the election was meant
    /// to prevent.
    ///
    /// Verifies that `schema_version` still matches `SCHEMA_VERSION` to
    /// guard against a race where another writer upgraded the schema
    /// between the secondary's attach and this promotion. Returns
    /// [`DbError::SchemaDrift`] in that case so the promoter aborts and
    /// the MCP process exits cleanly.
    pub fn open_existing_rw(path: impl AsRef<std::path::Path>) -> DbResult<Self> {
        register_sqlite_vec();
        let db_path = path.as_ref();
        let conn = Connection::open(db_path).map_err(|source| DbError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(&format!(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout={BUSY_TIMEOUT_MS};
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-65536;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=268435456;"
        ))
        .map_err(DbError::Pragma)?;

        let stored_schema = read_schema_version(&conn)?;
        if stored_schema != SCHEMA_VERSION {
            return Err(DbError::SchemaDrift {
                expected: SCHEMA_VERSION,
                stored: stored_schema,
            });
        }

        Ok(Self { conn, pinned: None })
    }

    /// Open an existing on-disk database in **read-only** mode for a
    /// secondary cartog process (Phase 4 read-only attach). Skips schema
    /// migrations and the embedding-fingerprint reconcile — the primary
    /// writer owns those.
    ///
    /// Behaviour:
    /// - Opens with `SQLITE_OPEN_READ_ONLY` so write attempts surface as
    ///   `SQLITE_READONLY` errors at runtime (a defense-in-depth backup
    ///   for the higher-level tool gating).
    /// - Reads the `metadata` snapshot (schema version + embedding
    ///   fingerprint) and stores it on the returned [`Database`] so the
    ///   promoter (Phase 5) can compare against the on-disk values later.
    /// - Returns [`DbError::SchemaDrift`] if the stored `schema_version`
    ///   doesn't match this binary's expected version — the primary
    ///   upgraded cartog underneath us and queries can't be trusted.
    pub fn open_readonly(path: impl AsRef<std::path::Path>) -> DbResult<Self> {
        use rusqlite::OpenFlags;
        register_sqlite_vec();
        let db_path = path.as_ref();
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| DbError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
        // busy_timeout is still useful: a long read can stall against a
        // writer mid-checkpoint. WAL keeps readers and writers from
        // blocking otherwise, but the timeout makes the bound explicit.
        conn.execute_batch(&format!("PRAGMA busy_timeout={BUSY_TIMEOUT_MS};"))
            .map_err(DbError::Pragma)?;

        let stored_schema = read_schema_version(&conn)?;
        if stored_schema != SCHEMA_VERSION {
            return Err(DbError::SchemaDrift {
                expected: SCHEMA_VERSION,
                stored: stored_schema,
            });
        }

        let stored_provider: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![EMBED_PROVIDER_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(DbError::Sqlite)?;
        let stored_model: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![EMBED_MODEL_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(DbError::Sqlite)?;
        let stored_dim: Option<usize> = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get::<_, i64>(0).map(|v| v as usize),
            )
            .optional()
            .map_err(DbError::Sqlite)?;
        // Embedding fingerprint is recorded together (Phase 6b backfill).
        // If any field is missing the fingerprint is "unknown" — readers
        // can still serve graph queries, just can't validate against a
        // promoter swap later.
        let embedding = match (stored_provider, stored_model, stored_dim) {
            (Some(provider), Some(model), Some(dimension)) => Some(EmbeddingFingerprint {
                provider,
                model,
                dimension,
            }),
            _ => None,
        };

        Ok(Self {
            conn,
            pinned: Some(PinnedAttach {
                schema_version: stored_schema,
                embedding,
            }),
        })
    }

    /// Open an in-memory database (for tests and benchmarks).
    #[doc(hidden)]
    pub fn open_memory() -> DbResult<Self> {
        register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(DbError::Pragma)?;
        conn.execute_batch(SCHEMA).map_err(DbError::Schema)?;
        conn.execute_batch(RAG_SCHEMA).map_err(DbError::RagSchema)?;
        conn.execute_batch(&rag_vec_schema(DEFAULT_EMBEDDING_DIM))
            .map_err(DbError::RagSchema)?;
        migrate(&conn);
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_edges_unresolved
                 ON edges(file_path) WHERE resolution_state = 0",
        )
        .map_err(DbError::Schema)?;
        Ok(Self { conn, pinned: None })
    }

    /// True when this `Database` was opened via [`Self::open_readonly`].
    /// MCP tool gating (Phase 4) consults this to refuse the 2 write tools.
    pub fn is_read_only(&self) -> bool {
        self.pinned.is_some()
    }

    /// Snapshot captured at attach time when [`Self::open_readonly`] was
    /// used. `None` for read-write opens.
    pub fn pinned_attach(&self) -> Option<&PinnedAttach> {
        self.pinned.as_ref()
    }

    /// Cap the number of pages this DB connection can hold.
    ///
    /// Intended for tests that need to force a `SQLITE_FULL` error on a
    /// subsequent write (for example, to verify that a transaction rolls back
    /// cleanly). Production code should never call this.
    #[doc(hidden)]
    pub fn set_max_page_count_for_tests(&self, pages: u32) -> Result<()> {
        self.conn
            .execute_batch(&format!("PRAGMA max_page_count = {pages}"))?;
        Ok(())
    }

    /// Open a single SQLite transaction that the caller is expected to wrap
    /// around a multi-step indexing pipeline.
    ///
    /// Drop without `commit()` rolls back, so a panic mid-pipeline leaves the
    /// DB in its prior state.
    ///
    /// # Calling conventions inside the transaction
    ///
    /// Helpers fall into two categories:
    ///
    /// 1. **Batched writers must use the `_in_tx` variant.** Their non-`_in_tx`
    ///    wrapper issues its own `BEGIN` and would error out at runtime
    ///    (`cannot start a transaction within a transaction`). Examples:
    ///    [`Self::insert_symbols_in_tx`], [`Self::delete_symbols_in_tx`],
    ///    [`Self::insert_edges_in_tx`], [`Self::insert_symbol_contents_in_tx`],
    ///    [`Self::clear_file_data_in_tx`], [`Self::remove_file_in_tx`],
    ///    [`Self::resolve_edges_in_tx`], [`Self::resolve_edges_scoped_in_tx`].
    ///
    /// 2. **Single-statement helpers can be called directly.** They issue one
    ///    `self.conn.execute(...)` and participate transparently in the active
    ///    transaction. Examples used by `cartog-indexer`'s Phase 3 today:
    ///    [`Self::upsert_file`], [`Self::clear_edges_for_file`],
    ///    [`Self::set_metadata`], [`Self::compute_in_degrees`],
    ///    [`Self::compute_in_degrees_scoped`], [`Self::invalidate_edges_targeting`].
    ///    These are tagged with `// tx-safe: single statement` so the contract
    ///    survives drive-by edits.
    ///
    /// # Why `unchecked_transaction` rather than [`rusqlite::Connection::transaction`]
    ///
    /// `transaction()` requires `&mut Connection`, which would force every
    /// caller of `Database` to hold a mutable borrow for the entire pipeline.
    /// `unchecked_transaction()` works through `&Connection` and produces an
    /// equivalent [`rusqlite::Transaction`] with the same `DropBehavior::Rollback`
    /// default — only borrow-check ergonomics differ.
    ///
    /// # Errors
    ///
    /// Returns an error if SQLite cannot begin a transaction — typically
    /// because another transaction is already active on this connection.
    pub fn begin_indexing_tx(&self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self.conn.unchecked_transaction()?)
    }

    /// Refresh the query planner's statistics via `PRAGMA optimize`.
    ///
    /// SQLite picks join order and index use from `sqlite_stat1`; without it,
    /// the planner guesses from index shape alone and can mis-plan (the tier-2
    /// resolution misplan in #110 was one such case). `PRAGMA optimize` runs
    /// `ANALYZE` only on tables whose row counts have drifted since the last
    /// analyze, so it is a cheap no-op when nothing changed — unlike a bare
    /// `ANALYZE`, which would re-scan every index on each call and reintroduce
    /// a per-index O(repo) cost.
    ///
    /// Call AFTER committing the indexing transaction, not inside it: a stats
    /// rebuild bundled into the big write tx would bloat it. No-op-safe to call
    /// when nothing was indexed, but the indexer skips it on no-op runs anyway.
    pub fn optimize(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA optimize;")
            .context("PRAGMA optimize (refresh planner statistics)")?;
        Ok(())
    }
}
