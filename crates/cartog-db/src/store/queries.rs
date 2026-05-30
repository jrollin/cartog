//! Graph traversal queries: search, refs, callees, impact, hierarchy, deps, stats.
//!
//! Part of the [`Database`](super::Database) impl, split out of `lib.rs` for navigability.

use super::*;

impl Database {
    // ── Queries ──

    /// Search for symbols by name — case-insensitive, prefix match ranks before substring.
    ///
    /// `%` and `_` in `query` are treated as literals, not LIKE wildcards.
    /// Note: `LOWER()` in SQLite is ASCII-only, which is acceptable for code identifiers.
    /// Returns an error if `query` is empty or `limit` is zero.
    pub fn search(
        &self,
        query: &str,
        kind_filter: Option<SymbolKind>,
        file_filter: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Symbol>> {
        anyhow::ensure!(!query.is_empty(), "search query cannot be empty");
        anyhow::ensure!(limit > 0, "search limit must be at least 1");

        // Escape LIKE special characters so query is matched literally.
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let kind_str = kind_filter.map(|k| k.as_str());
        // Ranking: match_tier + kind_penalty.
        //   match_tier: 0 = exact, 1 = prefix, 2 = substring
        //   kind_penalty: definitions (function/method/class) = 0, variable = 3, import = 6
        // Definitions always rank above variables/imports across all match tiers:
        //   exact class=0, prefix function=1, substring method=2,
        //   exact variable=3, prefix variable=4, substring variable=5,
        //   exact import=6, ...
        // Within the same rank score, secondary sort by kind (fn < method < class)
        // then by file_path and start_line for determinism.
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file_path, start_line, end_line,
                    start_byte, end_byte, parent_id, signature, visibility,
                    is_async, docstring, in_degree, content_hash, subtree_hash,
                    (CASE
                       WHEN LOWER(name) = LOWER(?1)                    THEN 0
                       WHEN LOWER(name) LIKE LOWER(?2) || '%' ESCAPE '\\' THEN 1
                       ELSE                                                  2
                     END) +
                    (CASE kind
                       WHEN 'function' THEN 0
                       WHEN 'method'   THEN 0
                       WHEN 'class'    THEN 0
                       WHEN 'variable' THEN 3
                       WHEN 'import'   THEN 6
                       ELSE                 3
                     END) AS rank
             FROM symbols
             WHERE LOWER(name) LIKE '%' || LOWER(?2) || '%' ESCAPE '\\'
               AND (?3 IS NULL OR kind = ?3)
               AND (?4 IS NULL OR file_path = ?4)
             ORDER BY rank,
                      in_degree DESC,
                      CASE kind
                        WHEN 'function' THEN 0
                        WHEN 'method'   THEN 1
                        WHEN 'class'    THEN 2
                        ELSE                 3
                      END,
                      file_path, start_line
             LIMIT ?5",
        )?;
        // ?1 = raw query (exact equality), ?2 = escaped query (LIKE patterns), ?3 = kind, ?4 = file, ?5 = limit
        let rows = stmt
            .query_map(
                params![query, escaped, kind_str, file_filter, limit],
                row_to_symbol,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Outline: all symbols in a file, ordered by line.
    pub fn outline(&self, file_path: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                    parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
             FROM symbols WHERE file_path = ?1
             ORDER BY start_line",
        )?;
        let rows = stmt
            .query_map(params![file_path], row_to_symbol)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Find what a symbol calls (edges originating from symbols matching the name).
    pub fn callees(&self, name: &str) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line
             FROM edges e
             JOIN symbols s ON e.source_id = s.id
             WHERE s.name = ?1 AND e.kind = 'calls'",
        )?;
        let rows = stmt
            .query_map(params![name], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// All references to a name, with the source symbol resolved.
    /// Optionally filter by edge kind.
    pub fn refs(
        &self,
        name: &str,
        kind_filter: Option<EdgeKind>,
    ) -> Result<Vec<(Edge, Option<Symbol>)>> {
        // Use a LEFT JOIN to resolve target_id → symbol name instead of a correlated subquery.
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(Edge, Option<Symbol>)> {
            let kind_str = row.get::<_, String>(4)?;
            let kind = kind_str.parse().unwrap_or(EdgeKind::References);
            let edge = Edge {
                source_id: row.get(1)?,
                target_name: row.get(2)?,
                target_id: row.get(3)?,
                kind,
                file_path: row.get(5)?,
                line: row.get(6)?,
            };
            let sym: Option<Symbol> = if row.get::<_, Option<String>>(7)?.is_some() {
                Some(row_to_symbol_offset(row, 7)?)
            } else {
                None
            };
            Ok((edge, sym))
        };

        let rows = if let Some(kind) = kind_filter {
            let mut stmt = self.conn.prepare_cached(
                "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line,
                        s.id, s.name, s.kind, s.file_path, s.start_line, s.end_line,
                        s.start_byte, s.end_byte, s.parent_id, s.signature, s.visibility,
                        s.is_async, s.docstring, s.in_degree, s.content_hash, s.subtree_hash
                 FROM edges e
                 LEFT JOIN symbols s ON e.source_id = s.id
                 LEFT JOIN symbols sym2 ON e.target_id = sym2.id
                 WHERE (e.target_name = ?1 OR sym2.name = ?1)
                   AND e.kind = ?2",
            )?;
            let rows = stmt
                .query_map(params![name, kind.as_str()], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        } else {
            let mut stmt = self.conn.prepare_cached(
                "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line,
                        s.id, s.name, s.kind, s.file_path, s.start_line, s.end_line,
                        s.start_byte, s.end_byte, s.parent_id, s.signature, s.visibility,
                        s.is_async, s.docstring, s.in_degree, s.content_hash, s.subtree_hash
                 FROM edges e
                 LEFT JOIN symbols s ON e.source_id = s.id
                 LEFT JOIN symbols sym2 ON e.target_id = sym2.id
                 WHERE e.target_name = ?1 OR sym2.name = ?1",
            )?;
            let rows = stmt
                .query_map(params![name], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        Ok(rows)
    }

    /// Inheritance hierarchy rooted at a class.
    pub fn hierarchy(&self, class_name: &str) -> Result<Vec<(String, String)>> {
        // Returns (child, parent) pairs
        let mut stmt = self.conn.prepare(
            "SELECT s.name, e.target_name
             FROM edges e
             JOIN symbols s ON e.source_id = s.id
             WHERE e.kind = 'inherits'
               AND (s.name = ?1 OR e.target_name = ?1)",
        )?;
        let rows = stmt
            .query_map(params![class_name], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// File-level dependencies (imports from a file).
    pub fn file_deps(&self, file_path: &str) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line
             FROM edges e
             WHERE e.file_path = ?1 AND e.kind = 'imports'",
        )?;
        let rows = stmt
            .query_map(params![file_path], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Transitive impact analysis: everything reachable within `depth` hops.
    ///
    /// Evaluated as a single recursive CTE rather than iterating `refs()` per
    /// frontier node — saves N round-trips and lets SQLite's planner amortize
    /// the LEFT JOINs. Each unique edge is returned once, labeled with the
    /// minimum depth at which it was reached.
    pub fn impact(&self, name: &str, max_depth: u32) -> Result<Vec<(Edge, u32)>> {
        if max_depth == 0 {
            return Ok(Vec::new());
        }

        let sql = "
            WITH RECURSIVE impacted(
                edge_id, source_id, target_name, target_id, kind,
                file_path, line, source_name, depth
            ) AS (
                SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind,
                       e.file_path, e.line, s.name, 1
                FROM edges e
                LEFT JOIN symbols s ON e.source_id = s.id
                LEFT JOIN symbols sym2 ON e.target_id = sym2.id
                WHERE e.target_name = ?1 OR sym2.name = ?1

                UNION

                SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind,
                       e.file_path, e.line, s.name, i.depth + 1
                FROM impacted i
                JOIN edges e
                  ON (e.target_name = i.source_name
                      OR EXISTS (
                          SELECT 1 FROM symbols t
                          WHERE t.id = e.target_id AND t.name = i.source_name
                      ))
                LEFT JOIN symbols s ON e.source_id = s.id
                WHERE i.source_name IS NOT NULL AND i.depth < ?2
            )
            SELECT source_id, target_name, target_id, kind, file_path, line,
                   MIN(depth) AS depth
            FROM impacted
            GROUP BY edge_id
            ORDER BY depth, edge_id
        ";

        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map(params![name, max_depth], |row| {
                let kind_str: String = row.get(3)?;
                let kind = kind_str.parse().unwrap_or(EdgeKind::References);
                let edge = Edge {
                    source_id: row.get(0)?,
                    target_name: row.get(1)?,
                    target_id: row.get(2)?,
                    kind,
                    file_path: row.get(4)?,
                    line: row.get(5)?,
                };
                let depth: u32 = row.get(6)?;
                Ok((edge, depth))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// True when no symbols have been indexed yet (fresh/empty DB). Cheap —
    /// a single `EXISTS(SELECT 1 FROM symbols)` that stops at the first row.
    /// Used by query commands to distinguish "no index yet" from a no-match.
    pub fn is_empty(&self) -> Result<bool> {
        let exists: bool =
            self.conn
                .query_row("SELECT EXISTS(SELECT 1 FROM symbols)", [], |row| row.get(0))?;
        Ok(!exists)
    }

    /// Index statistics.
    pub fn stats(&self) -> Result<IndexStats> {
        let num_files: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let num_symbols: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        // Single GROUP BY over edges replaces what would otherwise be four
        // sequential full table scans (one COUNT(*) per bucket). The partial
        // index `idx_edges_unresolved` only covers state=0, so state=2 and
        // state=3 counts can't use it — one scan + a 4-row Vec is cheaper.
        let mut bucket_stmt = self
            .conn
            .prepare("SELECT resolution_state, COUNT(*) FROM edges GROUP BY resolution_state")?;
        let mut num_resolved: u32 = 0;
        let mut num_unresolvable: u32 = 0;
        let mut num_external: u32 = 0;
        let mut num_edges: u32 = 0;
        let rows = bucket_stmt.query_map([], |row| {
            let state: i64 = row.get(0)?;
            let count: u32 = row.get(1)?;
            Ok((state, count))
        })?;
        for row in rows {
            let (state, count) = row?;
            num_edges += count;
            match state {
                1 => num_resolved = count,
                2 => num_unresolvable = count,
                3 => num_external = count,
                _ => {} // state=0 (unresolved) or any future state
            }
        }

        let mut lang_stmt = self.conn.prepare(
            "SELECT language, COUNT(*) FROM files GROUP BY language ORDER BY COUNT(*) DESC",
        )?;
        let languages: Vec<(String, u32)> = lang_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut kind_stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC")?;
        let symbol_kinds: Vec<(String, u32)> = kind_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(IndexStats {
            num_files,
            num_symbols,
            num_edges,
            num_resolved,
            num_unresolvable,
            num_external,
            languages,
            symbol_kinds,
        })
    }

    /// Record a successful query against the index for the `cartog stats --savings`
    /// / `cartog savings` retention hook.
    ///
    /// Best-effort: errors are swallowed (logged via `warn!`) so a failing
    /// write never aborts the user's actual query.
    ///
    /// **Read-only attach skips the write.** Secondary MCP servers opened via
    /// [`Self::open_readonly`] cannot write at all. As a result, queries
    /// served by a secondary are NOT reflected in `query_log` — there is no
    /// IPC that forwards them to the primary. `cartog stats --savings` on a
    /// machine that runs multiple MCP servers will therefore *undercount*
    /// secondary traffic, not overcount it. This is a deliberate trade-off:
    /// the alternative would be a separate per-process file with its own
    /// merge logic, which is more complexity than the retention hook needs.
    ///
    /// Stored fields: `tool` (e.g. `"search"`, `"refs"`, MCP-side already
    /// strips the `cartog_` prefix so CLI and MCP rows aggregate), `source`
    /// (`"cli"` or `"mcp"`), and a unix-seconds timestamp. The query payload
    /// itself is never recorded — see the privacy banner in README.
    pub fn log_query(&self, tool: &str, source: &str) {
        if self.is_read_only() {
            return;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Err(e) = self.conn.execute(
            "INSERT INTO query_log (tool, source, ts) VALUES (?1, ?2, ?3)",
            params![tool, source, ts],
        ) {
            // Always warn for the individual failure (debuggable in traces),
            // and additionally emit a one-shot loud-error on the first
            // failure so a persistently-broken query_log (e.g. SQLITE_FULL
            // or a missing table on a manually-tampered DB) is visible even
            // when warns are filtered.
            warn!(error = %e, tool, source, "query_log insert failed");
            if !LOG_QUERY_FAILURE_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::error!(
                    error = %e,
                    "query_log is failing — `cartog stats --savings` will undercount. \
                     Check disk space and `cartog doctor`."
                );
            }
        }
    }

    /// Aggregate `query_log` for `cartog stats --savings` / `cartog savings`.
    /// Safe on read-only attach (it's a read). Returns an empty report when
    /// the `query_log` table is missing (the read-only attach path skips
    /// schema bootstrap, so a v5 DB that lost the table — manual drop, partial
    /// snapshot restore — would otherwise surface a `no such table` error).
    pub fn savings_breakdown(&self) -> Result<SavingsReport> {
        // Probe for the table once. Only treat "no such table" as the empty-
        // report case so real DB faults (corruption, locked, permissions)
        // still propagate to the caller.
        if let Err(e) = self.conn.prepare("SELECT 1 FROM query_log LIMIT 0") {
            if is_no_such_table(&e) {
                return Ok(empty_savings_report());
            }
            return Err(e.into());
        }

        let mut tool_stmt = self.conn.prepare(
            "SELECT tool, COUNT(*) FROM query_log GROUP BY tool ORDER BY COUNT(*) DESC, tool",
        )?;
        let by_tool: Vec<(String, u64)> = tool_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut src_stmt = self.conn.prepare(
            "SELECT source, COUNT(*) FROM query_log GROUP BY source ORDER BY COUNT(*) DESC, source",
        )?;
        let by_source: Vec<(String, u64)> = src_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let total_queries: u64 = by_tool.iter().map(|(_, c)| c).sum();
        let tokens_used_cartog = total_queries.saturating_mul(TOKENS_PER_QUERY_CARTOG as u64);
        let tokens_used_grep = total_queries.saturating_mul(TOKENS_PER_QUERY_GREP as u64);
        let estimated_tokens_saved = tokens_used_grep.saturating_sub(tokens_used_cartog);
        // 0–100, computed with saturating_mul to match the rest of the
        // arithmetic in this function (everything else uses saturating_* to
        // stay safe at extreme query counts).
        let percent_saved = estimated_tokens_saved
            .saturating_mul(100)
            .checked_div(tokens_used_grep)
            .unwrap_or(0)
            .min(100) as u8;

        Ok(SavingsReport {
            by_tool,
            by_source,
            total_queries,
            tokens_used_cartog,
            tokens_used_grep,
            estimated_tokens_saved,
            percent_saved,
            baseline_delta: TOKENS_SAVED_PER_QUERY,
        })
    }

    /// Get all non-import symbols ordered by in-degree (highest first), then by file.
    ///
    /// Used by `cartog map` to produce a centrality-ranked codebase summary.
    pub fn top_symbols(&self, limit: u32) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                    parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
             FROM symbols
             WHERE kind != 'import' AND kind != 'variable'
             ORDER BY in_degree DESC, file_path, start_line
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], row_to_symbol)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Returns `true` if at least one file has been indexed.
    ///
    /// Cheaper than [`Database::stats`] for the common "is the index empty?" check —
    /// SQLite can satisfy `LIMIT 1` with a single index seek rather than a full count.
    pub fn has_indexed_files(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM files LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Get symbols for a set of file paths, grouped by file, ordered by line.
    ///
    /// Optionally filter by symbol kind. Only returns symbols for files that
    /// exist in the index. Files with no matching symbols are omitted.
    /// SQLite variable limit per query. Chunking keeps us well under the default
    /// `SQLITE_MAX_VARIABLE_NUMBER` (999 in older builds, 32766 in newer).
    pub(crate) const FILE_CHUNK_SIZE: usize = 500;

    pub fn symbols_for_files(
        &self,
        file_paths: &[String],
        kind_filter: Option<SymbolKind>,
    ) -> Result<Vec<Symbol>> {
        if file_paths.is_empty() {
            return Ok(Vec::new());
        }

        let kind_str = kind_filter.map(|k| k.as_str().to_string());
        let mut all_results = Vec::new();

        for chunk in file_paths.chunks(Self::FILE_CHUNK_SIZE) {
            let placeholders: Vec<_> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let kind_param_idx = chunk.len() + 1;

            let sql = format!(
                "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                        parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
                 FROM symbols
                 WHERE file_path IN ({})
                   AND (?{kind_param_idx} IS NULL OR kind = ?{kind_param_idx})
                 ORDER BY file_path, start_line",
                placeholders.join(", ")
            );
            let mut stmt = self.conn.prepare(&sql)?;

            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                .iter()
                .map(|p| Box::new(p.clone()) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            param_values.push(Box::new(kind_str.clone()));

            let params: Vec<&dyn rusqlite::types::ToSql> =
                param_values.iter().map(|p| &**p).collect();
            let rows = stmt
                .query_map(&*params, row_to_symbol)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            all_results.extend(rows);
        }

        // Re-sort across chunks to maintain file_path, start_line order
        if file_paths.len() > Self::FILE_CHUNK_SIZE {
            all_results.sort_by(|a, b| {
                a.file_path
                    .cmp(&b.file_path)
                    .then(a.start_line.cmp(&b.start_line))
            });
        }

        Ok(all_results)
    }

    /// Get all indexed file paths, sorted alphabetically.
    pub fn all_files(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM files ORDER BY path")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Load (path, hash) pairs for every indexed file in one query.
    ///
    /// Used by the parallel indexer to avoid per-file DB round trips when
    /// deciding whether a file needs re-parsing.
    pub fn all_file_hashes(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT path, hash FROM files")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }
}
