//! MCP search/overview tools: search, stats, map, changes.

use std::sync::Arc;

use rmcp::{handler::server::wrapper::Parameters, tool, tool_router, ErrorData as McpError};

use crate::types::*;
use crate::*;
use cartog_core::Compact;
use cartog_db::MAX_SEARCH_LIMIT;
use cartog_indexer as indexer;

#[tool_router(router = search_router, vis = "pub(crate)")]
impl CartogServer {
    /// Search for symbols by name — use this to discover exact names before calling refs/callees/impact.
    #[tool(
        description = "Find symbols by exact or partial name. Use ONLY to get a precise symbol name before calling cartog_refs, cartog_callees, or cartog_impact. Not for: general code discovery (use cartog_rag_search instead — better recall for natural-language queries). Supports prefix and substring matching, case-insensitive. Returns: Symbol[] ranked by centrality (most-referenced first).",
        annotations(
            title = "Search symbols",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<SymbolList>()
    )]
    pub(crate) async fn cartog_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let kind_str = params.kind;
        let file = params.file;
        let limit = params.limit.unwrap_or(30).min(MAX_SEARCH_LIMIT);
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            if query.is_empty() {
                return Err(mcp_err("query cannot be empty"));
            }

            let kind_filter = kind_str
                .as_deref()
                .map(|s| {
                    s.parse::<cartog_core::SymbolKind>().map_err(|_| {
                        mcp_err(
                            "invalid symbol kind. Valid: function, class, method, variable, import, interface, enum, type-alias, trait, module, document",
                        )
                    })
                })
                .transpose()?;

            // Validate file path is within CWD — consistent with cartog_outline / cartog_deps.
            let validated_file: Option<String> = file
                .map(|f| {
                    validate_path_within_cwd_canonical(&f, &cwd)
                        .map_err(mcp_err)
                        .map(|p| p.to_string_lossy().into_owned())
                })
                .transpose()?;
            let file_filter = validated_file.as_deref();
            debug!(query = %query, kind = ?kind_filter, limit, "search");
            let db = db.lock().map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;
            let mut symbols = db
                .search(&query, kind_filter, file_filter, limit)
                .map_err(|e| mcp_err(format!("search failed: {e}")))?;
            if mcp_compact() {
                symbols.compact_in_place();
            }

            let (symbols, omitted) = fit_to_budget(symbols, mcp_list_budget());
            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(SymbolList { results: symbols }).ok();
            tool_response(&db, json, structured, "cartog_search", omitted, stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Index statistics summary.
    #[tool(
        description = "Show index health: file count, symbol count, edge count, and edge resolution buckets. Use to verify the index is built and check coverage. Not for: finding code (use cartog_search or cartog_rag_search). Returns: {num_files, num_symbols, num_edges, num_resolved, num_unresolvable, num_external, languages, symbol_kinds, role, watcher_active}. num_external counts edges whose LSP-resolved target lives outside the indexed root (stdlib, deps, node_modules).",
        annotations(title = "Index stats", read_only_hint = true, open_world_hint = false),
        output_schema = output_schema_for::<StatsResult>()
    )]
    pub(crate) async fn cartog_stats(&self) -> Result<CallToolResult, McpError> {
        let db = Arc::clone(&self.db);
        let role = self.role.load();
        let watcher_active = self
            .watcher_active
            .load(std::sync::atomic::Ordering::Relaxed);
        let degraded = self.is_degraded();

        tokio::task::spawn_blocking(move || {
            debug!("stats");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let stats = db
                .stats()
                .map_err(|e| mcp_err(format!("stats query failed: {e}")))?;

            // `watcher_active=false` on a Primary means either the user did not
            // request `--watch`, or a post-promotion watcher spawn failed (e.g.,
            // another live `cartog watch` holds the watch slot, or notify install
            // failed). The user can distinguish the cases by whether they passed
            // `--watch`.
            let result = StatsResult {
                stats,
                role,
                watcher_active,
                degraded,
            };
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            // cartog_stats bypasses tool_response_named so it must log itself
            // — otherwise MCP-side stats calls disappear from
            // `cartog stats --savings`. Skip the write when degraded (the
            // in-memory placeholder is discarded; logging is pointless noise).
            if !degraded {
                log_tool_query(&db, "cartog_stats");
            }
            // Lead with a plain-text degraded banner so a human reading the
            // tool result sees the "no index yet" state, not just empty counts.
            if degraded {
                let banner = "no index yet — this project has no .cartog.toml and was not \
                    indexed. Run `cartog init` to opt in (the index loads on the next Claude \
                    Code launch), or set CARTOG_AUTO_INIT=1.\n";
                return Ok(success_result(format!("{banner}{json}"), structured));
            }
            Ok(success_result(json, structured))
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Codebase orientation: file list + top symbols by centrality.
    #[tool(
        description = "Orient yourself in an unfamiliar codebase. Returns the full file list plus the top N symbols ranked by reference count (most-used definitions first). Use as the FIRST call when dropped into a new repo, before search or refs. Not for: locating a specific symbol (use cartog_search), or fetching one file's structure (use cartog_outline). Returns: {files: string[], top_symbols: Symbol[]}.",
        annotations(title = "Codebase map", read_only_hint = true, open_world_hint = false),
        output_schema = output_schema_for::<MapResult>()
    )]
    pub(crate) async fn cartog_map(
        &self,
        Parameters(params): Parameters<MapParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(50);
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            debug!(limit, "map");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let files = db
                .all_files()
                .map_err(|e| mcp_err(format!("files query failed: {e}")))?;
            let mut top_symbols = db
                .top_symbols(limit)
                .map_err(|e| mcp_err(format!("top_symbols query failed: {e}")))?;
            if mcp_compact() {
                top_symbols.compact_in_place();
            }

            let (top_symbols, omitted) = fit_to_budget(top_symbols, mcp_list_budget());
            let result = MapResult { files, top_symbols };

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_map", omitted, stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show symbols affected by recent git changes.
    #[tool(
        description = "Show what changed recently. Symbols affected by the last N git commits plus working-tree changes. Use when asked 'what changed?', 'what did I modify?', or to understand recent code activity before a review. Not for: arbitrary git diffs (use Bash with `git diff`). Returns: {changed_files: string[], symbols: Symbol[]}.",
        annotations(
            title = "Recent changes",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<cartog_core::ChangesResult>()
    )]
    pub(crate) async fn cartog_changes(
        &self,
        Parameters(params): Parameters<ChangesParams>,
    ) -> Result<CallToolResult, McpError> {
        let commits = params.commits.unwrap_or(5);
        let kind_str = params.kind;
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            let kind_filter = kind_str
                .as_deref()
                .map(|s| {
                    s.parse::<cartog_core::SymbolKind>().map_err(|_| {
                        mcp_err(
                            "invalid symbol kind. Valid: function, class, method, variable, import, interface, enum, type-alias, trait, module, document",
                        )
                    })
                })
                .transpose()?;

            debug!(commits, kind = ?kind_filter, "changes");

            let root = std::env::current_dir()
                .map_err(|e| mcp_err(format!("cannot determine CWD: {e}")))?;

            let changed_files = indexer::git_recently_changed_files(&root, commits)
                .map_err(|e| mcp_err(format!("git changes failed: {e}")))?;

            let db = db.lock().map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;
            let mut symbols = db
                .symbols_for_files(&changed_files, kind_filter)
                .map_err(|e| mcp_err(format!("symbols query failed: {e}")))?;
            if mcp_compact() {
                symbols.compact_in_place();
            }

            let (symbols, omitted) = fit_to_budget(symbols, mcp_list_budget());
            let result = cartog_core::ChangesResult {
                changed_files,
                symbols,
            };

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_changes", omitted, stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }
}
