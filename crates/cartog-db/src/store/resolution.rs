//! 6-tier name-based edge resolution, full and scoped (incremental).
//!
//! Part of the [`Database`](super::Database) impl, split out of `lib.rs` for navigability.

use super::*;

impl Database {
    // ── Edge Resolution ──

    /// Resolve target_name → target_id for all unresolved edges.
    ///
    /// Runs two passes so that import edges resolved in pass 1 enable
    /// import-path resolution (tier 2) for non-import edges in pass 2.
    ///
    /// 6-tier priority resolution (per pass):
    /// 1. Same file — symbol with matching name in the same file
    /// 2. Import-path — follow resolved imports to find the target in the imported file
    /// 3. Same directory — symbol in a file in the same directory
    /// 4. Parent scope preference — when multiple global matches, prefer same parent scope
    /// 5. Unique project-wide match — exactly one symbol with that name globally
    /// 6. Class over constructor — when exactly 2 matches and one is a class, prefer class
    pub fn resolve_edges(&self) -> Result<u32> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("Failed to begin edge-resolution transaction")?;
        let total = self.resolve_edges_in_tx()?;
        tx.commit()
            .context("Failed to commit edge-resolution transaction")?;
        Ok(total)
    }

    /// Like [`Self::resolve_edges`] but assumes the caller already holds an
    /// open transaction.
    pub fn resolve_edges_in_tx(&self) -> Result<u32> {
        let mut total_resolved = 0u32;
        for pass in 0..2 {
            let resolved = self
                .resolve_edges_pass()
                .with_context(|| format!("Failed to resolve edges (pass {pass})"))?;
            if resolved == 0 {
                break;
            }
            total_resolved += resolved;
        }
        Ok(total_resolved)
    }

    fn resolve_edges_pass(&self) -> Result<u32> {
        // Skip state=2 (unresolvable) edges: the heuristic can't resolve what
        // LSP definitively gave up on, and re-walking them every dirty reindex
        // burns measurable time on large repos. They re-enter via
        // `reset_unresolvable_for_names` when a matching symbol appears.
        let mut unresolved_stmt = self.conn.prepare(
            "SELECT e.id, e.target_name, e.file_path, e.source_id
             FROM edges e WHERE e.target_id IS NULL AND e.resolution_state = 0",
        )?;

        let unresolved: Vec<(i64, String, String, String)> = unresolved_stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        self.resolve_edge_batch(&unresolved)
    }

    /// 6-tier heuristic resolution for a batch of unresolved edges.
    ///
    /// Caller is responsible for transaction wrapping — see the public
    /// [`Self::resolve_edges`] / [`Self::resolve_edges_scoped`] helpers.
    fn resolve_edge_batch(&self, unresolved: &[(i64, String, String, String)]) -> Result<u32> {
        let mut resolved = 0u32;

        let mut same_file_stmt = self
            .conn
            .prepare("SELECT id FROM symbols WHERE name = ?1 AND file_path = ?2 LIMIT 1")?;

        let mut import_resolve_stmt = self.conn.prepare(
            "SELECT s.id FROM symbols s
             INNER JOIN edges ie ON ie.kind = 'imports' AND ie.target_name = ?1
                 AND ie.target_id IS NOT NULL
             INNER JOIN symbols is2 ON is2.id = ie.source_id AND is2.file_path = ?2
             INNER JOIN symbols resolved ON resolved.id = ie.target_id
             WHERE s.name = ?1 AND s.kind != 'import'
                 AND s.file_path = resolved.file_path
             LIMIT 1",
        )?;

        let mut same_dir_stmt = self
            .conn
            .prepare("SELECT id FROM symbols WHERE name = ?1 AND file_path LIKE ?2 LIMIT 1")?;

        let mut parent_scope_stmt = self.conn.prepare(
            "SELECT s.id FROM symbols s
             INNER JOIN symbols source ON source.id = ?2
             WHERE s.name = ?1 AND s.parent_id = source.parent_id AND s.id != source.id
             LIMIT 1",
        )?;

        let mut anywhere_stmt = self
            .conn
            .prepare("SELECT id, kind FROM symbols WHERE name = ?1 AND kind != 'import' LIMIT 3")?;

        // Heuristic resolve must flip state=1 alongside target_id; otherwise
        // unresolved_edges() (state=0 filter) would still surface the edge to
        // the next LSP pass, wasting a server roundtrip on already-known answers.
        let mut update_stmt = self.conn.prepare(
            "UPDATE edges SET target_id = ?1, resolution_state = 1, resolution_source = ?2
             WHERE id = ?3",
        )?;

        for (edge_id, target_name, edge_file, source_id) in unresolved {
            // `\` is PHP's namespace separator (App\Auth\BaseService).
            let simple_name = target_name
                .rsplit(['.', '\\'])
                .next()
                .unwrap_or(target_name);

            // 1) Same file
            let target_id: Option<String> = same_file_stmt
                .query_row(params![simple_name, edge_file], |row| row.get(0))
                .optional()?;

            if let Some(tid) = target_id {
                update_stmt.execute(params![tid, EdgeProvenance::SameFile.as_str(), edge_id])?;
                resolved += 1;
                continue;
            }

            // 2) Import-path resolution
            let target_id: Option<String> = import_resolve_stmt
                .query_row(params![simple_name, edge_file], |row| row.get(0))
                .optional()?;

            if let Some(tid) = target_id {
                update_stmt.execute(params![tid, EdgeProvenance::ImportPath.as_str(), edge_id])?;
                resolved += 1;
                continue;
            }

            // 3) Same directory
            let dir = edge_file
                .rsplit_once('/')
                .map(|(d, _)| format!("{d}/%"))
                .unwrap_or_default();

            if !dir.is_empty() {
                let target_id: Option<String> = same_dir_stmt
                    .query_row(params![simple_name, dir], |row| row.get(0))
                    .optional()?;

                if let Some(tid) = target_id {
                    update_stmt.execute(params![tid, EdgeProvenance::SameDir.as_str(), edge_id])?;
                    resolved += 1;
                    continue;
                }
            }

            // 4) Parent scope preference
            let target_id: Option<String> = parent_scope_stmt
                .query_row(params![simple_name, source_id], |row| row.get(0))
                .optional()?;

            if let Some(tid) = target_id {
                update_stmt.execute(params![tid, EdgeProvenance::ParentScope.as_str(), edge_id])?;
                resolved += 1;
                continue;
            }

            // 5+6) Project-wide: unique match, or class-over-constructor disambiguation
            let mut rows = anywhere_stmt.query(params![simple_name])?;
            let mut matches: Vec<(String, String)> = Vec::new();
            while let Some(row) = rows.next()? {
                matches.push((row.get(0)?, row.get(1)?));
                if matches.len() == 3 {
                    break;
                }
            }
            drop(rows);

            // Tier and target are decided together so the provenance label can
            // never drift from the branch that picked the id.
            let resolution = match matches.as_slice() {
                [(id, _)] => Some((id, EdgeProvenance::UniqueGlobal)),
                [a, b] => disambiguate_two(a, b).map(|id| (id, EdgeProvenance::KindDisambig)),
                _ => None,
            };

            if let Some((tid, prov)) = resolution {
                update_stmt.execute(params![tid, prov.as_str(), edge_id])?;
                resolved += 1;
            }
        }

        Ok(resolved)
    }

    /// Compute and store in-degree centrality for all symbols.
    ///
    /// In-degree = number of resolved incoming edges (calls, imports, inherits, etc.).
    /// Higher in-degree means the symbol is referenced more across the codebase.
    /// Resets all in-degree values to 0 first, then batch-updates from the edges table.
    ///
    /// tx-safe: two unconditional statements participate in any active outer
    /// transaction — see [`Self::begin_indexing_tx`].
    pub fn compute_in_degrees(&self) -> Result<u32> {
        self.conn
            .execute("UPDATE symbols SET in_degree = 0", [])
            .context("Failed to reset in-degree counts")?;

        // UPDATE...FROM joins each count to its symbol by PK once. A correlated
        // subquery here re-scans the counts set per row — O(symbols×edges).
        let updated = self
            .conn
            .execute(
                "UPDATE symbols SET in_degree = counts.cnt
             FROM (
                 SELECT target_id, COUNT(*) AS cnt
                 FROM edges WHERE target_id IS NOT NULL
                 GROUP BY target_id
             ) AS counts
             WHERE symbols.id = counts.target_id",
                [],
            )
            .context("Failed to compute in-degree counts")?;

        Ok(updated as u32)
    }

    // ── Scoped resolution (incremental indexing) ──

    /// Invalidate resolved edges that point into any of the dirty files.
    ///
    /// When a file is re-indexed, its symbols may have been renamed/removed.
    /// Edges from *unchanged* files that previously resolved to those symbols
    /// must be cleared so they can be re-resolved against the new symbol set.
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn invalidate_edges_targeting(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        if dirty_files.is_empty() {
            return Ok(0);
        }
        // After file re-indexing, edges from unchanged files may point to
        // symbol IDs that no longer exist (removed or renamed symbols).
        // Set these dangling references to NULL AND reset resolution_state so
        // the heuristic + LSP passes get another shot. Without the state reset
        // the edge would stay at state=1 (resolved) but with target_id NULL —
        // permanently invisible to `unresolved_edges()`.
        let n = self
            .conn
            .execute(
                "UPDATE edges SET target_id = NULL, resolution_state = 0, resolution_source = NULL
             WHERE target_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM symbols WHERE symbols.id = edges.target_id)",
                [],
            )
            .context("Failed to invalidate edges targeting dirty files")?;
        Ok(n as u32)
    }

    /// Resolve edges scoped to dirty files only.
    ///
    /// Processes: edges originating from dirty files (freshly extracted)
    /// and edges whose target was just invalidated (target_id set to NULL).
    /// Uses the same 6-tier heuristic as `resolve_edges`.
    /// Resolve edges after scoped invalidation.
    ///
    /// After `invalidate_edges_targeting` has cleared target_ids for edges
    /// pointing into dirty files, this re-resolves all currently unresolved edges.
    /// Fewer edges are unresolved compared to a first-time full resolve.
    pub fn resolve_edges_scoped(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("Failed to begin scoped edge-resolution transaction")?;
        let total = self.resolve_edges_scoped_in_tx(dirty_files)?;
        tx.commit()
            .context("Failed to commit scoped edge-resolution transaction")?;
        Ok(total)
    }

    /// Like [`Self::resolve_edges_scoped`] but assumes the caller already
    /// holds an open transaction.
    pub fn resolve_edges_scoped_in_tx(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        if dirty_files.is_empty() {
            return Ok(0);
        }
        // After invalidation, the set of unresolved edges is naturally scoped:
        // only edges from dirty files (freshly extracted) or targeting dirty files
        // (just invalidated) have target_id = NULL.
        // Reuse the same 2-pass resolution.
        self.resolve_edges_in_tx()
    }

    /// Mark every still-unresolved edge as `resolution_state = 4`
    /// (heuristic-exhausted, no LSP pass ran).
    ///
    /// Call ONLY at the end of an index run that did not (and will not) run the
    /// LSP pass — i.e. `--no-lsp`, `cartog watch`, or a feature-`lsp`-off build.
    /// In those runs the 6-tier heuristic is the only resolver, so a leftover
    /// state=0 edge is permanently unresolvable until the symbol graph changes.
    /// Without this marker every incremental re-index re-walks the whole
    /// state=0 backlog (the watch-mode amplification from #109): the
    /// `resolution_state = 0` scan in `resolve_edges_pass` grows with the
    /// permanent-failure set, not just the dirty edges.
    ///
    /// State 4 is sticky like {2, 3} and re-enters the unresolved set the same
    /// way: [`Self::reset_unresolvable_for_names`] when a matching symbol is
    /// added, [`Self::reset_all_unresolvable`] on `--force`, or
    /// [`Self::reopen_heuristic_exhausted`] before a later LSP-enabled reindex.
    /// Returns the number of edges marked.
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn mark_heuristic_exhausted_in_tx(&self) -> Result<u32> {
        let n = self
            .conn
            .execute(
                "UPDATE edges SET resolution_state = 4
             WHERE target_id IS NULL AND resolution_state = 0",
                [],
            )
            .context("Failed to mark edges heuristic-exhausted")?;
        Ok(n as u32)
    }

    /// Recompute in-degree centrality after an incremental re-index.
    ///
    /// Scoping the reset to dirty files cannot be correct: a symbol in an
    /// unchanged file that *lost* its incoming edge is unfindable once the
    /// dirty file's old edges are deleted. Instead, correct every symbol
    /// whose stored value disagrees with the actual edge count — a full
    /// ground-truth pass that writes only the rows that changed (unlike
    /// [`Self::compute_in_degrees`], which rewrites all rows).
    ///
    /// tx-safe: every internal statement participates in any active outer
    /// transaction — see [`Self::begin_indexing_tx`]. Does NOT open one of
    /// its own, unlike the batched `*_in_tx` helpers; outside an outer
    /// transaction the zeroing is not atomic with the recompute.
    pub fn compute_in_degrees_scoped(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        if dirty_files.is_empty() {
            return Ok(0);
        }

        // Zero symbols that no longer have any incoming edge.
        let zeroed = self
            .conn
            .execute(
                "UPDATE symbols SET in_degree = 0
             WHERE in_degree != 0
               AND id NOT IN (SELECT target_id FROM edges WHERE target_id IS NOT NULL)",
                [],
            )
            .context("Failed to zero stale in-degree counts")?;

        // Fix symbols whose stored value disagrees with the actual count.
        // UPDATE...FROM avoids the correlated re-scan (see compute_in_degrees);
        // the `!=` predicate keeps the write set to only changed rows.
        let updated = self
            .conn
            .execute(
                "UPDATE symbols SET in_degree = counts.cnt
             FROM (
                 SELECT target_id, COUNT(*) AS cnt
                 FROM edges WHERE target_id IS NOT NULL
                 GROUP BY target_id
             ) AS counts
             WHERE symbols.id = counts.target_id
               AND symbols.in_degree != counts.cnt",
                [],
            )
            .context("Failed to recompute scoped in-degree counts")?;

        Ok((zeroed + updated) as u32)
    }
}
