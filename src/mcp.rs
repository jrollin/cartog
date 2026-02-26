use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::Parameters},
    model::*,
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::db::{Database, DB_FILE, MAX_SEARCH_LIMIT};
use crate::indexer;
use crate::types::EdgeKind;

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
    /// Filter by symbol kind: function, class, method, variable, import
    pub kind: Option<String>,
    /// Filter to a specific file path relative to project root
    pub file: Option<String>,
    /// Maximum results to return (default 20, max 100)
    pub limit: Option<u32>,
}

// ── Response wrappers for JSON serialization ──

#[derive(Debug, Serialize)]
struct RefEntry {
    edge: crate::types::Edge,
    source: Option<crate::types::Symbol>,
}

#[derive(Debug, Serialize)]
struct ImpactEntry {
    edge: crate::types::Edge,
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
fn validate_path_within_cwd_canonical(input: &str, cwd_canonical: &Path) -> Result<PathBuf, String> {
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

/// Build a JSON text response, appending a hint if the DB has no indexed files.
fn json_response(db: &Database, json: String) -> Result<CallToolResult, McpError> {
    // Single lightweight check instead of full stats() (which runs 4 COUNT queries).
    let is_empty = !db
        .has_indexed_files()
        .map_err(|e| mcp_err(format!("stats check failed: {e}")))?;
    if is_empty {
        let hint = "\n\n(Index is empty. Run cartog_index first to build the code graph.)";
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{json}{hint}"
        ))]))
    } else {
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

// ── MCP Server ──

#[derive(Clone)]
pub struct CartogServer {
    tool_router: ToolRouter<Self>,
    /// Shared database connection, opened once at server start.
    db: Arc<Mutex<Database>>,
    /// Canonicalized CWD captured at server start to avoid repeated syscalls.
    /// Wrapped in `Arc` so clones (required by `#[derive(Clone)]`) are cheap.
    cwd: Arc<Path>,
}

#[tool_router]
impl CartogServer {
    pub fn new() -> anyhow::Result<Self> {
        let db = Database::open(DB_FILE)
            .map_err(|e| anyhow::anyhow!("failed to open database: {e}"))?;
        let cwd = std::env::current_dir()
            .and_then(|p| p.canonicalize())
            .map_err(|e| anyhow::anyhow!("cannot determine CWD: {e}"))?;
        Ok(Self {
            tool_router: Self::tool_router(),
            db: Arc::new(Mutex::new(db)),
            cwd: Arc::from(cwd),
        })
    }

    /// Build or rebuild the code graph index for a directory.
    #[tool(
        description = "Build or rebuild the code graph index. Indexes source files with tree-sitter, extracts symbols and edges, stores in SQLite. Incremental by default (only re-indexes changed files)."
    )]
    async fn cartog_index(
        &self,
        Parameters(params): Parameters<IndexParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = params.path;
        let force = params.force;
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);

        tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "indexing directory");

            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let result = indexer::index_directory(&db, &validated, force)
                .map_err(|e| mcp_err(format!("indexing failed: {e}")))?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            Ok(CallToolResult::success(vec![Content::text(json)]))
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show symbols and structure of a file without reading its content.
    #[tool(
        description = "Show symbols and structure of a file (functions, classes, methods, imports with line ranges). Use instead of reading the file when you need structure, not content."
    )]
    async fn cartog_outline(
        &self,
        Parameters(params): Parameters<OutlineParams>,
    ) -> Result<CallToolResult, McpError> {
        let file = params.file;
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(file = %file, "outline");
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let symbols = db
                .outline(&file)
                .map_err(|e| mcp_err(format!("outline query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            json_response(&db, json)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Find all references to a symbol (calls, imports, inherits, type references, raises).
    #[tool(
        description = "Find all references to a symbol. Returns call sites, imports, inheritance, type annotations, and raise/rescue usages. Optionally filter by kind: calls, imports, inherits, references, raises."
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
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let results = db
                .refs(&name, kind_filter)
                .map_err(|e| mcp_err(format!("refs query failed: {e}")))?;

            let entries: Vec<RefEntry> = results
                .into_iter()
                .map(|(edge, sym)| RefEntry { edge, source: sym })
                .collect();

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            json_response(&db, json)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Find what a symbol calls.
    #[tool(
        description = "Find what a symbol calls. Returns all outgoing call edges from functions/methods matching the given name."
    )]
    async fn cartog_callees(
        &self,
        Parameters(params): Parameters<CalleesParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(name = %name, "callees");
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let edges = db
                .callees(&name)
                .map_err(|e| mcp_err(format!("callees query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            json_response(&db, json)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Transitive impact analysis — what breaks if this symbol changes?
    #[tool(
        description = "Transitive impact analysis. Shows everything that transitively depends on a symbol up to N hops. Use before refactoring to assess blast radius."
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
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let results = db
                .impact(&name, depth)
                .map_err(|e| mcp_err(format!("impact query failed: {e}")))?;

            let entries: Vec<ImpactEntry> = results
                .into_iter()
                .map(|(edge, d)| ImpactEntry { edge, depth: d })
                .collect();

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            json_response(&db, json)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Show inheritance hierarchy for a class.
    #[tool(
        description = "Show inheritance hierarchy for a class. Returns parent-child relationships for the given class name."
    )]
    async fn cartog_hierarchy(
        &self,
        Parameters(params): Parameters<HierarchyParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name;
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(name = %name, "hierarchy");
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let pairs = db
                .hierarchy(&name)
                .map_err(|e| mcp_err(format!("hierarchy query failed: {e}")))?;

            let entries: Vec<HierarchyEntry> = pairs
                .into_iter()
                .map(|(child, parent)| HierarchyEntry { child, parent })
                .collect();

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            json_response(&db, json)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// File-level import dependencies.
    #[tool(
        description = "Show file-level import dependencies. Returns all import edges from the given file."
    )]
    async fn cartog_deps(
        &self,
        Parameters(params): Parameters<DepsParams>,
    ) -> Result<CallToolResult, McpError> {
        let file = params.file;
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!(file = %file, "deps");
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let edges = db
                .file_deps(&file)
                .map_err(|e| mcp_err(format!("deps query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            json_response(&db, json)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Search for symbols by name — use this to discover exact names before calling refs/callees/impact.
    #[tool(
        description = "Search symbols by name (case-insensitive prefix + substring match). \
                       Use to discover symbol names before calling refs/callees/impact. \
                       Optionally filter by kind (function|class|method|variable|import) or file path. \
                       Returns up to 100 results ranked: exact match → prefix → substring."
    )]
    async fn cartog_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let kind_str = params.kind;
        let file = params.file;
        let limit = params.limit.unwrap_or(20).min(MAX_SEARCH_LIMIT);
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);

        tokio::task::spawn_blocking(move || {
            if query.is_empty() {
                return Err(mcp_err("query cannot be empty"));
            }

            let kind_filter = kind_str
                .as_deref()
                .map(|s| {
                    s.parse::<crate::types::SymbolKind>().map_err(|_| {
                        mcp_err(
                            "invalid symbol kind. Valid: function, class, method, variable, import",
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
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
            let symbols = db
                .search(&query, kind_filter, file_filter, limit)
                .map_err(|e| mcp_err(format!("search failed: {e}")))?;

            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            json_response(&db, json)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Index statistics summary.
    #[tool(
        description = "Show index statistics: file count, symbol count, edge count, resolution rate, breakdown by language and symbol kind."
    )]
    async fn cartog_stats(&self) -> Result<CallToolResult, McpError> {
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            debug!("stats");
            let db = db.lock().map_err(|_| mcp_err("database lock poisoned"))?;
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
}

#[tool_handler]
impl ServerHandler for CartogServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "cartog".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "cartog is a code graph indexer. It pre-computes a graph of symbols \
                 (functions, classes, methods, imports) and edges (calls, imports, inherits, \
                 type references, raises) using tree-sitter, stored in SQLite.\n\n\
                  Workflow:\n\
                  1. Run cartog_index first to build/update the graph (use force=true if results seem stale).\n\
                  2. Use cartog_search to discover symbol names by partial match before calling refs/callees/impact.\n\
                  3. Use cartog_outline instead of reading a file when you need structure, not content.\n\
                  4. Use cartog_refs to find all usages of a symbol (filter with kind param).\n\
                  5. Use cartog_impact before refactoring to assess blast radius.\n\
                  6. Re-run cartog_index after making code changes to keep the graph current.\n\
                  7. Only fall back to reading files when you need actual implementation logic.\n\n\
                 Supports: Python, TypeScript/JavaScript, Rust, Go, Ruby."
                    .into(),
            ),
        }
    }
}

/// Start the MCP server over stdio.
pub async fn run_server() -> anyhow::Result<()> {
    info!("starting cartog MCP server v{}", env!("CARGO_PKG_VERSION"));

    let server = CartogServer::new()?;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

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
        assert_eq!(20u32.min(MAX_SEARCH_LIMIT), 20);
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
            edge: crate::types::Edge::new("src:foo:1", "bar", EdgeKind::Calls, "src/main.py", 10),
            source: None,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"bar\""));
        assert!(json.contains("\"calls\""));
    }

    #[test]
    fn impact_entry_serializes() {
        let entry = ImpactEntry {
            edge: crate::types::Edge::new("src:foo:1", "bar", EdgeKind::Calls, "src/main.py", 10),
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
