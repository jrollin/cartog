//! MCP server for the cartog code graph.
//!
//! Exposes cartog's graph queries, indexing, and semantic search as 12 MCP tools
//! over stdio transport. Designed for Claude Code, Cursor, and other MCP clients.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use cartog_core::EdgeKind;
use cartog_db::{Database, MAX_SEARCH_LIMIT};
use cartog_indexer as indexer;
use cartog_rag as rag;
use cartog_watch as watch;
use cartog_watch::{WatchConfig, WatchHandle};

const MAX_IMPACT_DEPTH: u32 = 10;

// ── Parameter types ──

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IndexParams {
    /// Directory to index relative to project root (defaults to ".")
    #[serde(default = "default_dot")]
    pub path: String,
    /// Force full re-index, bypassing change detection
    #[serde(default)]
    pub force: bool,
}

fn default_dot() -> String {
    ".".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OutlineParams {
    /// File path relative to project root
    pub file: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RefsParams {
    /// Symbol name to find references for
    pub name: String,
    /// Filter by edge kind: calls, imports, inherits, references, raises
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CalleesParams {
    /// Symbol name to find callees of
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImpactParams {
    /// Symbol name to analyze impact for
    pub name: String,
    /// Maximum traversal depth (default 3, max 10)
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HierarchyParams {
    /// Class name to show hierarchy for
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DepsParams {
    /// File path to show import dependencies for
    pub file: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Case-insensitive query string (prefix + substring match against symbol names)
    pub query: String,
    /// Filter by symbol kind: function, class, method, variable, import, document
    pub kind: Option<String>,
    /// Filter to a specific file path relative to project root
    pub file: Option<String>,
    /// Maximum results to return (default 30, max 100)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RagIndexParams {
    /// Directory to index relative to project root (defaults to ".")
    #[serde(default = "default_dot")]
    pub path: String,
    /// Force re-embed all symbols (ignore existing embeddings)
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChangesParams {
    /// Number of recent commits to consider (default 5)
    pub commits: Option<u32>,
    /// Filter by symbol kind: function, class, method, variable, import, document
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RagSearchParams {
    /// Natural language query for semantic code search
    pub query: String,
    /// Filter by symbol kind: function, class, method, variable, import, interface, enum, type-alias, trait, module, document, all. Defaults to code only (excludes documents).
    pub kind: Option<String>,
    /// Maximum results to return (default 10)
    pub limit: Option<u32>,
}

// ── Response wrappers for JSON serialization ──

#[derive(Debug, Serialize)]
struct RefEntry {
    edge: cartog_core::Edge,
    source: Option<cartog_core::Symbol>,
}

#[derive(Debug, Serialize)]
struct ImpactEntry {
    edge: cartog_core::Edge,
    depth: u32,
}

#[derive(Debug, Serialize)]
struct HierarchyEntry {
    child: String,
    parent: String,
}

// ── Path validation ──

/// Validate that a path is within the given canonical CWD subtree.
/// Returns the resolved path on success, or an error string if the path escapes CWD.
fn validate_path_within_cwd_canonical(
    input: &str,
    cwd_canonical: &Path,
) -> Result<PathBuf, String> {
    // Resolve the input path relative to CWD
    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        cwd_canonical.join(input)
    };

    // Canonicalize if the path exists, otherwise normalize manually
    let resolved = if candidate.exists() {
        candidate
            .canonicalize()
            .map_err(|e| format!("cannot resolve path '{input}': {e}"))?
    } else {
        // For paths that don't exist yet (e.g., new index target), normalize
        // by resolving .. components manually
        normalize_path(&candidate)
    };

    if !resolved.starts_with(cwd_canonical) {
        return Err(format!("path '{input}' is outside the project directory"));
    }

    Ok(resolved)
}

/// Validate that a path is within the current working directory subtree.
/// Returns the canonicalized path on success, or an error if the path escapes CWD.
#[cfg(test)]
fn validate_path_within_cwd(input: &str) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot determine CWD: {e}"))?;
    let cwd_canonical = cwd
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize CWD: {e}"))?;
    validate_path_within_cwd_canonical(input, &cwd_canonical)
}

/// Normalize a path by resolving `.` and `..` components without requiring the path to exist.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

fn mcp_err(msg: impl std::fmt::Display) -> McpError {
    McpError::internal_error(msg.to_string(), None)
}

/// Static routing hints per tool — guides the agent to the next logical step.
fn suggestions_for(tool: &str) -> Option<&'static str> {
    match tool {
        "cartog_index" => Some("Next: use cartog_rag_search to find code, or cartog_search to look up a symbol name."),
        "cartog_search" => Some("Next: use cartog_refs to find usages, cartog_callees to trace calls, or cartog_impact to assess blast radius."),
        "cartog_rag_search" => Some("Next: use cartog_outline to see file structure, or cartog_refs to find all usages of a symbol."),
        "cartog_outline" => Some("Next: use Read with offset/limit to see specific lines, or cartog_refs to find usages of a symbol."),
        "cartog_refs" => Some("Next: use cartog_impact to assess blast radius, or cartog_callees to trace what a function calls."),
        "cartog_callees" => Some("Next: use cartog_refs to find callers, or cartog_impact to assess blast radius."),
        "cartog_impact" => Some("Next: read the affected files to plan changes, or use cartog_hierarchy to check class inheritance."),
        "cartog_hierarchy" => Some("Next: use cartog_refs to find usages, or cartog_impact to assess blast radius."),
        "cartog_deps" => Some("Next: use cartog_outline to see file structure, or cartog_refs to find usages of a symbol."),
        "cartog_changes" => Some("Next: use cartog_refs or cartog_impact on changed symbols to understand downstream effects."),
        _ => None,
    }
}

/// Default upper bound on response size. Keeps individual MCP tool calls
/// well under Claude's ~25K-token tool budget (~4 chars/token ≈ 100KB) with
/// headroom for model-side formatting. Override with `CARTOG_MCP_MAX_BYTES`.
const DEFAULT_MCP_MAX_BYTES: usize = 64 * 1024;

fn mcp_max_bytes() -> usize {
    std::env::var("CARTOG_MCP_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 256) // sanity: don't let a typo trim everything
        .unwrap_or(DEFAULT_MCP_MAX_BYTES)
}

/// Suggest a narrower tool when we've truncated a large response.
fn narrowing_hint_for(tool: &str) -> &'static str {
    match tool {
        "cartog_impact" => "Re-run with a smaller --depth, or call cartog_refs on a specific symbol to narrow the blast radius.",
        "cartog_map" => "Re-run with a smaller --tokens budget, or call cartog_outline on a specific file.",
        "cartog_changes" => "Re-run with a smaller --commits window.",
        "cartog_search" | "cartog_rag_search" => "Re-run with a tighter query or --limit.",
        "cartog_refs" => "Re-run with a more specific symbol name, or filter by --kind.",
        _ => "Re-run with a narrower scope or filter.",
    }
}

/// Build a JSON text response with next-tool suggestions appended.
///
/// Caps total response size at `mcp_max_bytes()` so individual tool calls
/// don't blow the caller's context window. On overflow the payload is cut
/// at a safe char boundary and an overflow notice pointing at a narrower
/// tool is appended.
fn tool_response(db: &Database, json: String, tool: &str) -> Result<CallToolResult, McpError> {
    let is_empty = !db
        .has_indexed_files()
        .map_err(|e| mcp_err(format!("stats check failed: {e}")))?;

    let budget = mcp_max_bytes();
    let (mut text, truncated_bytes) = if json.len() > budget {
        // Leave room for the truncation notice.
        let notice_cap = 256;
        let target = budget.saturating_sub(notice_cap);
        // UTF-8 chars are at most 4 bytes; step back to a char boundary.
        let cut = (target.saturating_sub(3)..=target)
            .rev()
            .find(|&i| json.is_char_boundary(i))
            .unwrap_or(0);
        let removed = json.len() - cut;
        (json[..cut].to_string(), removed)
    } else {
        (json, 0)
    };

    if truncated_bytes > 0 {
        text.push_str(&format!(
            "\n\n(Response truncated: {truncated_bytes} bytes omitted to stay under the \
             {budget}-byte cap. {hint})",
            hint = narrowing_hint_for(tool),
        ));
    } else if is_empty {
        text.push_str("\n\n(Index is empty. Run cartog_index first to build the code graph.)");
    } else if let Some(hint) = suggestions_for(tool) {
        text.push_str("\n\n");
        text.push_str(hint);
    }
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

// ── MCP Server ──

/// MCP server exposing cartog tools over stdio.
///
/// **Lock ordering** (always acquire in this order to avoid deadlocks):
///   `lsp_manager` → `db` → `embedding_provider` → `reranker_provider`
#[derive(Clone)]
pub struct CartogServer {
    #[expect(
        dead_code,
        reason = "stored by convention; routing uses Self::tool_router()"
    )]
    tool_router: ToolRouter<Self>,
    /// Shared database connection, opened once at server start.
    db: Arc<Mutex<Database>>,
    /// Canonicalized CWD captured at server start to avoid repeated syscalls.
    /// Wrapped in `Arc` so clones (required by `#[derive(Clone)]`) are cheap.
    cwd: Arc<Path>,
    /// Cached embedding provider, created once at server start to avoid
    /// reloading the ONNX model (or probing Ollama) on every request.
    embedding_provider: Arc<Mutex<Box<dyn rag::provider::EmbeddingProvider>>>,
    /// Cached reranker provider (if configured).
    reranker_provider: Arc<Mutex<Option<Box<dyn rag::provider::RerankerProvider>>>>,
    /// Persistent LSP manager for warm server reuse across index calls.
    #[cfg(feature = "lsp")]
    lsp_manager: Arc<Mutex<cartog_lsp::manager::LspManager>>,
}

#[tool_router]
impl CartogServer {
    pub fn new(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
    ) -> anyhow::Result<Self> {
        let db = Database::open(db_path, rag_config.resolved_dimension())
            .map_err(|e| anyhow::anyhow!("failed to open database: {e}"))?;
        let cwd = std::env::current_dir()
            .and_then(|p| p.canonicalize())
            .map_err(|e| anyhow::anyhow!("cannot determine CWD: {e}"))?;
        let provider = rag::create_embedding_provider(&rag_config)
            .map_err(|e| anyhow::anyhow!("failed to load embedding model: {e}"))?;
        let reranker = rag::create_reranker_provider(&rag_config.reranker_provider);
        Ok(Self {
            tool_router: Self::tool_router(),
            db: Arc::new(Mutex::new(db)),
            embedding_provider: Arc::new(Mutex::new(provider)),
            reranker_provider: Arc::new(Mutex::new(reranker)),
            #[cfg(feature = "lsp")]
            lsp_manager: Arc::new(Mutex::new(cartog_lsp::manager::LspManager::new(&cwd))),
            cwd: Arc::from(cwd),
        })
    }

    /// Build or rebuild the code graph index for a directory.
    #[tool(
        description = "Build or rebuild the code graph index. Run this first before any other cartog tool, or after making code changes to keep the graph current. Incremental by default — only re-indexes changed files. Use force=true if results seem stale."
    )]
    async fn cartog_index(
        &self,
        Parameters(params): Parameters<IndexParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = params.path;
        let force = params.force;
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        #[cfg(feature = "lsp")]
        let lsp_manager = Arc::clone(&self.lsp_manager);

        tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "indexing directory");

            // Phase 1: heuristic indexing (hold DB lock briefly)
            #[allow(unused_mut)]
            let mut result = {
                let db = db.lock().map_err(|_| {
                    mcp_err("internal error: database lock poisoned (server restart required)")
                })?;
                indexer::index_directory(&db, &validated, force, false)
                    .map_err(|e| mcp_err(format!("indexing failed: {e}")))?
                // db lock released here
            };

            // Phase 2: LSP resolution (holds both locks during LSP IO).
            // This blocks other tool calls for the duration of LSP resolution.
            // Acceptable because MCP serves a single agent session with sequential tool calls.
            // Future optimization: collect LSP results without DB lock, batch-write after.
            #[cfg(feature = "lsp")]
            {
                let mut mgr = lsp_manager.lock().map_err(|_| {
                    mcp_err("internal error: LSP manager lock poisoned (server restart required)")
                })?;
                let db = db.lock().map_err(|_| {
                    mcp_err("internal error: database lock poisoned (server restart required)")
                })?;
                match cartog_lsp::lsp_resolve_edges(&db, &validated, Some(&mut mgr)) {
                    Ok(n) => {
                        result.edges_lsp_resolved = n;
                        if n > 0 {
                            let _ = db.compute_in_degrees();
                        }
                    }
                    Err(e) => {
                        tracing::warn!("LSP resolution failed: {e:#}");
                    }
                }
                // db + mgr locks released here
            }

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let mut text = json;
            if let Some(hint) = suggestions_for("cartog_index") {
                text.push_str("\n\n");
                text.push_str(hint);
            }
            Ok(CallToolResult::success(vec![Content::text(text)]))
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show symbols and structure of a file without reading its content.
    #[tool(
        description = "Show file structure: functions, classes, methods, imports with signatures and line ranges. Use this INSTEAD of reading a file when you need to understand what's in it. Then use Read with offset/limit for specific lines you need."
    )]
    async fn cartog_outline(
        &self,
        Parameters(params): Parameters<OutlineParams>,
    ) -> Result<CallToolResult, McpError> {
        let file = params.file;
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(file = %file, "outline");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let symbols = db
                .outline(&file)
                .map_err(|e| mcp_err(format!("outline query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_outline")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Find all references to a symbol (calls, imports, inherits, type references, raises).
    #[tool(
        description = "Find all usages of a symbol across the codebase. Use when asked 'where is X used?', 'who calls X?', 'who imports X?'. Filter by kind: calls, imports, inherits, references, raises. Requires an exact symbol name — use cartog_search first if unsure of the name."
    )]
    async fn cartog_refs(
        &self,
        Parameters(params): Parameters<RefsParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let kind_str = params.kind;
        let db = Arc::clone(&self.db);

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

            let entries: Vec<RefEntry> = results
                .into_iter()
                .map(|(edge, sym)| RefEntry { edge, source: sym })
                .collect();

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_refs")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Find what a symbol calls.
    #[tool(
        description = "Trace what a function calls. Use when asked 'what does X call?', 'show me the call graph of X', or to understand execution flow. Requires an exact symbol name."
    )]
    async fn cartog_callees(
        &self,
        Parameters(params): Parameters<CalleesParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(name = %name, "callees");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let edges = db
                .callees(&name)
                .map_err(|e| mcp_err(format!("callees query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_callees")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Transitive impact analysis — what breaks if this symbol changes?
    #[tool(
        description = "Assess blast radius before refactoring. Shows everything that transitively depends on a symbol up to N hops. Use when asked 'what breaks if I change X?', 'is it safe to rename/delete X?', or before any rename/extract/move/delete refactoring."
    )]
    async fn cartog_impact(
        &self,
        Parameters(params): Parameters<ImpactParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let depth = params.depth.unwrap_or(3).min(MAX_IMPACT_DEPTH);
        let db = Arc::clone(&self.db);

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

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_impact")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show inheritance hierarchy for a class.
    #[tool(
        description = "Show class inheritance tree. Use when asked 'show the class hierarchy', 'what extends X?', 'what does X inherit from?'. Returns parent-child relationships."
    )]
    async fn cartog_hierarchy(
        &self,
        Parameters(params): Parameters<HierarchyParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let db = Arc::clone(&self.db);

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

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_hierarchy")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// File-level import dependencies.
    #[tool(
        description = "Show what a file imports. Use when asked 'what does this file depend on?', 'show imports for X'. Returns file-level import edges."
    )]
    async fn cartog_deps(
        &self,
        Parameters(params): Parameters<DepsParams>,
    ) -> Result<CallToolResult, McpError> {
        let file = params.file;
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(file = %file, "deps");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let edges = db
                .file_deps(&file)
                .map_err(|e| mcp_err(format!("deps query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_deps")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Search for symbols by name — use this to discover exact names before calling refs/callees/impact.
    #[tool(
        description = "Find symbols by exact or partial name. Use ONLY to get a precise symbol name before calling cartog_refs, cartog_callees, or cartog_impact. For general code discovery, use cartog_rag_search instead. Supports prefix and substring matching, case-insensitive."
    )]
    async fn cartog_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let kind_str = params.kind;
        let file = params.file;
        let limit = params.limit.unwrap_or(30).min(MAX_SEARCH_LIMIT);
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);

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
            let symbols = db
                .search(&query, kind_filter, file_filter, limit)
                .map_err(|e| mcp_err(format!("search failed: {e}")))?;

            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_search")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Index statistics summary.
    #[tool(
        description = "Show index health: file count, symbol count, edge count, resolution rate. Use to verify the index is built and check coverage."
    )]
    async fn cartog_stats(&self) -> Result<CallToolResult, McpError> {
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!("stats");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let stats = db
                .stats()
                .map_err(|e| mcp_err(format!("stats query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&stats)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            Ok(CallToolResult::success(vec![Content::text(json)]))
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show symbols affected by recent git changes.
    #[tool(
        description = "Show what changed recently. Returns symbols affected by the last N git commits plus working tree changes. Use when asked 'what changed?', 'what did I modify?', or to understand recent code activity before a review."
    )]
    async fn cartog_changes(
        &self,
        Parameters(params): Parameters<ChangesParams>,
    ) -> Result<CallToolResult, McpError> {
        let commits = params.commits.unwrap_or(5);
        let kind_str = params.kind;
        let db = Arc::clone(&self.db);

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
            let symbols = db
                .symbols_for_files(&changed_files, kind_filter)
                .map_err(|e| mcp_err(format!("symbols query failed: {e}")))?;

            let result = cartog_core::ChangesResult {
                changed_files,
                symbols,
            };

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_changes")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Build embedding index for semantic code search.
    #[tool(
        description = "Build the embedding index for semantic search. Run after cartog_index to enable vector similarity search. Requires the embedding model (run 'cartog rag setup' from CLI first). Usually not needed — cartog_rag_search works with keyword-only search even without embeddings."
    )]
    async fn cartog_rag_index(
        &self,
        Parameters(params): Parameters<RagIndexParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = params.path;
        let force = params.force;
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        let provider = Arc::clone(&self.embedding_provider);

        tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "rag index");

            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;

            // Ensure the code graph index is up to date first
            let _ = indexer::index_directory(&db, &validated, false, false)
                .map_err(|e| mcp_err(format!("code graph indexing failed: {e}")))?;

            let mut provider = provider.lock().map_err(|_| {
                mcp_err(
                    "internal error: embedding provider lock poisoned (server restart required)",
                )
            })?;
            let result = rag::indexer::index_embeddings(&db, provider.as_mut(), force)
                .map_err(|e| mcp_err(format!("embedding indexing failed: {e}")))?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            Ok(CallToolResult::success(vec![Content::text(json)]))
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Semantic search over code symbols using hybrid FTS5 + vector search.
    #[tool(
        description = "Search code by concept, keyword, or natural language — the DEFAULT entry point for finding code. Use when asked 'find code related to...', 'how does X work?', 'show me the authentication logic'. Returns code by default; use kind='document' for docs, kind='all' for both. Works even without embeddings (keyword matching). Prefer this over Grep for code discovery."
    )]
    async fn cartog_rag_search(
        &self,
        Parameters(params): Parameters<RagSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let kind_str = params.kind;
        let limit = params.limit.unwrap_or(10).min(MAX_SEARCH_LIMIT);
        let db = Arc::clone(&self.db);
        let provider = Arc::clone(&self.embedding_provider);
        let reranker = Arc::clone(&self.reranker_provider);

        tokio::task::spawn_blocking(move || {
            if query.is_empty() {
                return Err(mcp_err("query cannot be empty"));
            }

            debug!(query = %query, kind = ?kind_str, limit, "rag search");
            let db = db.lock().map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;

            let kind_filter = match kind_str.as_deref() {
                Some("all") => rag::search::KindFilter::All,
                Some(s) => {
                    let kind = s.parse::<cartog_core::SymbolKind>().map_err(|_| {
                        mcp_err(
                            "invalid symbol kind. Valid: function, class, method, variable, import, interface, enum, type-alias, trait, module, document, all",
                        )
                    })?;
                    rag::search::KindFilter::Exact(kind)
                }
                None => rag::search::KindFilter::CodeOnly,
            };

            let mut provider = provider
                .lock()
                .map_err(|_| mcp_err("internal error: embedding provider lock poisoned (server restart required)"))?;
            let mut reranker = reranker
                .lock()
                .map_err(|_| mcp_err("internal error: reranker lock poisoned (server restart required)"))?;
            let result = match reranker.as_mut() {
                Some(r) => rag::search::hybrid_search(
                    &db, &query, limit, kind_filter, provider.as_mut(), Some(r.as_mut()),
                ),
                None => rag::search::hybrid_search(
                    &db, &query, limit, kind_filter, provider.as_mut(), None,
                ),
            }.map_err(|e| mcp_err(format!("semantic search failed: {e}")))?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_rag_search")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }
}

#[tool_handler]
impl ServerHandler for CartogServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("cartog", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_instructions(
                "cartog is a code graph indexer with semantic search. \
                 ALWAYS prefer cartog tools over Grep, Glob, and Read for code navigation and search.\n\n\
                 Default entry point: cartog_rag_search — use for ANY code discovery query (keywords, natural language, concepts). \
                 Only fall back to Grep for string literals, config values, or when cartog returns no results.\n\n\
                 Quick reference:\n\
                 - Find code → cartog_rag_search (default)\n\
                 - Get exact symbol name → cartog_search (then feed into refs/callees/impact)\n\
                 - File structure → cartog_outline (instead of reading the file)\n\
                 - Who uses X? → cartog_refs\n\
                 - What does X call? → cartog_callees\n\
                 - Safe to change X? → cartog_impact\n\
                 - Class hierarchy → cartog_hierarchy\n\
                 - File imports → cartog_deps\n\
                 - Recent changes → cartog_changes\n\n\
                 Rules:\n\
                 - Run cartog_index first if the index is empty.\n\
                 - cartog_refs/callees/impact need exact symbol names — use cartog_search to find them.\n\
                 - Only read files when you need actual implementation logic, not structure.\n\n\
                 Supports: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, Markdown (.md).",
            )
    }
}

/// Start the MCP server over stdio.
///
/// When `watch` is true, a background file watcher keeps the index fresh.
/// When `rag` is true (requires `watch`), embeddings are also auto-updated.
pub async fn run_server(
    db_path: &std::path::Path,
    watch: bool,
    rag: bool,
    rag_config: rag::EmbeddingProviderConfig,
) -> anyhow::Result<()> {
    info!("starting cartog MCP server v{}", env!("CARGO_PKG_VERSION"));

    // Optionally spawn a background file watcher
    let db_path_str = db_path.to_string_lossy().into_owned();
    let _watch_handle: Option<WatchHandle> = if watch {
        let cwd = std::env::current_dir()?;
        let mut config = WatchConfig::new(cwd);
        config.rag = rag;
        config.rag_config = rag_config.clone();
        match watch::spawn_watch(config, &db_path_str) {
            Ok(handle) => {
                info!(rag, "background file watcher started");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to start background watcher, continuing without it");
                None
            }
        }
    } else {
        None
    };

    let server = CartogServer::new(db_path, rag_config)?;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    // WatchHandle is dropped here, signaling the watcher thread to stop.
    info!("cartog MCP server stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Path validation tests ──

    #[test]
    fn validate_path_dot_is_allowed() {
        let result = validate_path_within_cwd(".");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_path_subdirectory_is_allowed() {
        let result = validate_path_within_cwd("src");
        // May not exist in test env, but should not be rejected as "outside CWD"
        // (normalize_path handles non-existent paths)
        assert!(result.is_ok() || result.unwrap_err().contains("cannot resolve"));
    }

    #[test]
    fn validate_path_parent_escape_is_rejected() {
        let result = validate_path_within_cwd("../../etc/passwd");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("outside the project directory"),
            "should reject path traversal"
        );
    }

    #[test]
    fn validate_path_absolute_outside_cwd_is_rejected() {
        let result = validate_path_within_cwd("/etc/passwd");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("outside the project directory"),
            "should reject absolute paths outside CWD"
        );
    }

    #[test]
    fn validate_path_absolute_inside_cwd_is_allowed() {
        let cwd = std::env::current_dir().expect("CWD");
        let inside = cwd.join("src");
        let result = validate_path_within_cwd(inside.to_str().expect("utf-8 path"));
        // src/ exists in this project
        assert!(result.is_ok());
    }

    #[test]
    fn validate_path_dotdot_in_middle_is_rejected() {
        let result = validate_path_within_cwd("src/../../etc");
        assert!(result.is_err());
    }

    // ── Normalize path tests ──

    #[test]
    fn normalize_removes_dot() {
        let p = normalize_path(Path::new("/a/./b/./c"));
        assert_eq!(p, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn normalize_resolves_parent() {
        let p = normalize_path(Path::new("/a/b/../c"));
        assert_eq!(p, PathBuf::from("/a/c"));
    }

    // ── Depth capping ──

    /// Verify depth is clamped at MAX_IMPACT_DEPTH.
    #[test]
    fn impact_depth_is_capped() {
        fn resolve_depth(input: Option<u32>) -> u32 {
            input.unwrap_or(3).min(MAX_IMPACT_DEPTH)
        }
        assert_eq!(resolve_depth(Some(999)), MAX_IMPACT_DEPTH);
        assert_eq!(resolve_depth(Some(5)), 5);
    }

    /// Verify default depth when None is provided.
    #[test]
    fn impact_depth_default() {
        fn resolve_depth(input: Option<u32>) -> u32 {
            input.unwrap_or(3).min(MAX_IMPACT_DEPTH)
        }
        assert_eq!(resolve_depth(None), 3);
    }

    // ── Edge kind parsing ──

    #[test]
    fn parse_valid_edge_kinds() {
        assert_eq!("calls".parse::<EdgeKind>().unwrap(), EdgeKind::Calls);
        assert_eq!("imports".parse::<EdgeKind>().unwrap(), EdgeKind::Imports);
        assert_eq!("inherits".parse::<EdgeKind>().unwrap(), EdgeKind::Inherits);
        assert_eq!(
            "references".parse::<EdgeKind>().unwrap(),
            EdgeKind::References
        );
        assert_eq!("raises".parse::<EdgeKind>().unwrap(), EdgeKind::Raises);
    }

    #[test]
    fn parse_invalid_edge_kind_fails() {
        assert!("invalid".parse::<EdgeKind>().is_err());
        assert!("CALLS".parse::<EdgeKind>().is_err());
        assert!("".parse::<EdgeKind>().is_err());
    }

    // ── Tool handler tests (using in-memory DB) ──

    // These test the underlying DB operations that the MCP handlers call.
    // We cannot easily construct MCP tool calls in unit tests without a full
    // server, so we test the DB layer directly with the same patterns.

    #[test]
    fn empty_db_outline_returns_empty() {
        let db = Database::open_memory().expect("in-memory DB");
        let result = db.outline("nonexistent.py").expect("query");
        assert!(result.is_empty());
    }

    #[test]
    fn empty_db_refs_returns_empty() {
        let db = Database::open_memory().expect("in-memory DB");
        let result = db.refs("nonexistent", None).expect("query");
        assert!(result.is_empty());
    }

    #[test]
    fn empty_db_callees_returns_empty() {
        let db = Database::open_memory().expect("in-memory DB");
        let result = db.callees("nonexistent").expect("query");
        assert!(result.is_empty());
    }

    #[test]
    fn empty_db_impact_returns_empty() {
        let db = Database::open_memory().expect("in-memory DB");
        let result = db.impact("nonexistent", 3).expect("query");
        assert!(result.is_empty());
    }

    #[test]
    fn empty_db_hierarchy_returns_empty() {
        let db = Database::open_memory().expect("in-memory DB");
        let result = db.hierarchy("nonexistent").expect("query");
        assert!(result.is_empty());
    }

    #[test]
    fn empty_db_deps_returns_empty() {
        let db = Database::open_memory().expect("in-memory DB");
        let result = db.file_deps("nonexistent.py").expect("query");
        assert!(result.is_empty());
    }

    #[test]
    fn empty_db_search_returns_empty() {
        let db = Database::open_memory().expect("in-memory DB");
        let result = db.search("foo", None, None, 20).expect("query");
        assert!(result.is_empty());
    }

    #[test]
    fn search_limit_is_capped() {
        assert_eq!(999u32.min(MAX_SEARCH_LIMIT), MAX_SEARCH_LIMIT);
        assert_eq!(30u32.min(MAX_SEARCH_LIMIT), 30);
    }

    #[test]
    fn empty_db_stats_returns_zeros() {
        let db = Database::open_memory().expect("in-memory DB");
        let stats = db.stats().expect("query");
        assert_eq!(stats.num_files, 0);
        assert_eq!(stats.num_symbols, 0);
        assert_eq!(stats.num_edges, 0);
        assert_eq!(stats.num_resolved, 0);
    }

    // ── Response serialization tests ──

    #[test]
    fn ref_entry_serializes() {
        let entry = RefEntry {
            edge: cartog_core::Edge::new("src:foo:1", "bar", EdgeKind::Calls, "src/main.py", 10),
            source: None,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"bar\""));
        assert!(json.contains("\"calls\""));
    }

    #[test]
    fn impact_entry_serializes() {
        let entry = ImpactEntry {
            edge: cartog_core::Edge::new("src:foo:1", "bar", EdgeKind::Calls, "src/main.py", 10),
            depth: 2,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"depth\":2"));
    }

    #[test]
    fn hierarchy_entry_serializes() {
        let entry = HierarchyEntry {
            child: "Dog".to_string(),
            parent: "Animal".to_string(),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"Dog\""));
        assert!(json.contains("\"Animal\""));
    }
}
