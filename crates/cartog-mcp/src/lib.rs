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
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use cartog_core::EdgeKind;
use cartog_db::{Database, PinnedAttach, MAX_SEARCH_LIMIT};
use cartog_indexer as indexer;
use cartog_rag as rag;
use cartog_watch as watch;
use cartog_watch::{WatchConfig, WatchHandle};

mod progress;

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
    progress_tx: Option<tokio::sync::mpsc::Sender<progress::Phase>>,
    cancel: Option<indexer::CancelProbe<'_>>,
) -> Result<indexer::IndexResult, McpError> {
    let indexer_cb = progress_tx
        .as_ref()
        .map(|tx| progress::indexer_callback(tx.clone()));
    let mut result = {
        let db = db.lock().map_err(|_| {
            mcp_err("internal error: database lock poisoned (server restart required)")
        })?;
        let cb_ref: Option<&(dyn Fn(indexer::ProgressUpdate) + Send + Sync)> =
            indexer_cb.as_ref().map(|f| f as _);
        indexer::index_directory(&db, root, force, false, cb_ref, cancel)
            .map_err(|e| mcp_err(format!("indexing failed: {e}")))?
    };

    if result.dirty_files > 0 {
        if let Some(tx) = progress_tx.as_ref() {
            let _ = tx.try_send(progress::Phase::Custom("resolving with LSP"));
        }
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
    progress_tx: Option<tokio::sync::mpsc::Sender<progress::Phase>>,
    cancel: Option<indexer::CancelProbe<'_>>,
) -> Result<indexer::IndexResult, McpError> {
    let indexer_cb = progress_tx
        .as_ref()
        .map(|tx| progress::indexer_callback(tx.clone()));
    let db = db
        .lock()
        .map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;
    let cb_ref: Option<&(dyn Fn(indexer::ProgressUpdate) + Send + Sync)> =
        indexer_cb.as_ref().map(|f| f as _);
    indexer::index_directory(&db, root, force, false, cb_ref, cancel)
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
    /// Single-writer election role. `Primary` holds the `serve` PID lock
    /// and owns the RW DB connection. `ReadOnly` attached via
    /// [`Database::open_readonly`] because another cartog process owns
    /// the slot — the 2 write tools are gated, the 11 read tools work
    /// unchanged. Mutated atomically when the Phase 5 promoter detects
    /// the primary died and takes over.
    role: Arc<AtomicRole>,
    /// True when this Primary instance has a live file watcher running
    /// (`cartog serve --watch` or post-promotion equivalent). False for
    /// `cartog serve` without `--watch`, for read-only secondaries, and
    /// (importantly) for a Primary whose watcher failed to start after
    /// promotion — surfaced in `cartog_stats` output so users can see
    /// the degraded state.
    watcher_active: Arc<std::sync::atomic::AtomicBool>,
}

/// Role of this MCP server instance under single-writer election.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Primary,
    ReadOnly,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Primary => "primary",
            Role::ReadOnly => "read-only",
        }
    }
}

/// Lock-free, `Clone`-able cell holding the current [`Role`]. Backed by an
/// `AtomicU8` so the Phase 5 promoter task and every tool handler can read
/// and update it without coordinating via the DB mutex.
#[derive(Debug)]
pub struct AtomicRole(std::sync::atomic::AtomicU8);

impl AtomicRole {
    const PRIMARY: u8 = 0;
    const READ_ONLY: u8 = 1;

    fn new(role: Role) -> Self {
        Self(std::sync::atomic::AtomicU8::new(Self::encode(role)))
    }

    fn encode(role: Role) -> u8 {
        match role {
            Role::Primary => Self::PRIMARY,
            Role::ReadOnly => Self::READ_ONLY,
        }
    }

    fn load(&self) -> Role {
        match self.0.load(std::sync::atomic::Ordering::Acquire) {
            Self::PRIMARY => Role::Primary,
            _ => Role::ReadOnly,
        }
    }

    fn store(&self, role: Role) {
        self.0
            .store(Self::encode(role), std::sync::atomic::Ordering::Release);
    }
}

#[tool_router]
impl CartogServer {
    pub fn new(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
    ) -> anyhow::Result<Self> {
        let db = Database::open(db_path, rag_config.resolved_dimension())
            .map_err(|e| anyhow::anyhow!("failed to open database: {e}"))?;
        let cwd = Self::cwd()?;
        let provider = rag::create_embedding_provider(&rag_config)
            .map_err(|e| anyhow::anyhow!("failed to load embedding model: {e}"))?;
        db.reconcile_embedding_fingerprint(&rag::fingerprint_of(provider.as_ref()))
            .map_err(|e| anyhow::anyhow!("failed to reconcile embedding fingerprint: {e}"))?;
        let reranker = rag::create_reranker_provider(&rag_config.reranker_provider);
        Ok(Self {
            tool_router: Self::tool_router(),
            db: Arc::new(Mutex::new(db)),
            embedding_provider: Arc::new(Mutex::new(provider)),
            reranker_provider: Arc::new(Mutex::new(reranker)),
            #[cfg(feature = "lsp")]
            lsp_manager: Arc::new(Mutex::new(cartog_lsp::manager::LspManager::new(&cwd))),
            cwd: Arc::from(cwd),
            role: Arc::new(AtomicRole::new(Role::Primary)),
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Construct a secondary MCP server that attached read-only because
    /// another cartog process owns the `serve` PID lock. Skips schema
    /// migrations and the embedding-fingerprint reconcile (the primary
    /// owns both); the 2 write tools return a clear error at dispatch
    /// time. The 11 read tools work normally.
    pub fn new_read_only(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
    ) -> anyhow::Result<Self> {
        let db = Database::open_readonly(db_path)
            .map_err(|e| anyhow::anyhow!("failed to open database read-only: {e}"))?;
        let cwd = Self::cwd()?;
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
            role: Arc::new(AtomicRole::new(Role::ReadOnly)),
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    fn cwd() -> anyhow::Result<std::path::PathBuf> {
        std::env::current_dir()
            .and_then(|p| p.canonicalize())
            .map_err(|e| anyhow::anyhow!("cannot determine CWD: {e}"))
    }

    /// Role this server is running under: `Primary` owns the lock and the
    /// RW DB; `ReadOnly` attached behind a primary. May change at runtime
    /// when the promoter takes over from a dead primary (Phase 5).
    pub fn role(&self) -> Role {
        self.role.load()
    }

    /// If we're a read-only secondary, return an `McpError` explaining why
    /// the requested write tool isn't available. `None` when this is the
    /// primary and the call should proceed.
    fn refuse_if_read_only(&self, tool: &str) -> Option<McpError> {
        if self.role.load() == Role::ReadOnly {
            // The CLI fallback depends on which write tool was called: the
            // graph index is the cartog graph (`cartog index`), the vector
            // index is the embeddings (`cartog rag index`).
            let cli_cmd = if tool == "cartog_index" {
                "cartog index"
            } else {
                "cartog rag index"
            };
            Some(mcp_err(format!(
                "`{tool}` is unavailable: this cartog instance is read-only because \
                 another cartog process is the primary writer for this project. \
                 The primary may be running a file watcher that picks up changes \
                 automatically. To force a refresh from this terminal, run `{cli_cmd}` \
                 (or stop the primary and restart this MCP server)."
            )))
        } else {
            None
        }
    }

    /// Build or rebuild the code graph index for a directory.
    #[tool(
        description = "Build or rebuild the code graph index. Run this first before any other cartog tool, or after making code changes to keep the graph current. Incremental by default — only re-indexes changed files. Use force=true if results seem stale. Not for: routine queries (call once per session, not before every read). Returns: {files_indexed, files_skipped, symbols_added, edges_added, edges_resolved, edges_lsp_resolved, edges_marked_unresolvable}."
    )]
    async fn cartog_index(
        &self,
        Parameters(params): Parameters<IndexParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(err) = self.refuse_if_read_only("cartog_index") {
            return Err(err);
        }
        let path = params.path;
        let force = params.force;
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        #[cfg(feature = "lsp")]
        let lsp_manager = Arc::clone(&self.lsp_manager);
        #[cfg(not(feature = "lsp"))]
        let lsp_manager: () = ();

        let (progress_tx, forwarder) = match ctx.meta.get_progress_token() {
            Some(token) => {
                let notifier = progress::peer_notifier(ctx.peer.clone());
                let fwd = progress::spawn_forwarder(token, notifier);
                (Some(fwd.tx.clone()), Some(fwd))
            }
            None => (None, None),
        };

        let cancel = ctx.ct.clone();
        let join = tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "indexing directory");
            let probe = || cancel.is_cancelled();
            let probe_ref: Option<&(dyn Fn() -> bool + Send + Sync)> = Some(&probe);
            let result = index_with_optional_lsp(
                &db,
                &lsp_manager,
                &validated,
                force,
                progress_tx,
                probe_ref,
            )?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let mut text = json;
            if let Some(hint) = suggestions_for("cartog_index") {
                text.push_str("\n\n");
                text.push_str(hint);
            }
            Ok(CallToolResult::success(vec![Content::text(text)]))
        })
        .await;

        // Drain the forwarder unconditionally — even on join error or tool
        // error — so the spawned task doesn't leak waiting on `rx.recv()`.
        // The blocking closure already dropped its progress_tx clone; the
        // Forwarder still holds one, so we drop it here to close the channel.
        if let Some(fwd) = forwarder {
            drop(fwd.tx);
            let _ = fwd.join.await;
        }

        join.map_err(|e| mcp_err(format!("task join failed: {e}")))?
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
        let role = self.role.load();
        let watcher_active = self
            .watcher_active
            .load(std::sync::atomic::Ordering::Relaxed);

        tokio::task::spawn_blocking(move || {
            debug!("stats");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let stats = db
                .stats()
                .map_err(|e| mcp_err(format!("stats query failed: {e}")))?;

            // Serialize the base stats then splice the role + watcher
            // status alongside. `watcher_active=false` on a Primary means
            // either the user did not request `--watch`, or a post-
            // promotion watcher spawn failed (e.g., another live
            // `cartog watch` holds the watch slot, or notify install
            // failed). The user can distinguish the cases by checking
            // whether they passed `--watch`.
            let mut value = serde_json::to_value(&stats)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "role".to_string(),
                    serde_json::Value::String(role.as_str().to_string()),
                );
                obj.insert(
                    "watcher_active".to_string(),
                    serde_json::Value::Bool(watcher_active),
                );
            }
            let json = serde_json::to_string_pretty(&value)
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
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(err) = self.refuse_if_read_only("cartog_rag_index") {
            return Err(err);
        }
        let path = params.path;
        let force = params.force;
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        let provider = Arc::clone(&self.embedding_provider);

        let (progress_tx, forwarder) = match ctx.meta.get_progress_token() {
            Some(token) => {
                let notifier = progress::peer_notifier(ctx.peer.clone());
                let fwd = progress::spawn_forwarder(token, notifier);
                (Some(fwd.tx.clone()), Some(fwd))
            }
            None => (None, None),
        };

        let cancel = ctx.ct.clone();
        let join = tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "rag index");

            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;

            let probe = || cancel.is_cancelled();
            let probe_ref: Option<&(dyn Fn() -> bool + Send + Sync)> = Some(&probe);

            // Ensure the code graph index is up to date first. Inner phases are
            // intentionally suppressed: the RAG tool exposes only rag-specific
            // phases (preparing/embedding/storing) so the client-facing
            // vocabulary stays stable.
            let _ = indexer::index_directory(&db, &validated, false, false, None, probe_ref)
                .map_err(|e| mcp_err(format!("code graph indexing failed: {e}")))?;

            let mut provider = provider.lock().map_err(|_| {
                mcp_err(
                    "internal error: embedding provider lock poisoned (server restart required)",
                )
            })?;

            let rag_cb = progress_tx
                .as_ref()
                .map(|tx| progress::rag_callback(tx.clone()));
            let cb_ref: Option<&(dyn Fn(rag::indexer::ProgressUpdate) + Send + Sync)> =
                rag_cb.as_ref().map(|f| f as _);

            let result =
                rag::indexer::index_embeddings(&db, provider.as_mut(), force, cb_ref, probe_ref)
                    .map_err(|e| mcp_err(format!("embedding indexing failed: {e}")))?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            Ok(CallToolResult::success(vec![Content::text(json)]))
        })
        .await;

        // Drain the forwarder unconditionally — see cartog_index for rationale.
        if let Some(fwd) = forwarder {
            drop(fwd.tx);
            let _ = fwd.join.await;
        }

        join.map_err(|e| mcp_err(format!("task join failed: {e}")))?
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

/// Environment variable that, when set to `0`, disables single-writer
/// election (every cartog process opens RW like pre-Phase-2 cartog). The
/// migration-busy-retry from Phase 6a remains the only defense in that mode.
pub const SINGLE_WRITER_ENV: &str = "CARTOG_SINGLE_WRITER";

#[derive(Default)]
pub struct ServerOptions {
    /// Directory for the server's PID file (written on startup, removed on
    /// graceful exit). `None` disables PID-file tracking. Consulted by
    /// `cartog self update` to detect a running peer.
    pub pid_lock_dir: Option<PathBuf>,
}

/// Outcome of trying to claim the `serve` lock at MCP startup.
#[derive(Debug)]
pub enum ServeLockOutcome {
    /// No `pid_lock_dir` configured — election skipped, this process runs
    /// as if it were the only one (legacy behavior, used in tests).
    Untracked,
    /// We won the election; the lock is held until this value is dropped.
    Primary(cartog_process_lock::ProcessLock),
    /// Another cartog process holds the lock. The caller decides whether
    /// to exit cleanly or (later, in Phase 4) attach read-only.
    Held(cartog_process_lock::ActiveLock),
}

/// Read `CARTOG_SINGLE_WRITER` from the environment. Defaults to election
/// enabled; set to `0` / `false` / `no` to opt out.
fn single_writer_election_enabled() -> bool {
    match std::env::var(SINGLE_WRITER_ENV) {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"),
        Err(_) => true,
    }
}

/// Acquire the serve PID lock with single-writer election. Returns
/// [`ServeLockOutcome::Held`] when a live peer already owns the slot so the
/// caller can branch on it (exit cleanly today, attach read-only in a
/// later phase).
pub fn acquire_serve_lock(opts: &ServerOptions) -> anyhow::Result<ServeLockOutcome> {
    let dir = match opts.pid_lock_dir.as_deref() {
        Some(d) => d,
        None => return Ok(ServeLockOutcome::Untracked),
    };
    if !single_writer_election_enabled() {
        // Kill switch: use the old overwrite-on-acquire behavior. We still
        // write our PID file so `cartog self update` and friends see us.
        let lock = cartog_process_lock::ProcessLock::acquire_overwriting(dir, SERVE_LOCK_SLOT)
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to acquire serve PID lock at {} (single-writer election disabled): {e}",
                    dir.display()
                )
            })?;
        return Ok(ServeLockOutcome::Primary(lock));
    }
    match cartog_process_lock::ProcessLock::acquire(dir, SERVE_LOCK_SLOT) {
        Ok(lock) => Ok(ServeLockOutcome::Primary(lock)),
        Err(cartog_process_lock::AcquireError::Held(held)) => Ok(ServeLockOutcome::Held(held)),
        Err(cartog_process_lock::AcquireError::Io(e)) => Err(anyhow::anyhow!(
            "failed to acquire serve PID lock at {}: {e}",
            dir.display()
        )),
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
    opts: ServerOptions,
) -> anyhow::Result<()> {
    info!("starting cartog MCP server v{}", env!("CARGO_PKG_VERSION"));

    // Acquire first so an election loss is resolved before opening DB or
    // spawning the watcher.
    let (role, initial_lock, primary_to_watch) = match acquire_serve_lock(&opts)? {
        ServeLockOutcome::Primary(lock) => (Role::Primary, Some(lock), None),
        ServeLockOutcome::Untracked => (Role::Primary, None, None),
        ServeLockOutcome::Held(held) => {
            info!(
                primary_pid = held.pid,
                primary_start_time = ?held.start_time,
                "another cartog process is the primary writer for this DB \
                 (PID {}); attaching read-only. \
                 Indexing tools will return a read-only error; queries work normally. \
                 Promotion to primary happens automatically if the holder dies.",
                held.pid
            );
            (Role::ReadOnly, None, Some(held))
        }
    };

    // Only the primary owns the watcher: starting one as a secondary would
    // give us two indexers fighting over the DB. Read-only clients ride
    // along on the primary's index updates via WAL.
    let db_path_str = db_path.to_string_lossy().into_owned();
    let initial_watch_handle: Option<WatchHandle> = if watch && role == Role::Primary {
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
        if watch && role == Role::ReadOnly {
            info!(
                "watcher skipped: this is a read-only secondary; the primary owns indexing \
                 (will start automatically on promotion)"
            );
        }
        None
    };

    let server = match role {
        Role::Primary => CartogServer::new(db_path, rag_config.clone())?,
        Role::ReadOnly => CartogServer::new_read_only(db_path, rag_config.clone())?,
    };

    // Reflect initial watcher state on the server's flag so `cartog_stats`
    // surfaces it accurately from request #1. Will be updated by the
    // promoter on a successful post-promotion watcher spawn.
    server.watcher_active.store(
        initial_watch_handle.is_some(),
        std::sync::atomic::Ordering::Relaxed,
    );

    // Shared cells so the promoter (if any) can install the lock + watcher
    // after winning election, and so the cells stay alive for the whole
    // `run_server` lifetime — Drop on shutdown fires here.
    let lock_cell = Arc::new(Mutex::new(initial_lock));
    let watch_cell = Arc::new(Mutex::new(initial_watch_handle));

    let promoter_handle: Option<tokio::task::JoinHandle<()>> = if role == Role::ReadOnly {
        match (primary_to_watch, opts.pid_lock_dir.clone()) {
            (Some(primary), Some(state_dir)) => {
                let pinned = server
                    .db
                    .lock()
                    .ok()
                    .and_then(|g| g.pinned_attach().cloned());
                let cwd = (*server.cwd).to_path_buf();
                Some(tokio::task::spawn(promoter_task(PromoterArgs {
                    db: Arc::clone(&server.db),
                    role: Arc::clone(&server.role),
                    lock_cell: Arc::clone(&lock_cell),
                    watch_cell: Arc::clone(&watch_cell),
                    watcher_active: Arc::clone(&server.watcher_active),
                    db_path: db_path.to_path_buf(),
                    state_dir,
                    cwd,
                    primary,
                    pinned,
                    watch_requested: watch,
                    rag,
                    rag_config,
                    poll_interval: DEFAULT_PROMOTER_POLL_INTERVAL,
                })))
            }
            _ => None,
        }
    } else {
        None
    };

    let service = server.serve(stdio()).await?;

    // Wait for any of: rmcp's normal shutdown (stdin EOF when the parent
    // dies, or an explicit close), SIGINT (Ctrl+C in a foreground terminal),
    // or SIGTERM (kill <pid>; only fires on Unix). Returning from
    // `run_server` lets the `ProcessLock` Drop impl unlink the PID file —
    // we deliberately avoid `std::process::exit` here to keep that cleanup.
    tokio::select! {
        result = service.waiting() => {
            result?;
        }
        _ = wait_for_sigint() => {
            info!("received SIGINT, shutting down");
        }
        _ = wait_for_sigterm() => {
            info!("received SIGTERM, shutting down");
        }
    }

    // Cancel the promoter task before this function returns, otherwise
    // dropping its JoinHandle would NOT stop the task — it would keep
    // polling against `args.db` for up to one `poll_interval` and could
    // race the shutdown by promoting after `run_server` is logically
    // done. `abort()` is non-blocking; the runtime drops the task on
    // its next yield point (the await in `tokio::time::sleep`).
    if let Some(h) = promoter_handle {
        h.abort();
    }

    // WatchHandle is dropped here, signaling the watcher thread to stop.
    info!("cartog MCP server stopped");
    Ok(())
}

/// Inputs for the Phase 5 promoter task. Bundled so the call site in
/// [`run_server`] stays readable; all fields are owned or `Arc`-cloned
/// before the task is spawned.
struct PromoterArgs {
    /// Live DB handle on the secondary. The promoter replaces its contents
    /// with a fresh RW [`Database`] when it takes ownership.
    db: Arc<Mutex<Database>>,
    /// Role flag visible to tool handlers. Flipped to `Primary` on
    /// successful promotion.
    role: Arc<AtomicRole>,
    /// Slot for the acquired [`ProcessLock`] once we win election.
    lock_cell: Arc<Mutex<Option<cartog_process_lock::ProcessLock>>>,
    /// Slot for the watcher handle spawned after promotion (when the user
    /// asked for `serve --watch`).
    watch_cell: Arc<Mutex<Option<WatchHandle>>>,
    /// Reflects whether a file watcher is currently running. Set to true
    /// on a successful post-promotion spawn, left false if the watcher
    /// failed to start (degraded Primary: surfaced in `cartog_stats`).
    watcher_active: Arc<std::sync::atomic::AtomicBool>,
    db_path: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    /// CWD captured at server startup. Reused for the post-promotion
    /// watcher so the watch root doesn't follow a later `std::env::set_current_dir`.
    cwd: std::path::PathBuf,
    /// Snapshot of the primary we attached behind. Promotion fires when
    /// this process is no longer running.
    primary: cartog_process_lock::ActiveLock,
    /// What we saw in `metadata` at attach time. Compared against the
    /// on-disk values at promotion time so we abort cleanly if the primary
    /// upgraded the schema or swapped the embedding stack under us.
    pinned: Option<PinnedAttach>,
    watch_requested: bool,
    rag: bool,
    rag_config: rag::EmbeddingProviderConfig,
    /// Polling interval. Const in production
    /// ([`DEFAULT_PROMOTER_POLL_INTERVAL`]); override in tests to keep
    /// the suite fast.
    poll_interval: std::time::Duration,
}

/// How often the promoter checks whether the primary is still alive. Kept
/// short enough that handoff feels responsive to a user closing the other
/// Claude Code window, long enough that the polling cost is invisible.
const DEFAULT_PROMOTER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Background task that runs in read-only mode and watches the primary's
/// liveness. On primary death, validates schema/fingerprint and attempts
/// promotion (atomic O_EXCL lock acquire → swap DB to RW → spawn watcher
/// if the user asked for one → flip role). Exits cleanly on schema drift
/// or when another reader wins the race.
async fn promoter_task(args: PromoterArgs) {
    loop {
        tokio::time::sleep(args.poll_interval).await;

        // Primary still alive? Liveness uses start_time when available
        // (closes the PID-reuse window).
        let primary_alive = match args.primary.start_time {
            Some(st) => cartog_process_lock::is_same_process(args.primary.pid, st),
            None => cartog_process_lock::is_alive(args.primary.pid),
        };
        if primary_alive {
            continue;
        }

        info!(
            primary_pid = args.primary.pid,
            "primary cartog process is gone; attempting promotion to primary"
        );

        // Cheap pre-check: skip the lock acquire if state already diverged.
        // We re-validate AFTER acquire too — the TOCTOU window between
        // here and acquire lets a third writer slip in.
        if let Err(e) = validate_pinned_state(&args.db_path, args.pinned.as_ref()) {
            info!(error = %e, "aborting promotion: on-disk state diverged before lock acquire");
            return;
        }

        // Atomic O_EXCL acquire. Other readers may race us; the loser stays
        // read-only and tries again on the next tick.
        let new_lock =
            match cartog_process_lock::ProcessLock::acquire(&args.state_dir, SERVE_LOCK_SLOT) {
                Ok(lock) => lock,
                Err(cartog_process_lock::AcquireError::Held(held)) => {
                    info!(
                        new_primary_pid = held.pid,
                        "another reader won the promotion race; staying read-only"
                    );
                    continue;
                }
                Err(cartog_process_lock::AcquireError::Io(e)) => {
                    tracing::warn!(error = %e, "promotion lock acquire failed; staying read-only");
                    continue;
                }
            };

        // Re-validate AFTER acquire: between the first validate and the
        // acquire, a third writer could have promoted itself, upgraded the
        // schema, and exited (releasing the lock to us). We now own the
        // lock, so the state can't change again — checking once here is
        // sufficient. On drift, drop the lock and exit cleanly so the
        // user restarts against the new schema.
        if let Err(e) = validate_pinned_state(&args.db_path, args.pinned.as_ref()) {
            info!(
                error = %e,
                "aborting promotion: on-disk state diverged after lock acquire"
            );
            drop(new_lock);
            return;
        }

        // Swap the DB connection to read-write. We hold the Mutex for the
        // entire swap, so no tool handler can be mid-query against the
        // about-to-close read-only connection. On open_existing_rw
        // failure (transient I/O, disk pressure), drop the lock and loop
        // — a subsequent poll may succeed. Only a poisoned mutex is
        // permanently fatal.
        let rw = match Database::open_existing_rw(&args.db_path) {
            Ok(rw) => rw,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "open_existing_rw failed during promotion; dropping lock and retrying"
                );
                drop(new_lock);
                continue;
            }
        };
        match args.db.lock() {
            Ok(mut guard) => {
                *guard = rw;
            }
            Err(_) => {
                tracing::error!("db mutex poisoned; cannot promote, exiting promoter task");
                drop(new_lock);
                return;
            }
        }

        // Install the lock so it lives until shutdown (Drop unlinks the
        // PID file). Flip role to Primary BEFORE spawning the watcher so
        // tool handlers that re-check `role.load()` immediately see the
        // new state — and the write tools start accepting requests at
        // the same moment the DB is RW (no window where role lags the
        // swap).
        //
        // A poisoned lock_cell mutex is fatal in the same way args.db
        // poison is (treated symmetrically above): if we let `new_lock`
        // fall off the end of an `if let Ok(_)` arm, Drop unlinks the
        // PID file, and then storing Role::Primary creates a "primary
        // with no lock" state — a fresh `cartog serve` would win the
        // next O_EXCL acquire and we'd have two Primaries. Instead, bail
        // out cleanly: drop the lock, leave role as ReadOnly, exit the
        // promoter task. The next-attempt path is now closed (caller
        // would need to restart the process), but a poisoned mutex
        // means the whole server is degraded anyway.
        match args.lock_cell.lock() {
            Ok(mut guard) => {
                *guard = Some(new_lock);
            }
            Err(_) => {
                tracing::error!(
                    "lock_cell mutex poisoned; cannot install serve lock, exiting promoter task without flipping role"
                );
                drop(new_lock);
                return;
            }
        }
        args.role.store(Role::Primary);

        if args.watch_requested {
            // Reuse the cwd captured at server startup, not
            // std::env::current_dir() — the latter follows runtime
            // chdir() calls (rare in MCP children but possible in tests
            // and embedded uses).
            let mut config = WatchConfig::new(args.cwd.clone());
            config.rag = args.rag;
            config.rag_config = args.rag_config.clone();
            config.pid_lock_dir = Some(args.state_dir.clone());
            // Skip migrations because we validated the schema when we
            // attached read-only — re-running them would re-trigger the
            // embedding-dimension reconcile the election prevents.
            //
            // We DO still acquire `watch.pid` (the watcher's own slot)
            // even though we already hold `serve.pid`. The two slots
            // serve different consumers: `serve.pid` blocks other MCP
            // servers, `watch.pid` blocks a separately-running
            // `cartog watch` from a terminal. Without the watch slot a
            // terminal `cartog watch` would happily start and create
            // two concurrent indexers writing to the same DB.
            config.skip_migrations = true;
            let db_path_str = args.db_path.to_string_lossy().into_owned();
            match watch::spawn_watch(config, &db_path_str) {
                Ok(handle) => {
                    // If watch_cell is poisoned, dropping `handle` here
                    // signals shutdown to the watcher thread (its
                    // shutdown flag flips in Drop). That's the best we
                    // can do; the server stays Primary with no watcher
                    // — degraded but not corrupt — and we leave
                    // watcher_active = false so `cartog_stats` surfaces
                    // the degradation.
                    match args.watch_cell.lock() {
                        Ok(mut guard) => {
                            *guard = Some(handle);
                            args.watcher_active
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        Err(_) => {
                            tracing::error!(
                                "watch_cell mutex poisoned; post-promotion watcher discarded — \
                                 server is Primary but will not auto-reindex"
                            );
                            drop(handle);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "post-promotion watcher failed to start");
                }
            }
        }

        info!("promoted to primary for {}", args.db_path.display());
        return;
    }
}

/// Re-read `schema_version` and the embedding fingerprint from disk and
/// compare to what the secondary saw at attach. Used by the promoter
/// before attempting to take over — if either changed, a third writer
/// already took over and upgraded under us.
fn validate_pinned_state(
    db_path: &std::path::Path,
    pinned: Option<&PinnedAttach>,
) -> anyhow::Result<()> {
    let pinned = match pinned {
        Some(p) => p,
        None => return Ok(()),
    };
    let reader = Database::open_readonly(db_path)
        .map_err(|e| anyhow::anyhow!("re-attach read-only failed: {e}"))?;
    let now = reader
        .pinned_attach()
        .ok_or_else(|| anyhow::anyhow!("internal: re-attached DB has no pinned state"))?;
    if now != pinned {
        anyhow::bail!(
            "DB metadata changed since attach: was {pinned:?}, now {now:?} (another writer took over)"
        );
    }
    Ok(())
}

/// Resolve to a future that completes when the process receives SIGTERM.
/// On Windows this also covers `CTRL_CLOSE_EVENT` (console window closed)
/// and `CTRL_SHUTDOWN_EVENT`. On platforms where the relevant signal source
/// can't be installed, the future never completes — `service.waiting()`
/// remains the shutdown signal.
///
/// `wait_for_sigint` wraps `tokio::signal::ctrl_c()` so a failure to
/// install the SIGINT handler does NOT immediately win the
/// `tokio::select!` branch with an `Err` resolved future — without the
/// wrapper, `_ = tokio::signal::ctrl_c()` would treat installation
/// failure as "SIGINT fired" and exit. Mirrors `wait_for_sigterm`.
async fn wait_for_sigint() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(error = %e, "failed to install SIGINT handler; falling back to other shutdown signals");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(unix)]
async fn wait_for_sigterm() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to install SIGTERM handler; falling back to stdin-EOF only");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(windows)]
async fn wait_for_sigterm() {
    use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
    let close = ctrl_close();
    let shutdown = ctrl_shutdown();
    match (close, shutdown) {
        (Ok(mut c), Ok(mut s)) => {
            tokio::select! {
                _ = c.recv() => {}
                _ = s.recv() => {}
            }
        }
        _ => {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn wait_for_sigterm() {
    std::future::pending::<()>().await;
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
        let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let opts = ServerOptions {
            pid_lock_dir: Some(dir.path().to_path_buf()),
        };
        let outcome = acquire_serve_lock(&opts).expect("acquire");
        let lock = match outcome {
            ServeLockOutcome::Primary(l) => l,
            other => panic!("expected Primary, got {other:?}"),
        };
        let path = dir.path().join(format!("{SERVE_LOCK_SLOT}.pid"));
        assert!(path.exists(), "PID file should exist while lock is held");
        // File is now two lines (pid + start_time); only the first line is the PID.
        let contents = std::fs::read_to_string(&path).unwrap();
        let pid: u32 = contents.lines().next().unwrap().trim().parse().unwrap();
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
        let outcome = acquire_serve_lock(&opts).expect("noop");
        assert!(
            matches!(outcome, ServeLockOutcome::Untracked),
            "no lock dir → Untracked"
        );
    }

    /// Serialize tests that read or mutate the `CARTOG_SINGLE_WRITER` env
    /// var. The variable is process-global; cargo test runs cases in
    /// parallel by default, so without this mutex a concurrent setter
    /// flips the value mid-read on another thread.
    fn env_mutex() -> &'static std::sync::Mutex<()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn second_acquire_for_same_dir_reports_held() {
        // Two acquire_serve_lock calls against the same dir: the first wins,
        // the second must surface Held(_) with the first's PID so the caller
        // can branch.
        let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let opts = ServerOptions {
            pid_lock_dir: Some(dir.path().to_path_buf()),
        };
        let _first = match acquire_serve_lock(&opts).expect("first acquire") {
            ServeLockOutcome::Primary(l) => l,
            other => panic!("expected Primary, got {other:?}"),
        };
        let second = acquire_serve_lock(&opts).expect("second acquire returns ok");
        match second {
            ServeLockOutcome::Held(held) => {
                assert_eq!(held.slot, SERVE_LOCK_SLOT);
                assert_eq!(held.pid, std::process::id());
            }
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn kill_switch_disables_election() {
        let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
        // CARTOG_SINGLE_WRITER=0 must let a second acquire_overwriting-style
        // call succeed despite a live first holder. Restoring the env var
        // afterwards is best-effort; tests in a single binary share env so
        // we set + unset around the call site.
        let dir = tempfile::TempDir::new().unwrap();
        let opts = ServerOptions {
            pid_lock_dir: Some(dir.path().to_path_buf()),
        };
        let _first = match acquire_serve_lock(&opts).expect("first acquire") {
            ServeLockOutcome::Primary(l) => l,
            other => panic!("expected Primary, got {other:?}"),
        };

        // SAFETY: tests in `cargo test` run in threads, but env mutation is
        // process-global. Other tests in this file don't depend on this var,
        // and we restore it before returning.
        let prev = std::env::var(SINGLE_WRITER_ENV).ok();
        // SAFETY: env vars are inherently process-wide and tests share them.
        // We restore the prior value before this test returns so adjacent
        // tests aren't affected.
        unsafe {
            std::env::set_var(SINGLE_WRITER_ENV, "0");
        }
        let result = acquire_serve_lock(&opts);
        // SAFETY: same reason — restoring prior state regardless of outcome.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(SINGLE_WRITER_ENV, v),
                None => std::env::remove_var(SINGLE_WRITER_ENV),
            }
        }
        match result.expect("kill switch acquire") {
            ServeLockOutcome::Primary(_) => {} // expected
            other => panic!("expected Primary with kill switch, got {other:?}"),
        }
    }

    // ── Role / read-only attach tests (Phase 4) ──

    fn test_rag_config() -> rag::EmbeddingProviderConfig {
        rag::EmbeddingProviderConfig::default()
    }

    #[test]
    fn primary_server_reports_primary_role() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let server =
            CartogServer::new(&db_path, test_rag_config()).expect("primary server constructs");
        assert_eq!(server.role(), Role::Primary);
    }

    #[test]
    fn read_only_server_reports_read_only_role() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        // First open writable to materialize the file with current schema.
        {
            let _primary =
                CartogServer::new(&db_path, test_rag_config()).expect("primary server constructs");
        }
        let reader = CartogServer::new_read_only(&db_path, test_rag_config())
            .expect("read-only server constructs");
        assert_eq!(reader.role(), Role::ReadOnly);
    }

    #[test]
    fn promoter_validate_pinned_state_matches_when_unchanged() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            // Materialize a DB so open_readonly can later read its state.
            let _primary = Database::open(&db_path, 384).unwrap();
        }
        let pinned = Database::open_readonly(&db_path)
            .unwrap()
            .pinned_attach()
            .cloned();
        validate_pinned_state(&db_path, pinned.as_ref()).expect("matching pin must validate");
    }

    #[test]
    fn promoter_validate_pinned_state_detects_schema_bump() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let pinned = {
            let db = Database::open(&db_path, 384).unwrap();
            drop(db);
            Database::open_readonly(&db_path)
                .unwrap()
                .pinned_attach()
                .cloned()
        };
        // Simulate another writer upgrading the schema underneath us.
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.set_metadata("schema_version", "9999").unwrap();
        }
        let result = validate_pinned_state(&db_path, pinned.as_ref());
        // open_readonly returns SchemaDrift; validate_pinned_state wraps as anyhow.
        assert!(result.is_err(), "schema bump under us must fail validation");
    }

    #[test]
    fn atomic_role_round_trip() {
        let r = AtomicRole::new(Role::ReadOnly);
        assert_eq!(r.load(), Role::ReadOnly);
        r.store(Role::Primary);
        assert_eq!(r.load(), Role::Primary);
        r.store(Role::ReadOnly);
        assert_eq!(r.load(), Role::ReadOnly);
    }

    #[test]
    fn read_only_server_refuses_write_tools() {
        // refuse_if_read_only is the helper gating cartog_index and
        // cartog_rag_index. Verify both call sites get an error in
        // ReadOnly mode and pass through silently in Primary mode.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let _primary =
                CartogServer::new(&db_path, test_rag_config()).expect("primary server constructs");
        }
        let reader = CartogServer::new_read_only(&db_path, test_rag_config())
            .expect("read-only server constructs");

        let err = reader
            .refuse_if_read_only("cartog_index")
            .expect("read-only must refuse");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("read-only") && msg.contains("cartog_index"),
            "error must name the gate and the tool, got: {msg}"
        );
        // cartog_index → suggests `cartog index` (graph), not `cartog rag index`.
        assert!(
            msg.contains("cartog index") && !msg.contains("cartog rag index"),
            "cartog_index refusal must suggest `cartog index`, got: {msg}"
        );
        // Drops the misleading "~5s" promise.
        assert!(
            !msg.contains("~5s"),
            "refusal must not promise an exact pickup latency, got: {msg}"
        );

        let err = reader
            .refuse_if_read_only("cartog_rag_index")
            .expect("read-only must refuse");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("read-only") && msg.contains("cartog_rag_index"),
            "error must name the gate and the tool, got: {msg}"
        );
        // cartog_rag_index → suggests `cartog rag index` (vectors).
        assert!(
            msg.contains("cartog rag index"),
            "cartog_rag_index refusal must suggest `cartog rag index`, got: {msg}"
        );

        let primary = CartogServer::new(&db_path, test_rag_config()).expect("primary reconstructs");
        assert!(
            primary.refuse_if_read_only("cartog_index").is_none(),
            "primary must NOT refuse"
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
        let r1 = index_with_optional_lsp(&db, &lsp_mgr, &fixtures, false, None, None).unwrap();
        assert!(
            r1.dirty_files > 0,
            "first index must report dirty files (got {})",
            r1.dirty_files
        );

        // Second call without changes must be a no-op AND must skip LSP.
        let r2 = index_with_optional_lsp(&db, &lsp_mgr, &fixtures, false, None, None).unwrap();
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

    // ── Promoter regression tests (review fix M-promoter) ──

    fn promoter_args_for_test(
        db: Arc<Mutex<Database>>,
        role: Arc<AtomicRole>,
        db_path: std::path::PathBuf,
        state_dir: std::path::PathBuf,
        primary: cartog_process_lock::ActiveLock,
        pinned: Option<PinnedAttach>,
    ) -> PromoterArgs {
        PromoterArgs {
            db,
            role,
            lock_cell: Arc::new(Mutex::new(None)),
            watch_cell: Arc::new(Mutex::new(None)),
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            db_path: db_path.clone(),
            state_dir,
            cwd: std::env::current_dir().unwrap(),
            primary,
            pinned,
            watch_requested: false,
            rag: false,
            rag_config: rag::EmbeddingProviderConfig::default(),
            // Very short for tests so the loop responds quickly.
            poll_interval: std::time::Duration::from_millis(20),
        }
    }

    #[tokio::test]
    async fn promoter_abort_cancels_the_polling_task() {
        // Regression for review fix M-promoter (d): dropping the JoinHandle
        // does NOT cancel a tokio task — only abort() does. Without the
        // abort in run_server's shutdown path, the promoter could keep
        // polling for up to one poll_interval after run_server returns
        // and even promote during that window. We assert that abort()
        // really terminates the task within a small bounded time.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let state_dir = dir.path().join("state");
        // Materialize a DB so open_readonly can attach.
        {
            let _ = Database::open(&db_path, 384).unwrap();
        }
        let db = Arc::new(Mutex::new(Database::open_readonly(&db_path).unwrap()));
        let role = Arc::new(AtomicRole::new(Role::ReadOnly));
        let pinned = db.lock().unwrap().pinned_attach().cloned();
        // Pretend the primary is our own process (so liveness reports
        // true and the promoter just keeps polling forever, never
        // promoting). This isolates the test to the abort behavior.
        let primary = cartog_process_lock::ActiveLock {
            slot: SERVE_LOCK_SLOT.to_string(),
            pid: std::process::id(),
            start_time: cartog_process_lock::process_start_time(std::process::id()),
        };
        let args = promoter_args_for_test(db, role, db_path, state_dir, primary, pinned);
        let handle = tokio::task::spawn(promoter_task(args));

        // Let it poll a few times.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(!handle.is_finished(), "promoter must keep polling");
        handle.abort();
        // Allow a brief moment for the runtime to cancel.
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(
            handle.is_finished(),
            "abort must terminate the promoter task"
        );
    }

    #[test]
    fn validate_pinned_state_returns_ok_when_pin_is_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let _ = Database::open(&db_path, 384).unwrap();
        }
        validate_pinned_state(&db_path, None).expect("no pin must validate trivially");
    }

    #[test]
    fn validate_pinned_state_detects_drift() {
        // Drift is exercised via the embedding fingerprint (not
        // schema_version) because `open_readonly` rejects schema_version
        // mismatch *before* this helper's comparison runs.
        //
        // A fresh `Database::open` writes only `embedding_dimension`
        // (no provider/model), so the read-only attach captures
        // `pinned.embedding = None`. After the test writes provider and
        // model, the next `open_readonly` inside `validate_pinned_state`
        // assembles `embedding = Some(...)`, and `None != Some(...)`
        // surfaces as drift.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let pinned = {
            let _ = Database::open(&db_path, 384).unwrap();
            Database::open_readonly(&db_path)
                .unwrap()
                .pinned_attach()
                .cloned()
        };
        // Another writer rewrites the embedding provider under us.
        {
            let mutator = Database::open(&db_path, 384).unwrap();
            mutator
                .set_metadata("embedding_provider", "ollama")
                .unwrap();
            mutator
                .set_metadata("embedding_model", "nomic-embed-text")
                .unwrap();
        }
        let err = validate_pinned_state(&db_path, pinned.as_ref())
            .expect_err("divergent disk state must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("DB metadata changed"),
            "error message should name the drift, got: {msg}"
        );
    }

    #[tokio::test]
    async fn promoter_aborts_when_state_diverges_after_acquire() {
        // Integration smoke. The post-acquire branch logic is covered
        // by `validate_pinned_state_detects_drift`; this test verifies
        // the promoter wires drift detection to a clean exit (role stays
        // ReadOnly, task finishes).
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        {
            let _ = Database::open(&db_path, 384).unwrap();
        }
        let reader_db = Database::open_readonly(&db_path).unwrap();
        let pinned = reader_db.pinned_attach().cloned();
        let db = Arc::new(Mutex::new(reader_db));
        let role = Arc::new(AtomicRole::new(Role::ReadOnly));
        // Pretend primary is dead (no such PID).
        let primary = cartog_process_lock::ActiveLock {
            slot: SERVE_LOCK_SLOT.to_string(),
            pid: 4_194_304,
            start_time: None,
        };
        // Mutate the DB metadata under the reader.
        {
            let mutator = Database::open(&db_path, 384).unwrap();
            mutator.set_metadata("schema_version", "9999").unwrap();
        }

        let args = promoter_args_for_test(
            Arc::clone(&db),
            Arc::clone(&role),
            db_path,
            state_dir,
            primary,
            pinned,
        );
        let handle = tokio::task::spawn(promoter_task(args));
        // Give the promoter one tick.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // The promoter should have noticed primary-gone, validated, seen
        // drift, and exited (return). Role must NOT be Primary.
        assert!(handle.is_finished(), "promoter must exit on drift");
        let _ = handle.await;
        assert_eq!(
            role.load(),
            Role::ReadOnly,
            "drifted DB must not flip role to Primary"
        );
    }

    #[tokio::test]
    async fn promoter_loops_on_transient_open_failure() {
        // Regression for review fix M-promoter (b): pre-fix, an
        // open_existing_rw failure caused the promoter to `return`,
        // disabling promotion forever even if the next poll would
        // succeed. The fix loops on transient failures.
        //
        // We can exercise the "open fails -> loop" path by deleting the
        // DB file entirely between the validate and the open_existing_rw
        // call. open_existing_rw will fail; the promoter should drop
        // the lock and try again on the next tick (where it'll fail
        // validation, since the DB is missing, and exit cleanly).
        //
        // The key contract is: we don't return on the first
        // open_existing_rw failure — we drop the lock and loop. We
        // assert that by checking the lock file does not persist after
        // a failed promotion attempt.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        {
            let _ = Database::open(&db_path, 384).unwrap();
        }
        let reader = Database::open_readonly(&db_path).unwrap();
        let pinned = reader.pinned_attach().cloned();
        let db = Arc::new(Mutex::new(reader));
        let role = Arc::new(AtomicRole::new(Role::ReadOnly));
        let primary = cartog_process_lock::ActiveLock {
            slot: SERVE_LOCK_SLOT.to_string(),
            pid: 4_194_304,
            start_time: None,
        };

        let args = promoter_args_for_test(
            Arc::clone(&db),
            Arc::clone(&role),
            db_path.clone(),
            state_dir.clone(),
            primary,
            pinned,
        );
        let handle = tokio::task::spawn(promoter_task(args));
        // Give the loop a moment to enter its first tick, then yank the DB.
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        std::fs::remove_file(&db_path).unwrap();
        // The promoter should now either: (a) loop on validate-failure
        // and never acquire, or (b) acquire then fail open and drop the
        // lock + loop. Either way the role stays ReadOnly and the lock
        // file is not left behind.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        handle.abort();
        let _ = handle.await;
        assert_eq!(
            role.load(),
            Role::ReadOnly,
            "promoter must not flip role under transient failure"
        );
        let lock_path = state_dir.join("serve.pid");
        assert!(
            !lock_path.exists(),
            "promoter must release the lock on failure (not strand serve.pid)"
        );
    }
}
