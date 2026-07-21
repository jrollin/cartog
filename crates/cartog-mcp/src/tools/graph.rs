//! MCP graph-navigation tools: outline, refs, callees, impact, trace, hierarchy, deps.

use std::sync::Arc;

use rmcp::{
    handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData as McpError,
};

use crate::types::*;
use crate::*;
use cartog_core::{Compact, EdgeKind};
use cartog_rag as rag;

#[tool_router(router = graph_router, vis = "pub(crate)")]
impl CartogServer {
    /// Show symbols and structure of a file without reading its content.
    #[tool(
        description = "Show one file's structure: functions, classes, methods, imports with signatures and line ranges. Use this INSTEAD of reading a file when you need to understand what's in it — then Read only the specific lines you need. For understanding how a FEATURE or AREA works (spanning files), prefer cartog_context — it returns the relevant bodies across files in one call. Not for: reading the actual function body (use Read with offset/limit), or finding usages (use cartog_refs). Returns: Symbol[] with {name, kind, signature, line_start, line_end, parent_id, is_async, is_exported}.",
        annotations(title = "Outline file", read_only_hint = true, open_world_hint = false),
        output_schema = output_schema_for::<SymbolList>()
    )]
    pub(crate) async fn cartog_outline(
        &self,
        Parameters(params): Parameters<OutlineParams>,
    ) -> Result<CallToolResult, McpError> {
        let file = params.file;
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            debug!(file = %file, "outline");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let mut symbols = db
                .outline(&file)
                .map_err(|e| mcp_err(format!("outline query failed: {e}")))?;
            if mcp_compact() {
                symbols.compact_in_place();
            }

            let (symbols, omitted) = fit_to_budget(symbols, mcp_list_budget());
            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(SymbolList { results: symbols }).ok();
            tool_response(&db, json, structured, "cartog_outline", omitted, stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Find all references to a symbol (calls, imports, inherits, type references, raises).
    #[tool(
        description = "Find all usages of a symbol across the codebase. Use when asked 'where is X used?', 'who calls X?', 'who imports X?'. Filter by kind: calls, imports, inherits, references, raises. Requires an exact symbol name — use cartog_search first if unsure of the name. Not for: discovering what a function calls (use cartog_callees), or transitive impact (use cartog_impact). Returns: array of {edge: {kind, target_name, line}, source: Symbol | null}.",
        annotations(
            title = "Find references",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<RefList>()
    )]
    pub(crate) async fn cartog_refs(
        &self,
        Parameters(params): Parameters<RefsParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let kind_str = params.kind;
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            let kind_filter = kind_str
                .as_deref()
                .map(|s| {
                    s.parse::<EdgeKind>().map_err(|_| {
                        mcp_err(format!(
                            "invalid edge kind '{s}'. \
                             Valid: calls, imports, inherits, references, raises"
                        ))
                    })
                })
                .transpose()?;

            debug!(name = %name, kind = ?kind_filter, "refs");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let results = db
                .refs(&name, kind_filter)
                .map_err(|e| mcp_err(format!("refs query failed: {e}")))?;

            let compact = mcp_compact();
            let entries: Vec<RefEntry> = results
                .into_iter()
                .map(|(edge, mut sym)| {
                    if compact {
                        if let Some(s) = sym.as_mut() {
                            s.compact_in_place();
                        }
                    }
                    RefEntry { edge, source: sym }
                })
                .collect();

            let (entries, omitted) = fit_to_budget(entries, mcp_list_budget());
            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(RefList { results: entries }).ok();
            tool_response_named(
                &db,
                json,
                structured,
                "cartog_refs",
                omitted,
                Some(&name),
                stale,
            )
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Find what a symbol calls.
    #[tool(
        description = "Trace what a function calls. Use when asked 'what does X call?', 'show me the call graph of X', or to understand execution flow. Requires an exact symbol name. Not for: finding who calls a function (use cartog_refs with kind=calls). Returns: Edge[] of {kind, target_name, line, file}.",
        annotations(
            title = "Trace callees",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<EdgeList>()
    )]
    pub(crate) async fn cartog_callees(
        &self,
        Parameters(params): Parameters<CalleesParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            debug!(name = %name, "callees");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let edges = db
                .callees(&name)
                .map_err(|e| mcp_err(format!("callees query failed: {e}")))?;

            let (edges, omitted) = fit_to_budget(edges, mcp_list_budget());
            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(EdgeList { results: edges }).ok();
            tool_response_named(
                &db,
                json,
                structured,
                "cartog_callees",
                omitted,
                Some(&name),
                stale,
            )
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Transitive impact analysis — what breaks if this symbol changes?
    #[tool(
        description = "Assess blast radius before refactoring. Shows everything that transitively depends on a symbol up to N hops. Use when asked 'what breaks if I change X?', 'is it safe to rename/delete X?', or before any rename/extract/move/delete refactoring. Not for: direct callers only (use cartog_refs), or what the symbol calls (use cartog_callees). Returns: array of {edge, depth} where depth=1 is direct, depth=2 is one hop away, etc.",
        annotations(
            title = "Impact analysis",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<ImpactList>()
    )]
    pub(crate) async fn cartog_impact(
        &self,
        Parameters(params): Parameters<ImpactParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let depth = params.depth.unwrap_or(3).min(MAX_IMPACT_DEPTH);
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            debug!(name = %name, depth, "impact");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let results = db
                .impact(&name, depth)
                .map_err(|e| mcp_err(format!("impact query failed: {e}")))?;

            let entries: Vec<ImpactEntry> = results
                .into_iter()
                .map(|(edge, d)| ImpactEntry { edge, depth: d })
                .collect();

            let (entries, omitted) = fit_to_budget(entries, mcp_list_budget());
            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(ImpactList { results: entries }).ok();
            tool_response_named(
                &db,
                json,
                structured,
                "cartog_impact",
                omitted,
                Some(&name),
                stale,
            )
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Find a call path between two symbols, with each hop's body inline.
    #[tool(
        description = "Trace how one symbol reaches another through the call graph. Returns the shortest call path from `from` to `to`, each hop carrying the calling symbol's body inline. Use when asked 'how does A reach B?', 'trace the execution flow from X to Y', 'what's the call path between X and Y?'. Only statically-resolved `calls` edges are followed (dynamic dispatch is not traced). Not for: all callers (use cartog_refs), or blast radius (use cartog_impact). Returns: {found: bool, hops: [{source_name, target_name, kind, file_path, line, body?}]}.",
        annotations(
            title = "Trace call path",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<TraceList>()
    )]
    pub(crate) async fn cartog_trace(
        &self,
        Parameters(params): Parameters<TraceParams>,
    ) -> Result<CallToolResult, McpError> {
        let from = params.from;
        let to = params.to;
        let depth = params.depth.unwrap_or(8).min(MAX_TRACE_DEPTH);
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            debug!(from = %from, to = %to, depth, "trace");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let path = db
                .trace(&from, &to, depth)
                .map_err(|e| mcp_err(format!("trace query failed: {e}")))?;

            let compact = mcp_compact();
            let hops: Vec<TraceHop> = path
                .iter()
                .flatten()
                .map(|h| TraceHop {
                    // Compact bounds each hop body to a snippet (preview), keeping
                    // the path readable without shipping full function bodies.
                    body: trace_hop_body(&db, &cwd, &h.source_id).map(|b| {
                        if compact {
                            rag::search::snippet(&b)
                        } else {
                            b
                        }
                    }),
                    source_name: h.source_name.clone(),
                    target_name: h.target_name.clone(),
                    kind: h.kind.to_string(),
                    file_path: h.file_path.clone(),
                    line: h.line,
                })
                .collect();

            let (hops, omitted) = fit_to_budget(hops, mcp_list_budget());
            let result = TraceList {
                found: path.is_some(),
                hops,
            };
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response_named(
                &db,
                json,
                structured,
                "cartog_trace",
                omitted,
                Some(&from),
                stale,
            )
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show inheritance hierarchy for a class.
    #[tool(
        description = "Show class inheritance tree. Use when asked 'show the class hierarchy', 'what extends X?', 'what does X inherit from?'. Not for: trait/interface implementations (use cartog_refs with kind=implements). Returns: array of {child: string, parent: string} (symbol names) ordered top-down.",
        annotations(
            title = "Class hierarchy",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<HierarchyList>()
    )]
    pub(crate) async fn cartog_hierarchy(
        &self,
        Parameters(params): Parameters<HierarchyParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            debug!(name = %name, "hierarchy");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let pairs = db
                .hierarchy(&name)
                .map_err(|e| mcp_err(format!("hierarchy query failed: {e}")))?;

            let entries: Vec<HierarchyEntry> = pairs
                .into_iter()
                .map(|(child, parent)| HierarchyEntry { child, parent })
                .collect();

            let (entries, omitted) = fit_to_budget(entries, mcp_list_budget());
            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(HierarchyList { results: entries }).ok();
            tool_response_named(
                &db,
                json,
                structured,
                "cartog_hierarchy",
                omitted,
                Some(&name),
                stale,
            )
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// File-level import dependencies.
    #[tool(
        description = "Show what a file imports. Use when asked 'what does this file depend on?', 'show imports for X'. Not for: reverse dependencies (use cartog_refs with kind=imports on the imported module). Returns: Edge[] of {target_name, line} per import statement.",
        annotations(
            title = "File dependencies",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<EdgeList>()
    )]
    pub(crate) async fn cartog_deps(
        &self,
        Parameters(params): Parameters<DepsParams>,
    ) -> Result<CallToolResult, McpError> {
        let file = params.file;
        let db = Arc::clone(&self.db);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            debug!(file = %file, "deps");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let edges = db
                .file_deps(&file)
                .map_err(|e| mcp_err(format!("deps query failed: {e}")))?;

            let (edges, omitted) = fit_to_budget(edges, mcp_list_budget());
            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(EdgeList { results: edges }).ok();
            tool_response(&db, json, structured, "cartog_deps", omitted, stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }
}
