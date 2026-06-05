//! RAG layer: symbol content, FTS5 search, embedding map, sqlite-vec vector storage.
//!
//! Part of the [`Database`](super::Database) impl, split out of `lib.rs` for navigability.

use super::*;

/// Kind scope for [`Database::fts5_search_kinded`], so retrieval can filter by
/// kind in SQL. Mirrors the rag layer's `KindFilter`.
#[derive(Debug, Clone, Copy)]
pub enum KindScope {
    /// No kind restriction (the historical `fts5_search` behaviour).
    All,
    /// Exclude `Document` and `Import` symbols.
    CodeOnly,
    /// Only the given kind.
    Exact(SymbolKind),
}

impl Database {
    // ── RAG: Symbol Content ──

    /// Insert or replace symbol content (raw source + metadata header for embedding).
    ///
    /// `symbol_name` is used to compute a normalized form (camelCase/snake_case split)
    /// stored in the FTS5 index for better keyword matching.
    pub fn upsert_symbol_content(
        &self,
        symbol_id: &str,
        symbol_name: &str,
        content: &str,
        header: &str,
    ) -> Result<()> {
        let normalized = normalize_symbol_name(symbol_name);
        // Explicit delete-then-insert (not INSERT OR REPLACE): the FTS5
        // external-content delete trigger does not fire on a REPLACE-conflict
        // with recursive_triggers off, which would leave the old content
        // searchable in the FTS index.
        self.conn.execute(
            "DELETE FROM symbol_content WHERE symbol_id = ?1",
            params![symbol_id],
        )?;
        self.conn.execute(
            "INSERT INTO symbol_content (symbol_id, content, header, normalized_name)
             VALUES (?1, ?2, ?3, ?4)",
            params![symbol_id, content, header, normalized],
        )?;
        Ok(())
    }

    /// Insert multiple symbol contents in a single transaction.
    ///
    /// Tuples: `(symbol_id, symbol_name, content, header)`.
    pub fn insert_symbol_contents(&self, items: &[(String, String, String, String)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_symbol_contents_in_tx(items)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::insert_symbol_contents`] but assumes the caller already
    /// holds an open transaction.
    pub fn insert_symbol_contents_in_tx(
        &self,
        items: &[(String, String, String, String)],
    ) -> Result<()> {
        // Delete-then-insert per row so the FTS5 delete trigger fires and the
        // old content does not linger in the index (see `upsert_symbol_content`).
        let mut del = self
            .conn
            .prepare_cached("DELETE FROM symbol_content WHERE symbol_id = ?1")?;
        let mut ins = self.conn.prepare_cached(
            "INSERT INTO symbol_content (symbol_id, content, header, normalized_name)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (symbol_id, name, content, header) in items {
            let normalized = normalize_symbol_name(name);
            del.execute(params![symbol_id])?;
            ins.execute(params![symbol_id, content, header, normalized])?;
        }
        Ok(())
    }

    /// Remove symbol content for all symbols in a file.
    pub fn clear_symbol_content_for_file(&self, file_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM symbol_content WHERE symbol_id IN
             (SELECT id FROM symbols WHERE file_path = ?1)",
            params![file_path],
        )?;
        Ok(())
    }

    /// Get the content + header for a symbol.
    pub fn get_symbol_content(&self, symbol_id: &str) -> Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT content, header FROM symbol_content WHERE symbol_id = ?1",
                params![symbol_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("Failed to query symbol content")
    }

    /// Batch fetch content + header for multiple symbols.
    ///
    /// Returns a map of `symbol_id → (content, header)` for all found symbols.
    pub fn get_symbol_contents_batch(
        &self,
        symbol_ids: &[String],
    ) -> Result<std::collections::HashMap<String, (String, String)>> {
        let mut result = std::collections::HashMap::with_capacity(symbol_ids.len());
        if symbol_ids.is_empty() {
            return Ok(result);
        }
        for chunk in symbol_ids.chunks(Self::FILE_CHUNK_SIZE) {
            let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
            let sql = format!(
                "SELECT symbol_id, content, header FROM symbol_content WHERE symbol_id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                .iter()
                .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (id, content, header) in rows {
                result.insert(id, (content, header));
            }
        }
        Ok(result)
    }

    // ── RAG: FTS5 Search ──

    /// Full-text search over symbol names and content using BM25 ranking.
    ///
    /// Returns symbol IDs ordered by relevance (best match first).
    pub fn fts5_search(&self, query: &str, limit: u32) -> Result<Vec<String>> {
        self.fts5_search_kinded(query, limit, KindScope::All)
    }

    /// Like [`Self::fts5_search`] but filters by kind in SQL, so a prose query
    /// doesn't spend the whole `limit` budget on `Document` (markdown) hits.
    pub fn fts5_search_kinded(
        &self,
        query: &str,
        limit: u32,
        scope: KindScope,
    ) -> Result<Vec<String>> {
        // `All` keeps the lean no-JOIN path; the others join `symbols` for `kind`.
        let (where_kind, kind_param): (&str, Option<&str>) = match scope {
            KindScope::All => ("", None),
            KindScope::CodeOnly => ("AND s.kind NOT IN ('document', 'import')", None),
            KindScope::Exact(k) => ("AND s.kind = ?3", Some(k.as_str())),
        };
        let sql = if matches!(scope, KindScope::All) {
            "SELECT sc.symbol_id
             FROM symbol_fts f
             JOIN symbol_content sc ON sc.rowid = f.rowid
             WHERE symbol_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
                .to_string()
        } else {
            format!(
                "SELECT sc.symbol_id
                 FROM symbol_fts f
                 JOIN symbol_content sc ON sc.rowid = f.rowid
                 JOIN symbols s ON s.id = sc.symbol_id
                 WHERE symbol_fts MATCH ?1 {where_kind}
                 ORDER BY rank
                 LIMIT ?2"
            )
        };
        let ctx =
            || format!("fts5_search_kinded (scope={scope:?}, query={query:?}, limit={limit})");
        let mut stmt = self.conn.prepare(&sql).with_context(ctx)?;
        let rows: Vec<String> = match kind_param {
            Some(k) => stmt
                .query_map(params![query, limit, k], |row| row.get(0))
                .with_context(ctx)?
                .collect::<std::result::Result<_, _>>()
                .with_context(ctx)?,
            None => stmt
                .query_map(params![query, limit], |row| row.get(0))
                .with_context(ctx)?
                .collect::<std::result::Result<_, _>>()
                .with_context(ctx)?,
        };
        Ok(rows)
    }

    // ── RAG: Embedding Map ──

    /// Get or create an integer ID for a symbol in the embedding map.
    ///
    /// Returns the `id` (integer rowid) used as key in the vec0 virtual table.
    pub fn get_or_create_embedding_id(&self, symbol_id: &str) -> Result<i64> {
        // Try to get existing
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM symbol_embedding_map WHERE symbol_id = ?1",
                params![symbol_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            return Ok(id);
        }

        // Insert new
        self.conn.execute(
            "INSERT INTO symbol_embedding_map (symbol_id) VALUES (?1)",
            params![symbol_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Look up the symbol ID for an embedding map rowid.
    pub fn symbol_id_for_embedding(&self, embedding_id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT symbol_id FROM symbol_embedding_map WHERE id = ?1",
                params![embedding_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to query embedding map")
    }

    /// Batch look up symbol IDs for multiple embedding map rowids.
    pub fn symbol_ids_for_embeddings(&self, embedding_ids: &[i64]) -> Result<Vec<(i64, String)>> {
        if embedding_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut all_results = Vec::with_capacity(embedding_ids.len());
        for chunk in embedding_ids.chunks(Self::FILE_CHUNK_SIZE) {
            let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "SELECT id, symbol_id FROM symbol_embedding_map WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                .iter()
                .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            all_results.extend(rows);
        }
        Ok(all_results)
    }

    // ── RAG: Vector Storage (sqlite-vec) ──

    /// Insert or replace an embedding vector for a symbol.
    ///
    /// `embedding_id` is the integer key from `symbol_embedding_map`.
    /// `embedding` is a 384-dim f32 vector serialized as little-endian bytes.
    pub fn upsert_embedding(&self, embedding_id: i64, embedding: &[u8]) -> Result<()> {
        // Delete existing entry if any (vec0 doesn't support REPLACE)
        self.conn.execute(
            "DELETE FROM symbol_vec WHERE rowid = ?1",
            params![embedding_id],
        )?;
        self.conn.execute(
            "INSERT INTO symbol_vec (rowid, embedding) VALUES (?1, ?2)",
            params![embedding_id, embedding],
        )?;
        Ok(())
    }

    /// Insert multiple embeddings in a single transaction.
    pub fn insert_embeddings(&self, items: &[(i64, Vec<u8>)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (id, embedding) in items {
            self.conn
                .execute("DELETE FROM symbol_vec WHERE rowid = ?1", params![id])?;
            self.conn.execute(
                "INSERT INTO symbol_vec (rowid, embedding) VALUES (?1, ?2)",
                params![id, embedding],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// KNN vector search: find the `limit` nearest neighbors to `query_embedding`.
    ///
    /// Returns `(embedding_id, distance)` pairs ordered by distance (ascending).
    pub fn vector_search(&self, query_embedding: &[u8], limit: u32) -> Result<Vec<(i64, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT rowid, distance
             FROM symbol_vec
             WHERE embedding MATCH ?1
             ORDER BY distance
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![query_embedding, limit], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count the number of embeddings stored.
    pub fn embedding_count(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM symbol_embedding_map", [], |row| {
                row.get(0)
            })?)
    }

    /// Check if a symbol already has an embedding.
    pub fn has_embedding(&self, symbol_id: &str) -> Result<bool> {
        let map_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM symbol_embedding_map WHERE symbol_id = ?1",
                params![symbol_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = map_id {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM symbol_vec WHERE rowid = ?1)",
                params![id],
                |row| row.get(0),
            )?;
            Ok(exists)
        } else {
            Ok(false)
        }
    }

    /// Remove all RAG data (content, FTS, embeddings, embedding map) for symbols in a file.
    pub fn clear_rag_data_for_file(&self, file_path: &str) -> Result<()> {
        // Delete embeddings via the map
        self.conn.execute(
            "DELETE FROM symbol_vec WHERE rowid IN
             (SELECT em.id FROM symbol_embedding_map em
              JOIN symbols s ON em.symbol_id = s.id
              WHERE s.file_path = ?1)",
            params![file_path],
        )?;
        // Delete embedding map entries
        self.conn.execute(
            "DELETE FROM symbol_embedding_map WHERE symbol_id IN
             (SELECT id FROM symbols WHERE file_path = ?1)",
            params![file_path],
        )?;
        // Delete content (triggers will clean up FTS)
        self.clear_symbol_content_for_file(file_path)?;
        Ok(())
    }

    /// Get a symbol by its ID.
    pub fn get_symbol(&self, id: &str) -> Result<Option<Symbol>> {
        self.conn
            .query_row(
                "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                        parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
                 FROM symbols WHERE id = ?1",
                params![id],
                row_to_symbol,
            )
            .optional()
            .context("Failed to query symbol")
    }

    /// Get multiple symbols by their IDs, preserving order.
    pub fn get_symbols_by_ids(&self, ids: &[String]) -> Result<Vec<Symbol>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                    parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
             FROM symbols WHERE id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = ids
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows: std::collections::HashMap<String, Symbol> = stmt
            .query_map(param_refs.as_slice(), row_to_symbol)?
            .filter_map(|r| r.ok())
            .map(|s| (s.id.clone(), s))
            .collect();
        // Preserve caller's ordering
        Ok(ids.iter().filter_map(|id| rows.get(id).cloned()).collect())
    }

    /// Get all symbol IDs that have content stored but no embedding yet.
    ///
    /// Variables are excluded — they are too numerous and low-signal for embedding.
    pub fn symbols_needing_embeddings(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT sc.symbol_id FROM symbol_content sc
             JOIN symbols s ON s.id = sc.symbol_id
             WHERE s.kind NOT IN (?1, ?2)
             AND NOT EXISTS (
                 SELECT 1 FROM symbol_embedding_map em
                 JOIN symbol_vec sv ON sv.rowid = em.id
                 WHERE em.symbol_id = sc.symbol_id
             )",
        )?;
        let rows = stmt
            .query_map(
                params![SymbolKind::Variable.as_str(), SymbolKind::Import.as_str(),],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count symbols that have content stored.
    pub fn symbol_content_count(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM symbol_content", [], |row| row.get(0))?)
    }

    /// Get all symbol IDs that have content stored (excluding variables and imports).
    pub fn all_content_symbol_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT sc.symbol_id FROM symbol_content sc
             JOIN symbols s ON s.id = sc.symbol_id
             WHERE s.kind NOT IN (?1, ?2)
             ORDER BY sc.symbol_id",
        )?;
        let rows = stmt
            .query_map(
                params![SymbolKind::Variable.as_str(), SymbolKind::Import.as_str(),],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Clear all embedding data (for force re-embed).
    pub fn clear_all_embeddings(&self) -> Result<()> {
        self.conn.execute("DELETE FROM symbol_vec", [])?;
        self.conn.execute("DELETE FROM symbol_embedding_map", [])?;
        Ok(())
    }
}
