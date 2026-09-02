//! MCP search/overview tools: search, search_all, stats, map, changes.

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
                            "invalid symbol kind. Valid: function, class, method, variable, import, interface, enum, enum_member, type_alias, trait, module, document, macro, component",
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
                // Mirrors `refuse_if_degraded`: deliberately does not claim the
                // config is *absent*. A present but rejected `.cartog.toml` also
                // lands here, and this is the most-read of the degraded surfaces
                // (a read tool an agent calls freely), so claiming absence sends
                // the agent after `cartog init`, which cannot fix a broken file.
                let banner = "no index yet — this project has no usable .cartog.toml and was \
                    not indexed. Run `cartog init` to opt in (the index loads on the next \
                    Claude Code launch), or set CARTOG_AUTO_INIT=1. If a .cartog.toml does \
                    exist, check stderr for the config error it reported — fix that and \
                    relaunch.\n";
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
                            "invalid symbol kind. Valid: function, class, method, variable, import, interface, enum, enum_member, type_alias, trait, module, document, macro, component",
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

#[tool_router(router = search_all_router, vis = "pub(crate)")]
impl CartogServer {
    /// Federated exact-symbol search across the machine's other projects.
    ///
    /// Gated by **neither** `refuse_if_degraded` nor `refuse_if_read_only`, for
    /// the same reason as `cartog_list_projects`: it never touches *this*
    /// project's index. It reads the registry, then opens each selected
    /// project's database **read-only** — a registry row grants discovery, not
    /// write access, and the read-only open is the enforcement rather than a
    /// convention. A degraded server is exactly when searching the projects
    /// that *are* indexed matters most.
    ///
    /// Unlike `cartog_list_projects`, this **does** open foreign databases, so
    /// its cost scales with the number of projects queried — hence the
    /// `max_projects` cap and the `under`/`lang` filters.
    #[tool(
        description = "Find a symbol by name across the OTHER cartog-indexed projects on this \
                       machine (not the current one — use cartog_search for that). Use when the \
                       symbol's project is unknown: a sibling service, a shared library. Narrow \
                       with `under` (a directory subtree) or `lang`. Results are grouped per \
                       project and ranked within it — cross-project relevance is NOT comparable, \
                       so there is no merged ranking. Take a result's db_path and pass it as the \
                       `db` argument of another tool to drill in. Not for: natural-language \
                       discovery (no federated semantic search exists). Returns: \
                       {registry_available, projects[{name, db_path, symbols[]}], queried}.",
        annotations(
            title = "Search all projects",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            // Reads only local databases this machine already wrote.
            open_world_hint = false
        ),
        output_schema = output_schema_for::<SearchAllResult>()
    )]
    pub(crate) async fn cartog_search_all(
        &self,
        Parameters(params): Parameters<SearchAllParams>,
    ) -> Result<CallToolResult, McpError> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            debug!("search_all: {}", params.query);
            if params.query.is_empty() {
                return Err(mcp_err("query cannot be empty"));
            }
            let kind = params
                .kind
                .as_deref()
                .map(|s| {
                    s.parse::<cartog_core::SymbolKind>()
                        .map_err(|_| mcp_err("invalid symbol kind"))
                })
                .transpose()?;
            let limit = params.limit.unwrap_or(10).min(MAX_SEARCH_LIMIT);

            let listing = cartog_registry::list_projects(cartog_db::CURRENT_SCHEMA_VERSION);
            if !listing.available {
                // A distinct, honest answer: there is no registry, which is not
                // the same as "no other project matched".
                let empty = SearchAllResult {
                    registry_available: false,
                    projects: Vec::new(),
                    queried: 0,
                    unreadable: Vec::new(),
                    elided_by_cap: 0,
                };
                let structured = serde_json::to_value(&empty).ok();
                return Ok(success_result(
                    "No project registry on this machine, so there are no other projects to \
                     search. Nothing has been indexed yet, or CARTOG_REGISTRY is disabled.\n"
                        .to_string(),
                    structured,
                ));
            }

            let (candidates, elided_by_cap) = select_fanout_candidates(
                listing.projects,
                &db_path,
                params.under.as_deref(),
                params.lang.as_deref(),
                params.max_projects.unwrap_or(10),
            );
            let queried = candidates.len();

            let mut projects = Vec::new();
            let mut unreadable = Vec::new();
            for row in candidates {
                match query_project(&row, &params.query, kind, limit) {
                    Ok(symbols) if symbols.is_empty() => {}
                    Ok(symbols) => projects.push(ProjectMatches {
                        name: row.display_name().to_string(),
                        root: row.root.display().to_string(),
                        db_path: row.db_path.display().to_string(),
                        description: row.description.as_ref().map(|d| d.text.clone()),
                        symbols,
                    }),
                    // Carry the root cause: schema drift, a corrupt file, an
                    // EACCES and a SQLITE_BUSY need different fixes, so one
                    // guessed cause sends the reader after the wrong one.
                    Err(e) => {
                        unreadable.push(format!("{}: {}", row.display_name(), e.root_cause()))
                    }
                }
            }

            // Trim whole projects at the element level so the text block and
            // `structuredContent` are bounded together — an outputSchema tool
            // must always return structuredContent, so capping only the text
            // would leave the structured half unbounded (cf. PR #151).
            let (projects, omitted) = fit_to_budget(projects, mcp_list_budget());

            let result = SearchAllResult {
                registry_available: true,
                projects,
                queried,
                unreadable,
                elided_by_cap,
            };
            let mut text = render_search_all(&result, &params.query);
            if omitted > 0 {
                text.push_str(&format!(
                    "\n({omitted} more project(s) with matches omitted to fit the response \
                     budget.)\n"
                ));
            }
            let structured = serde_json::to_value(&result).ok();
            Ok(success_result(text, structured))
        })
        .await
        .map_err(|e| mcp_err(format!("search_all task panicked: {e}")))?
    }
}

/// Choose which projects a fan-out queries, and count what the cap left out.
///
/// Mirrors `select_candidates` in `crates/cartog/src/commands/search_all.rs`. The
/// logic is duplicated rather than shared because no existing crate can host
/// it: `cartog-registry` deliberately carries no `cartog-db` dependency (so a
/// graph-schema bump never forces a registry migration) and `cartog-db`
/// depends only on `cartog-core`. Keep the two in step.
///
/// Excludes the caller's own project — `cartog_search` covers that, and
/// including it would double-report every hit. Orders most-symbols-first so
/// the cap keeps substantial projects rather than registry order.
pub(crate) fn select_fanout_candidates(
    rows: Vec<cartog_registry::ProjectRow>,
    current_db: &Path,
    under: Option<&str>,
    lang: Option<&str>,
    max_projects: usize,
) -> (Vec<cartog_registry::ProjectRow>, usize) {
    let current = (!current_db.as_os_str().is_empty())
        .then(|| cartog_registry::slot_for_db("serve", current_db));
    let under = under.map(|u| canonical_path(Path::new(u)));

    let mut kept: Vec<cartog_registry::ProjectRow> = rows
        .into_iter()
        .filter(|r| {
            let slot = cartog_registry::slot_for_db("serve", &r.db_path);
            current.as_deref() != Some(slot.as_str()) && current.as_deref() != Some(r.id.as_str())
        })
        .filter(|r| !r.markers.missing)
        .filter(|r| match &under {
            Some(u) => canonical_path(&r.root).starts_with(u),
            None => true,
        })
        .filter(|r| match lang {
            Some(l) => r
                .languages
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(l)),
            None => true,
        })
        .collect();

    kept.sort_by(|a, b| {
        b.symbol_count
            .unwrap_or(0)
            .cmp(&a.symbol_count.unwrap_or(0))
            .then_with(|| a.display_name().cmp(b.display_name()))
    });

    let cap = max_projects.clamp(1, 50);
    let elided = kept.len().saturating_sub(cap);
    kept.truncate(cap);
    (kept, elided)
}

/// Query one foreign project, read-only.
///
/// `open_readonly` also refuses a schema this binary does not own, which is the
/// right outcome: a drifted graph's rows cannot be trusted, so the project is
/// reported unreadable rather than half-answered.
fn query_project(
    row: &cartog_registry::ProjectRow,
    query: &str,
    kind: Option<cartog_core::SymbolKind>,
    limit: u32,
) -> anyhow::Result<Vec<cartog_core::Symbol>> {
    let db = cartog_db::Database::open_readonly(&row.db_path)?;
    let mut symbols = db.search(query, kind, None, limit)?;
    if mcp_compact() {
        symbols.compact_in_place();
    }
    Ok(symbols)
}

/// `canonicalize` when the path exists, else the path as given, so a filter
/// still behaves sensibly for a project whose database has been removed.
fn canonical_path(p: &Path) -> std::path::PathBuf {
    // Expand `~` first: an agent may pass `~/work` literally, and
    // `canonicalize` leaves it alone — so the `starts_with` test would match
    // nothing and the fan-out would silently return zero projects. Mirrors
    // `canonical` in the CLI's search_all.rs.
    let expanded = match p.strip_prefix("~") {
        Ok(rest) => match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            Some(home) => std::path::PathBuf::from(home).join(rest),
            None => p.to_path_buf(),
        },
        Err(_) => p.to_path_buf(),
    };
    expanded.canonicalize().unwrap_or(expanded)
}

fn render_search_all(result: &SearchAllResult, query: &str) -> String {
    if result.projects.is_empty() {
        return format!(
            "No symbols matching '{query}' in {} other project(s).\n",
            result.queried
        );
    }
    let total: usize = result.projects.iter().map(|p| p.symbols.len()).sum();
    let mut out = format!(
        "{total} match(es) for '{query}' across {} of {} project(s). Ranked within each \
         project; cross-project relevance is not comparable.\n",
        result.projects.len(),
        result.queried,
    );
    for p in &result.projects {
        out.push_str(&format!("\n{} ({})\n", p.name, p.root));
        if let Some(d) = &p.description {
            out.push_str(&format!("  {d}\n"));
        }
        for s in &p.symbols {
            out.push_str(&format!(
                "  {} — {}:{}\n",
                s.name, s.file_path, s.start_line
            ));
        }
        out.push_str(&format!("  db: {}\n", p.db_path));
    }
    if !result.unreadable.is_empty() {
        out.push_str(&format!(
            "\nCould not read {} project(s):\n  {}\n",
            result.unreadable.len(),
            result.unreadable.join("\n  "),
        ));
    }
    if result.elided_by_cap > 0 {
        out.push_str(&format!(
            "\n{} more project(s) matched but were not queried — raise max_projects.\n",
            result.elided_by_cap,
        ));
    }
    out
}
