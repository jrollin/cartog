//! Metadata, file, symbol, and edge writes (the core CRUD surface).
//!
//! Part of the [`Database`](super::Database) impl, split out of `lib.rs` for navigability.

use super::*;

impl Database {
    // ── Metadata ──

    /// Retrieve a metadata value by key.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to query metadata")
    }

    /// Store a metadata key-value pair (upserts on conflict).
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Reconcile the stored embedding fingerprint with the one currently in
    /// use. Call right after `Database::open` from any code path that owns
    /// an `EmbeddingProvider` (indexer, watcher, MCP serve, `rag index`).
    ///
    /// Behavior:
    /// - All three fields (provider, model, dimension) match stored → no-op,
    ///   zero writes.
    /// - Any field differs → drop `symbol_vec`, clear `symbol_embedding_map`,
    ///   recreate the vector table at the new dimension, update all three
    ///   metadata keys. The user must run `cartog rag index` to repopulate.
    /// - DB has dimension but no provider/model (older cartog versions) →
    ///   backfill provider+model without wiping. The stored vectors stay
    ///   valid against whatever stack produced them; we just record the
    ///   identity going forward.
    ///
    /// Writes use `retry_busy` so a concurrent writer on the same DB does
    /// not crash this caller with `SQLITE_BUSY`.
    pub fn reconcile_embedding_fingerprint(&self, fp: &EmbeddingFingerprint) -> Result<()> {
        let stored_provider: Option<String> = self.get_metadata(EMBED_PROVIDER_KEY)?;
        let stored_model: Option<String> = self.get_metadata(EMBED_MODEL_KEY)?;
        let stored_dim: Option<usize> = self
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get::<_, i64>(0).map(|v| v as usize),
            )
            .optional()
            .context("Failed to query embedding dimension")?;

        // Full match AND `symbol_vec` actually exists on disk: zero writes.
        // The dim+table pair is the real invariant; checking metadata
        // alone misses the case where a previous open crashed mid-
        // migration and left the DB without `symbol_vec` while metadata
        // still claims a fingerprint.
        if stored_provider.as_deref() == Some(fp.provider.as_str())
            && stored_model.as_deref() == Some(fp.model.as_str())
            && stored_dim == Some(fp.dimension)
            && symbol_vec_exists(&self.conn)?
        {
            return Ok(());
        }

        // Backwards-compat: stored has dim from an older cartog (no provider/model
        // recorded yet). Treat first-time-with-provider as a backfill, not an
        // invalidation. Embeddings produced by the previous run are still valid
        // against whatever stack the user had configured then.
        let dim_matches = stored_dim == Some(fp.dimension);
        let is_backfill = dim_matches && stored_provider.is_none() && stored_model.is_none();

        if !is_backfill {
            tracing::warn!(
                old_provider = ?stored_provider,
                old_model = ?stored_model,
                old_dim = ?stored_dim,
                new_provider = %fp.provider,
                new_model = %fp.model,
                new_dim = fp.dimension,
                "Embedding fingerprint changed — clearing vector index. Run `cartog rag index` to re-embed."
            );
        }

        // Wrap the whole sequence in a transaction so a mid-sequence
        // failure (e.g. busy-retry exhausted on the third metadata write)
        // rolls back atomically. Otherwise the next open could see
        // partial state — e.g. provider/model match but dimension stale,
        // or symbol_vec dropped but metadata still pointing at the old
        // dim — and either skip migration or silently re-wipe.
        let schema = rag_vec_schema(fp.dimension);
        let do_wipe = !is_backfill;
        retry_busy(|| {
            let tx = self.conn.unchecked_transaction()?;
            if do_wipe {
                tx.execute("DROP TABLE IF EXISTS symbol_vec", [])?;
                tx.execute("DELETE FROM symbol_embedding_map", [])?;
            }
            tx.execute_batch(&schema)?;
            tx.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![EMBED_PROVIDER_KEY, fp.provider],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![EMBED_MODEL_KEY, fp.model],
            )?;
            #[cfg(test)]
            if RECONCILE_FAIL_AFTER_MODEL
                .with(|b| b.swap(false, std::sync::atomic::Ordering::SeqCst))
            {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                    Some("injected mid-sequence failure".into()),
                ));
            }
            tx.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('embedding_dimension', ?1)",
                params![fp.dimension.to_string()],
            )?;
            tx.commit()
        })
        .map_err(|e| anyhow::anyhow!("failed to reconcile embedding fingerprint: {e}"))?;

        Ok(())
    }

    // ── Files ──

    /// Insert or update file metadata.
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn upsert_file(&self, file: &FileInfo) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO files (path, last_modified, hash, language, num_symbols)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                file.path,
                file.last_modified,
                file.hash,
                file.language,
                file.num_symbols,
            ],
        )?;
        Ok(())
    }

    /// Look up stored metadata for a file.
    pub fn get_file(&self, path: &str) -> Result<Option<FileInfo>> {
        self.conn
            .query_row(
                "SELECT path, last_modified, hash, language, num_symbols FROM files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(FileInfo {
                        path: row.get(0)?,
                        last_modified: row.get(1)?,
                        hash: row.get(2)?,
                        language: row.get(3)?,
                        num_symbols: row.get(4)?,
                    })
                },
            )
            .optional()
            .context("Failed to query file")
    }

    /// Remove edges only for a file (used by Merkle diff which updates symbols surgically).
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn clear_edges_for_file(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?1", params![path])?;
        Ok(())
    }

    /// Remove all symbols, edges, and RAG data for a file (before re-indexing it).
    pub fn clear_file_data(&self, path: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.clear_file_data_in_tx(path)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::clear_file_data`] but assumes the caller already holds an
    /// open transaction. Used by `cartog-indexer` to wrap the entire Phase 3
    /// pipeline atomically.
    pub fn clear_file_data_in_tx(&self, path: &str) -> Result<()> {
        self.clear_rag_data_for_file(path)?;
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?1", params![path])?;
        self.conn
            .execute("DELETE FROM symbols WHERE file_path = ?1", params![path])?;
        Ok(())
    }

    /// Remove a file and all its symbols and edges from the index.
    pub fn remove_file(&self, path: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.remove_file_in_tx(path)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::remove_file`] but assumes the caller already holds an
    /// open transaction.
    pub fn remove_file_in_tx(&self, path: &str) -> Result<()> {
        self.clear_file_data_in_tx(path)?;
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }

    // ── Symbols ──

    /// Insert or replace a single symbol.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert_symbol(&self, sym: &Symbol) -> Result<()> {
        self.conn
            .prepare_cached(SQL_INSERT_SYMBOL)?
            .execute(params![
                sym.id,
                sym.name,
                sym.kind.as_str(),
                sym.file_path,
                sym.start_line,
                sym.end_line,
                sym.start_byte,
                sym.end_byte,
                sym.parent_id,
                sym.signature,
                sym.visibility.as_str(),
                sym.is_async,
                sym.docstring,
                sym.content_hash,
                sym.subtree_hash,
            ])?;
        Ok(())
    }

    /// Insert or replace multiple symbols in a single transaction.
    pub fn insert_symbols(&self, symbols: &[Symbol]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_symbols_in_tx(symbols)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::insert_symbols`] but assumes the caller already holds an
    /// open transaction.
    pub fn insert_symbols_in_tx(&self, symbols: &[Symbol]) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(SQL_INSERT_SYMBOL)?;
        for sym in symbols {
            stmt.execute(params![
                sym.id,
                sym.name,
                sym.kind.as_str(),
                sym.file_path,
                sym.start_line,
                sym.end_line,
                sym.start_byte,
                sym.end_byte,
                sym.parent_id,
                sym.signature,
                sym.visibility.as_str(),
                sym.is_async,
                sym.docstring,
                sym.content_hash,
                sym.subtree_hash,
            ])?;
        }
        Ok(())
    }

    /// Get stored symbol hashes for a file (for Merkle diff).
    /// Returns `(id, content_hash, subtree_hash)` tuples.
    #[allow(clippy::type_complexity)]
    pub fn get_symbol_hashes_for_file(
        &self,
        file_path: &str,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, content_hash, subtree_hash FROM symbols WHERE file_path = ?1")?;
        let rows = stmt
            .query_map(params![file_path], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Update only the position fields of a symbol (for moved-but-unchanged symbols).
    pub fn update_symbol_position(
        &self,
        id: &str,
        start_line: u32,
        end_line: u32,
        start_byte: u32,
        end_byte: u32,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE symbols SET start_line = ?2, end_line = ?3,
                    start_byte = ?4, end_byte = ?5 WHERE id = ?1",
            params![id, start_line, end_line, start_byte, end_byte],
        )?;
        Ok(())
    }

    /// Delete multiple symbols and cascade (edges, content, embeddings) in a
    /// single transaction.
    pub fn delete_symbols(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        self.delete_symbols_in_tx(ids)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::delete_symbols`] but assumes the caller already holds an
    /// open transaction.
    pub fn delete_symbols_in_tx(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut del_out = self
            .conn
            .prepare_cached("DELETE FROM edges WHERE source_id = ?1")?;
        // Reset state alongside target_id so the orphaned edge re-enters
        // unresolved_edges() instead of becoming a (NULL, state=1) zombie.
        // Clear resolution_source too, else the edge keeps a stale provenance
        // tag that no longer reflects a real target until/unless it re-resolves.
        let mut null_in = self.conn.prepare_cached(
            "UPDATE edges SET target_id = NULL, resolution_state = 0, resolution_source = NULL
             WHERE target_id = ?1",
        )?;
        let mut del_content = self
            .conn
            .prepare_cached("DELETE FROM symbol_content WHERE symbol_id = ?1")?;
        let mut del_sym = self
            .conn
            .prepare_cached("DELETE FROM symbols WHERE id = ?1")?;
        for id in ids {
            del_out.execute(params![id])?;
            null_in.execute(params![id])?;
            // Embedding vec+map delete shared with clear_embeddings_for_symbols_in_tx.
            self.delete_embedding_rows_for_id_in_tx(id)?;
            del_content.execute(params![id])?;
            del_sym.execute(params![id])?;
        }
        Ok(())
    }

    /// Delete a single symbol and cascade to edges, content, and embeddings.
    pub fn delete_symbol(&self, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn
            .execute("DELETE FROM edges WHERE source_id = ?1", params![id])?;
        self.conn.execute(
            "UPDATE edges SET target_id = NULL, resolution_state = 0, resolution_source = NULL
             WHERE target_id = ?1",
            params![id],
        )?;
        let _ = self.conn.execute(
            "DELETE FROM symbol_vec WHERE rowid IN \
             (SELECT id FROM symbol_embedding_map WHERE symbol_id = ?1)",
            params![id],
        );
        let _ = self.conn.execute(
            "DELETE FROM symbol_embedding_map WHERE symbol_id = ?1",
            params![id],
        );
        let _ = self.conn.execute(
            "DELETE FROM symbol_content WHERE symbol_id = ?1",
            params![id],
        );
        self.conn
            .execute("DELETE FROM symbols WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    // ── Edges ──

    /// Insert a single edge.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert_edge(&self, edge: &Edge) -> Result<()> {
        self.conn.prepare_cached(SQL_INSERT_EDGE)?.execute(params![
            edge.source_id,
            edge.target_name,
            edge.target_id,
            edge.kind.as_str(),
            edge.file_path,
            edge.line,
            i64::from(edge.target_id.is_some()),
            edge.provenance.map(|p| p.as_str()),
        ])?;
        Ok(())
    }

    /// Insert multiple edges in a single transaction.
    pub fn insert_edges(&self, edges: &[Edge]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_edges_in_tx(edges)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::insert_edges`] but assumes the caller already holds an
    /// open transaction.
    pub fn insert_edges_in_tx(&self, edges: &[Edge]) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(SQL_INSERT_EDGE)?;
        for edge in edges {
            stmt.execute(params![
                edge.source_id,
                edge.target_name,
                edge.target_id,
                edge.kind.as_str(),
                edge.file_path,
                edge.line,
                i64::from(edge.target_id.is_some()),
                edge.provenance.map(|p| p.as_str()),
            ])?;
        }
        Ok(())
    }
}
