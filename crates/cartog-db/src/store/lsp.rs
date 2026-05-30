//! LSP-resolution helper queries (unresolved edges, marker updates).
//!
//! Part of the [`Database`](super::Database) impl, split out of `lib.rs` for navigability.

use super::*;

impl Database {
    // ── LSP Resolution Helpers ──
    //
    // These helpers (`unresolved_edges`, `find_symbol_at_location`,
    // `update_edge_target`, `mark_edge_unresolvable`, `mark_edge_external`,
    // `edge_resolution_state`) are called from `cartog-lsp::lsp_resolve_edges`,
    // which itself runs inside `index_directory`'s outer indexing transaction
    // (see [`Self::begin_indexing_tx`]). They MUST remain transaction-free —
    // any future addition of `unchecked_transaction()` here would re-introduce
    // the Phase 3 atomicity bug at runtime ("cannot start a transaction
    // within a transaction").

    /// Return edges still waiting for resolution (`resolution_state = 0`).
    ///
    /// Edges marked `state = 2` (unresolvable) or `state = 3` (external) are
    /// excluded so a dirty reindex doesn't re-query the language server for
    /// edges it already classified. Both are sticky and re-enter the
    /// unresolved set only via [`Self::reset_unresolvable_for_names`] when a
    /// matching symbol is added, or [`Self::reset_all_unresolvable`] on
    /// `--force`. ([`Self::invalidate_edges_targeting`] only touches state=1
    /// rows because it filters on `target_id IS NOT NULL`, and state {2, 3}
    /// rows always have `target_id NULL`.)
    ///
    /// tx-safe: read-only single statement — see note above the section header.
    pub fn unresolved_edges(&self) -> Result<Vec<UnresolvedEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.target_name, e.file_path, e.line
             FROM edges e
             WHERE e.resolution_state = 0",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(UnresolvedEdge {
                edge_id: row.get(0)?,
                target_name: row.get(1)?,
                file_path: row.get(2)?,
                line: row.get(3)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Find the tightest-enclosing symbol at a given file + line.
    ///
    /// tx-safe: read-only single statement — see the LSP-section header note.
    pub fn find_symbol_at_location(&self, file_path: &str, line: u32) -> Result<Option<String>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM symbols
                 WHERE file_path = ?1 AND start_line <= ?2 AND end_line >= ?2
                 ORDER BY (end_line - start_line) ASC
                 LIMIT 1",
                params![file_path, line],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id)
    }

    /// Update a single edge's target_id and flip it to `resolution_state = 1`.
    ///
    /// tx-safe: single statement — see the LSP-section header note. If you
    /// ever batch this internally with `unchecked_transaction()`, also update
    /// `index_directory` so it does not call `lsp_resolve_edges` inside its
    /// outer transaction.
    pub fn update_edge_target(&self, edge_id: i64, target_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET target_id = ?1, resolution_state = 1 WHERE id = ?2",
            params![target_id, edge_id],
        )?;
        Ok(())
    }

    /// Test-only inspector: returns true when the edge is at `resolution_state = 2`.
    ///
    /// Exposed because the column is otherwise crate-private, and downstream
    /// integration tests (cartog-indexer) need a read-only way to assert the
    /// marker state without snapshotting raw SQL.
    pub fn is_edge_unresolvable(&self, edge_id: i64) -> Result<bool> {
        Ok(self.edge_resolution_state(edge_id)? == 2)
    }

    /// Test-only inspector: returns the raw `resolution_state` value for an edge.
    ///
    /// 0=unresolved, 1=resolved, 2=unresolvable, 3=external.
    pub fn edge_resolution_state(&self, edge_id: i64) -> Result<i64> {
        let state: i64 = self.conn.query_row(
            "SELECT resolution_state FROM edges WHERE id = ?1",
            params![edge_id],
            |row| row.get(0),
        )?;
        Ok(state)
    }

    /// Reset every edge at `resolution_state IN (2, 3)` back to `0`. Used by
    /// `cartog index --force` to honor the "retry everything" contract:
    /// without this, the heuristic + LSP would still skip permanently-marked
    /// edges (both unresolvable and external) even on a forced re-index.
    ///
    /// tx-safe: single statement — see the LSP-section header note.
    pub fn reset_all_unresolvable(&self) -> Result<u32> {
        let n = self.conn.execute(
            "UPDATE edges SET resolution_state = 0 WHERE resolution_state IN (2, 3)",
            [],
        )?;
        Ok(n as u32)
    }

    /// Mark a single edge as `resolution_state = 2` (LSP definitively gave up).
    ///
    /// Callers MUST only invoke this after a definitive negative answer from
    /// the language server. Never call from a transient-error branch (server
    /// crash, didOpen failure, half-loaded warmup) — the marker is sticky
    /// across runs until [`Self::reset_unresolvable_for_names`] reopens it
    /// (on a matching new symbol) or [`Self::reset_all_unresolvable`] runs
    /// (`--force`).
    ///
    /// The `WHERE resolution_state = 0` guard preserves the invariant that
    /// state {2, 3} rows have `target_id IS NULL` — without it an accidental
    /// call on a state=1 (resolved) edge would silently flip the state while
    /// keeping the stale target, hiding a corrupted edge from
    /// [`Self::unresolved_edges`].
    ///
    /// tx-safe: single statement — see the LSP-section header note.
    pub fn mark_edge_unresolvable(&self, edge_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET resolution_state = 2 WHERE id = ?1 AND resolution_state = 0",
            params![edge_id],
        )?;
        Ok(())
    }

    /// Mark a single edge as `resolution_state = 3` (LSP located the target
    /// outside the indexed root — stdlib, third-party deps, node_modules).
    ///
    /// Same stickiness contract as [`Self::mark_edge_unresolvable`]: only call
    /// after a definitive positive answer naming an out-of-root URI;
    /// reopened by the same name-keyed and force-reset paths. The
    /// `WHERE resolution_state = 0` guard preserves the `target_id IS NULL`
    /// invariant for state=3 rows.
    ///
    /// tx-safe: single statement — see the LSP-section header note.
    pub fn mark_edge_external(&self, edge_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET resolution_state = 3 WHERE id = ?1 AND resolution_state = 0",
            params![edge_id],
        )?;
        Ok(())
    }

    /// Reset `resolution_state` from {2, 3} → 0 for edges whose target_name is in `names`.
    ///
    /// Called from the indexer when new symbols are added: an edge that was
    /// previously "unresolvable" (no symbol with this name existed) or
    /// "external" (target lived outside the index — but the user just vendored
    /// it in-tree) may now be resolvable against the freshly-added target.
    /// Returns the number of edges reopened. No-op when `names` is empty.
    ///
    /// tx-safe: single statement. Names are batched to honor SQLite's default
    /// 999-parameter limit; only rows at state 2 or 3 are touched so the write
    /// set stays tiny even on a large rename.
    pub fn reset_unresolvable_for_names(&self, names: &[String]) -> Result<u32> {
        if names.is_empty() {
            return Ok(0);
        }
        const CHUNK: usize = 500;
        let mut total: u32 = 0;
        for chunk in names.chunks(CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "UPDATE edges
                 SET resolution_state = 0
                 WHERE resolution_state IN (2, 3)
                   AND target_name IN ({placeholders})"
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|n| n as &dyn rusqlite::ToSql).collect();
            let n = self.conn.execute(&sql, params.as_slice())?;
            total += n as u32;
        }
        Ok(total)
    }
}
