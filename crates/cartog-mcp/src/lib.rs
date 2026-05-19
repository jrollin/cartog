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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MapParams {
    /// Maximum top-ranked symbols to include in the map (default 50).
    /// Symbols are ranked by in-degree centrality so the most-referenced
    /// definitions surface first.
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

/// Run `index_directory` followed by an optional LSP resolution pass.
///
/// Exposed as a free function (rather than inlined in the `cartog_index`
/// tool handler) so integration tests can exercise the LSP gate without
/// constructing a full `CartogServer` (which loads ONNX models).
///
/// LSP pass is skipped on no-op runs (`dirty_files == 0`) — see
/// `cartog-indexer` for the gate rationale.
#[cfg(feature = "lsp")]
fn index_with_optional_lsp(
    db: &Arc<Mutex<Database>>,
    lsp_manager: &Arc<Mutex<cartog_lsp::manager::LspManager>>,
    root: &Path,
    force: bool,
) -> Result<indexer::IndexResult, McpError> {
    let mut result = {
        let db = db.lock().map_err(|_| {
            mcp_err("internal error: database lock poisoned (server restart required)")
        })?;
        indexer::index_directory(&db, root, force, false)
            .map_err(|e| mcp_err(format!("indexing failed: {e}")))?
    };

    if result.dirty_files > 0 {
        let mut mgr = lsp_manager.lock().map_err(|_| {
            mcp_err("internal error: LSP manager lock poisoned (server restart required)")
        })?;
        let db = db.lock().map_err(|_| {
            mcp_err("internal error: database lock poisoned (server restart required)")
        })?;
        match cartog_lsp::lsp_resolve_edges(&db, root, Some(&mut mgr)) {
            Ok(stats) => {
                result.edges_lsp_resolved = stats.resolved;
                result.edges_marked_unresolvable = stats.marked_unresolvable;
                if stats.resolved > 0 {
                    let _ = db.compute_in_degrees();
                }
            }
            Err(e) => {
                tracing::warn!("LSP resolution failed: {e:#}");
            }
        }
    }

    Ok(result)
}

#[cfg(not(feature = "lsp"))]
fn index_with_optional_lsp(
    db: &Arc<Mutex<Database>>,
    _lsp_manager: &(),
    root: &Path,
    force: bool,
) -> Result<indexer::IndexResult, McpError> {
    let db = db
        .lock()
        .map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;
    indexer::index_directory(&db, root, force, false)
        .map_err(|e| mcp_err(format!("indexing failed: {e}")))
}

/// Static routing hints per tool — guides the agent to the next logical step.
fn suggestions_for(tool: &str) -> Option<&'static str> {
    match tool {
        "cartog_index" => Some("Next: use cartog_map to orient yourself, cartog_rag_search to find code, or cartog_search to look up a symbol name."),
        "cartog_map" => Some("Next: use cartog_outline on an interesting file, cartog_rag_search for a concept, or cartog_search for a specific name."),
        "cartog_search" => Some("Next: use cartog_refs to find usages, cartog_callees to trace calls, or cartog_impact to assess blast radius."),
        "cartog_rag_search" => Some("Next: use cartog_outline to see file structure, or cartog_refs to find all usages of a symbol."),
        "cartog_outline" => Some("Next: use Read with offset/limit to see specific lines, or cartog_refs to find usages of a symbol."),
        "cartog_refs" => Some("Next: use cartog_impact to assess blast radius, or cartog_callees to trace what a function calls."),
        "cartog_callees" => Some("Next: use cartog_refs to find callers, or cartog_impact to assess blast radius."),
        "cartog_impact" => Some("Next: read the affected files to plan changes, or use cartog_hierarchy to check class inheritance."),
        "cartog_hierarchy" => Some("Next: use cartog_refs to find usages, or cartog_impact to assess blast radius."),
        "cartog_deps" => Some("Next: use cartog_outline to see file structure, or cartog_refs to find usages of a symbol."),
        "cartog_changes" => Some("Next: use cartog_refs or cartog_impact on changed symbols to understand downstream effects."),
        "cartog_stats" => Some("Next: run cartog_index if empty, or cartog_map to orient yourself in the codebase."),
        "cartog_rag_index" => Some("Next: use cartog_rag_search to query the new embedding index."),
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
        description = "Build or rebuild the code graph index. Run this first before any other cartog tool, or after making code changes to keep the graph current. Incremental by default — only re-indexes changed files. Use force=true if results seem stale. Not for: routine queries (call once per session, not before every read). Returns: {files_indexed, files_skipped, symbols_added, edges_added, edges_resolved, edges_lsp_resolved, edges_marked_unresolvable}."
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
        #[cfg(not(feature = "lsp"))]
        let lsp_manager: () = ();

        tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "indexing directory");
            let result = index_with_optional_lsp(&db, &lsp_manager, &validated, force)?;

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
        description = "Show file structure: functions, classes, methods, imports with signatures and line ranges. Use this INSTEAD of reading a file when you need to understand what's in it — then Read only the specific lines you need. Not for: reading the actual function body (use Read with offset/limit), or finding usages (use cartog_refs). Returns: Symbol[] with {name, kind, signature, line_start, line_end, parent_id, is_async, is_exported}."
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
        description = "Find all usages of a symbol across the codebase. Use when asked 'where is X used?', 'who calls X?', 'who imports X?'. Filter by kind: calls, imports, inherits, references, raises. Requires an exact symbol name — use cartog_search first if unsure of the name. Not for: discovering what a function calls (use cartog_callees), or transitive impact (use cartog_impact). Returns: array of {edge: {kind, target_name, line}, source: Symbol | null}."
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
        description = "Trace what a function calls. Use when asked 'what does X call?', 'show me the call graph of X', or to understand execution flow. Requires an exact symbol name. Not for: finding who calls a function (use cartog_refs with kind=calls). Returns: Edge[] of {kind, target_name, line, file}."
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
        description = "Assess blast radius before refactoring. Shows everything that transitively depends on a symbol up to N hops. Use when asked 'what breaks if I change X?', 'is it safe to rename/delete X?', or before any rename/extract/move/delete refactoring. Not for: direct callers only (use cartog_refs), or what the symbol calls (use cartog_callees). Returns: array of {edge, depth} where depth=1 is direct, depth=2 is one hop away, etc."
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
        description = "Show class inheritance tree. Use when asked 'show the class hierarchy', 'what extends X?', 'what does X inherit from?'. Not for: trait/interface implementations (use cartog_refs with kind=implements). Returns: array of {child: string, parent: string} (symbol names) ordered top-down."
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
        description = "Show what a file imports. Use when asked 'what does this file depend on?', 'show imports for X'. Not for: reverse dependencies (use cartog_refs with kind=imports on the imported module). Returns: Edge[] of {target_name, line} per import statement."
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
        description = "Find symbols by exact or partial name. Use ONLY to get a precise symbol name before calling cartog_refs, cartog_callees, or cartog_impact. Not for: general code discovery (use cartog_rag_search instead — better recall for natural-language queries). Supports prefix and substring matching, case-insensitive. Returns: Symbol[] ranked by centrality (most-referenced first)."
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
        description = "Show index health: file count, symbol count, edge count, resolution rate. Use to verify the index is built and check coverage. Not for: finding code (use cartog_search or cartog_rag_search). Returns: {num_files, num_symbols, num_edges, resolution_rate_percent}."
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

    /// Codebase orientation: file list + top symbols by centrality.
    #[tool(
        description = "Orient yourself in an unfamiliar codebase. Returns the full file list plus the top N symbols ranked by reference count (most-used definitions first). Use as the FIRST call when dropped into a new repo, before search or refs. Not for: locating a specific symbol (use cartog_search), or fetching one file's structure (use cartog_outline). Returns: {files: string[], top_symbols: Symbol[]}."
    )]
    async fn cartog_map(
        &self,
        Parameters(params): Parameters<MapParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(50);
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(limit, "map");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let files = db
                .all_files()
                .map_err(|e| mcp_err(format!("files query failed: {e}")))?;
            let top_symbols = db
                .top_symbols(limit)
                .map_err(|e| mcp_err(format!("top_symbols query failed: {e}")))?;

            #[derive(Serialize)]
            struct MapResult {
                files: Vec<String>,
                top_symbols: Vec<cartog_core::Symbol>,
            }
            let result = MapResult { files, top_symbols };

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            tool_response(&db, json, "cartog_map")
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show symbols affected by recent git changes.
    #[tool(
        description = "Show what changed recently. Symbols affected by the last N git commits plus working-tree changes. Use when asked 'what changed?', 'what did I modify?', or to understand recent code activity before a review. Not for: arbitrary git diffs (use Bash with `git diff`). Returns: {changed_files: string[], symbols: Symbol[]}."
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
        description = "Build the embedding index for semantic search. Optional — cartog_rag_search ALREADY works at FTS5 (BM25) quality without embeddings; only run this when you want vector recall on top. Requires `cartog rag setup` from the CLI first to download the model. Not for: first-time setup of cartog (cartog_index is what you want). Returns: {embedded, skipped, failed, dim}."
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
        description = "Search code by concept, keyword, or natural language — the DEFAULT entry point for finding code. Use when asked 'find code related to...', 'how does X work?', 'show me the authentication logic'. Works even without embeddings (keyword matching alone is already strong). Prefer this over Grep for code discovery. Not for: looking up a known symbol name (use cartog_search instead — more precise). Filter with kind='document' for docs, kind='all' for both. Returns: Symbol[] ranked by relevance with snippet excerpts."
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
                "cartog is a code graph indexer with hybrid keyword + semantic search. \
                 Prefer cartog tools over Grep/Glob/Read for code navigation — each \
                 tool's description tells you when to use it and what it returns. \
                 Default entry points: cartog_map (orient in a new repo), \
                 cartog_rag_search (find code by concept), cartog_search (look up an exact symbol name). \
                 Languages: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, PHP, Markdown.",
            )
    }
}

pub const SERVE_LOCK_SLOT: &str = "serve";

#[derive(Default)]
pub struct ServerOptions {
    /// Directory for the server's PID file (written on startup, removed on
    /// graceful exit). `None` disables PID-file tracking. Consulted by
    /// `cartog self update` to detect a running peer.
    pub pid_lock_dir: Option<PathBuf>,
}

/// Acquire the serve PID lock; returns `Ok(None)` when no lock dir is configured.
pub fn acquire_serve_lock(
    opts: &ServerOptions,
) -> anyhow::Result<Option<cartog_process_lock::ProcessLock>> {
    use anyhow::Context;
    let dir = match opts.pid_lock_dir.as_deref() {
        Some(d) => d,
        None => return Ok(None),
    };
    cartog_process_lock::ProcessLock::acquire(dir, SERVE_LOCK_SLOT)
        .map(Some)
        .with_context(|| format!("failed to acquire serve PID lock at {}", dir.display()))
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
    opts: ServerOptions,
) -> anyhow::Result<()> {
    info!("starting cartog MCP server v{}", env!("CARGO_PKG_VERSION"));

    // Acquire first so a lock failure aborts before opening DB / watcher.
    let _lock = acquire_serve_lock(&opts)?;

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

    // ── PID-file lock tests ──

    #[test]
    fn pid_file_acquired_when_lock_dir_set() {
        let dir = tempfile::TempDir::new().unwrap();
        let opts = ServerOptions {
            pid_lock_dir: Some(dir.path().to_path_buf()),
        };
        let lock = acquire_serve_lock(&opts).expect("acquire");
        assert!(lock.is_some(), "lock should be returned when dir is set");
        let path = dir.path().join(format!("{SERVE_LOCK_SLOT}.pid"));
        assert!(path.exists(), "PID file should exist while lock is held");
        let pid: u32 = std::fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(pid, std::process::id());
        drop(lock);
        assert!(
            !path.exists(),
            "PID file should be removed once the lock is dropped"
        );
    }

    #[test]
    fn pid_file_skipped_when_lock_dir_unset() {
        let opts = ServerOptions::default();
        let lock = acquire_serve_lock(&opts).expect("noop");
        assert!(
            lock.is_none(),
            "no lock dir → no PID file, no guard returned"
        );
    }

    // ── Index + LSP gate tests ──

    #[cfg(feature = "lsp")]
    #[test]
    fn index_with_optional_lsp_skips_lsp_on_noop_reindex() {
        // Regression guard for the MCP-side gate: when no file changed since
        // the previous index, the LSP pass MUST be skipped — otherwise we
        // re-spawn rust-analyzer / pyright on every cartog_index call.
        //
        // Copies the auth fixture to a tempdir so a real source edit can be
        // applied between calls without touching the repo.
        use cartog_db::Database;
        use cartog_lsp::manager::LspManager;

        let fixtures_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/auth");
        if !fixtures_src.exists() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let fixtures = tmp.path().join("auth");
        std::fs::create_dir_all(&fixtures).unwrap();
        for entry in std::fs::read_dir(&fixtures_src).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), fixtures.join(entry.file_name())).unwrap();
        }

        let db = Arc::new(Mutex::new(Database::open_memory().unwrap()));
        let lsp_mgr = Arc::new(Mutex::new(LspManager::new(&fixtures)));

        // First call primes the index. dirty_files > 0 → LSP is allowed (it may
        // resolve nothing if pyright isn't on PATH, but the gate must let it run).
        let r1 = index_with_optional_lsp(&db, &lsp_mgr, &fixtures, false).unwrap();
        assert!(
            r1.dirty_files > 0,
            "first index must report dirty files (got {})",
            r1.dirty_files
        );

        // Second call without changes must be a no-op AND must skip LSP.
        let r2 = index_with_optional_lsp(&db, &lsp_mgr, &fixtures, false).unwrap();
        assert_eq!(r2.dirty_files, 0);
        assert_eq!(
            r2.edges_lsp_resolved, 0,
            "no-op reindex must skip LSP (MCP-side gate broken)"
        );
        assert_eq!(r2.files_indexed, 0);
    }

    #[test]
    fn pid_file_acquire_failure_propagates() {
        // Pointing pid_lock_dir at a regular file makes ProcessLock::acquire
        // fail at create_dir_all; the error must surface to the caller so
        // `cartog serve` aborts rather than silently running unlocked.
        let blocker = tempfile::NamedTempFile::new().unwrap();
        let opts = ServerOptions {
            pid_lock_dir: Some(blocker.path().to_path_buf()),
        };
        let err = acquire_serve_lock(&opts).unwrap_err();
        assert!(
            err.to_string().contains("serve PID lock"),
            "error should mention the lock context, got: {err}"
        );
    }
}
