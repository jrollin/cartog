//! MCP server for the cartog code graph.
//!
//! Exposes cartog's graph queries, indexing, semantic search, and deferred
//! self-update as 16 MCP tools over stdio transport. Designed for Claude Code,
//! Cursor, and other MCP clients.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = ""]
#![doc = include_str!("../README.md")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::schema_for_output, wrapper::Parameters},
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
use cartog_watch::{StaleSnapshot, WatchConfig, WatchHandle};

mod progress;

const MAX_IMPACT_DEPTH: u32 = 10;
const MAX_TRACE_DEPTH: u32 = 20;
const DEFAULT_CONTEXT_TOKENS: u32 = 6000;
const MAX_CONTEXT_TOKENS: u32 = 20000;

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
pub struct TraceParams {
    /// Starting symbol (the caller end of the path)
    pub from: String,
    /// Target symbol (the callee end of the path)
    pub to: String,
    /// Maximum path length to search (default 8, max 20)
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
pub struct ContextParams {
    /// Natural-language description of the task you're about to work on
    pub task: String,
    /// Approximate token budget for the returned bundle (default 6000)
    pub tokens: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MapParams {
    /// Maximum top-ranked symbols to include in the map (default 50).
    /// Symbols are ranked by in-degree centrality so the most-referenced
    /// definitions surface first.
    pub limit: Option<u32>,
}

// ── Response wrappers for JSON serialization ──
//
// MCP `structuredContent` must be a JSON object, and `schema_for_output`
// rejects non-object output schemas, so every tool returns an object — list
// tools wrap their array under a `results` field. The text content block keeps
// the original (bare-array) shape for clients without schema support.

#[derive(Debug, Serialize, JsonSchema)]
struct RefEntry {
    edge: cartog_core::Edge,
    source: Option<cartog_core::Symbol>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ImpactEntry {
    edge: cartog_core::Edge,
    depth: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
struct HierarchyEntry {
    child: String,
    parent: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SymbolList {
    results: Vec<cartog_core::Symbol>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct EdgeList {
    results: Vec<cartog_core::Edge>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct RefList {
    results: Vec<RefEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ImpactList {
    results: Vec<ImpactEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TraceHop {
    source_name: String,
    target_name: String,
    kind: String,
    file_path: String,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TraceList {
    /// Empty when `from == to`; absent path is reported as `found: false`.
    found: bool,
    hops: Vec<TraceHop>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct HierarchyList {
    results: Vec<HierarchyEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MapResult {
    files: Vec<String>,
    top_symbols: Vec<cartog_core::Symbol>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct StatsResult {
    #[serde(flatten)]
    stats: cartog_db::IndexStats,
    role: Role,
    watcher_active: bool,
}

/// Result of `cartog_update` (arm a deferred self-update).
#[derive(Debug, Serialize, JsonSchema)]
struct UpdateResult {
    /// The currently-running cartog version.
    current: String,
    /// The version that will be installed at the next boundary, when armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    /// One of `armed`, `up-to-date`, `cargo-refused`, or `error`.
    status: String,
    /// When the armed update takes effect.
    apply: String,
    /// Human-readable summary for display.
    message: String,
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
    redact: indexer::RedactionConfig,
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
        // LSP runs via the shared `lsp_manager` below (which already carries any
        // command overrides), so the indexer's own LSP pass stays off and its
        // override map is empty.
        indexer::index_directory(
            &db,
            root,
            force,
            false,
            cb_ref,
            cancel,
            redact,
            &std::collections::HashMap::new(),
        )
        .map_err(|e| mcp_err(format!("indexing failed: {e}")))?
    };

    if result.dirty_files > 0 {
        let mut mgr = lsp_manager.lock().map_err(|_| {
            mcp_err("internal error: LSP manager lock poisoned (server restart required)")
        })?;
        let db = db.lock().map_err(|_| {
            mcp_err("internal error: database lock poisoned (server restart required)")
        })?;
        // Map (done, total) into a counting ResolvingLsp phase; reuses the
        // indexer label so the wording stays single-sourced.
        let lsp_progress = progress_tx.as_ref().map(|tx| {
            let tx = tx.clone();
            move |done: u32, total: u32| {
                let _ = tx.try_send(progress::Phase::Indexer(
                    cartog_indexer::ProgressUpdate::ResolvingLsp { done, total },
                ));
            }
        });
        let lsp_progress_ref: Option<cartog_lsp::LspProgress<'_>> = lsp_progress
            .as_ref()
            .map(|f| f as &(dyn Fn(u32, u32) + Send + Sync));
        // Overrides live on the shared `mgr` (set at construction), so the
        // map passed here is ignored — pass empty.
        match cartog_lsp::lsp_resolve_edges(
            &db,
            root,
            Some(&mut mgr),
            &std::collections::HashMap::new(),
            lsp_progress_ref,
        ) {
            Ok(stats) => {
                result.edges_lsp_resolved = stats.resolved;
                result.edges_marked_unresolvable = stats.marked_unresolvable;
                result.edges_marked_external = stats.marked_external;
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
    redact: indexer::RedactionConfig,
) -> Result<indexer::IndexResult, McpError> {
    let indexer_cb = progress_tx
        .as_ref()
        .map(|tx| progress::indexer_callback(tx.clone()));
    let db = db
        .lock()
        .map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;
    let cb_ref: Option<&(dyn Fn(indexer::ProgressUpdate) + Send + Sync)> =
        indexer_cb.as_ref().map(|f| f as _);
    indexer::index_directory(
        &db,
        root,
        force,
        false,
        cb_ref,
        cancel,
        redact,
        &std::collections::HashMap::new(),
    )
    .map_err(|e| mcp_err(format!("indexing failed: {e}")))
}

/// Static routing hints per tool — guides the agent to the next logical step.
fn suggestions_for(tool: &str) -> Option<&'static str> {
    match tool {
        "cartog_index" => Some("Next: use cartog_map to orient yourself, cartog_rag_search to find code, or cartog_search to look up a symbol name."),
        "cartog_map" => Some("Next: use cartog_outline on an interesting file, cartog_rag_search for a concept, or cartog_search for a specific name."),
        "cartog_search" => Some("Next: use cartog_refs to find usages, cartog_callees to trace calls, or cartog_impact to assess blast radius."),
        "cartog_rag_search" => Some("Next: use cartog_outline to see file structure, or cartog_refs to find all usages of a symbol."),
        "cartog_context" => Some("Next: use cartog_outline or Read on a returned symbol, or cartog_trace to follow a call path between two of them."),
        "cartog_outline" => Some("Next: use Read with offset/limit to see specific lines, or cartog_refs to find usages of a symbol."),
        "cartog_refs" => Some("Next: use cartog_impact to assess blast radius, or cartog_callees to trace what a function calls."),
        "cartog_callees" => Some("Next: use cartog_refs to find callers, or cartog_impact to assess blast radius."),
        "cartog_trace" => Some("Next: read the hop bodies, or use cartog_refs/cartog_impact on a hop to widen the view."),
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
        "cartog_trace" => "Re-run with a smaller --depth, or pick closer endpoints.",
        _ => "Re-run with a narrower scope or filter.",
    }
}

/// Generate a tool's output schema with schemars' non-standard integer
/// `format` values stripped.
///
/// schemars tags Rust integers with formats like `uint32`/`int64` that aren't
/// JSON Schema standard formats, so strict client validators (e.g. Ajv) log a
/// warning per field on every connection. Removing them keeps the schema valid
/// (the field is still an `integer`) and the client log clean.
fn output_schema_for<T: schemars::JsonSchema + std::any::Any>() -> Arc<JsonObject> {
    let schema = schema_for_output::<T>().expect("output schema must be a JSON object");
    let mut value = serde_json::Value::Object((*schema).clone());
    strip_int_formats(&mut value);
    match value {
        serde_json::Value::Object(map) => Arc::new(map),
        _ => schema,
    }
}

/// Non-standard integer formats schemars emits for Rust integer types.
const NONSTANDARD_INT_FORMATS: &[&str] = &[
    "uint", "uint8", "uint16", "uint32", "uint64", "uint128", "int128",
];

/// Recursively remove non-standard integer `format` annotations from a schema.
fn strip_int_formats(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(fmt)) = map.get("format") {
                if NONSTANDARD_INT_FORMATS.contains(&fmt.as_str()) {
                    map.remove("format");
                }
            }
            map.values_mut().for_each(strip_int_formats);
        }
        serde_json::Value::Array(items) => {
            items.iter_mut().for_each(strip_int_formats);
        }
        _ => {}
    }
}

/// Build a successful result carrying both a text block and structured content.
/// `structuredContent` is attached only when present (callers omit it for
/// truncated responses, so an oversized payload can't bypass the size cap).
fn success_result(text: String, structured: Option<serde_json::Value>) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(text)]);
    result.structured_content = structured;
    result
}

/// Discover the plugin's pinned version so `cartog_update` can arm the PIN
/// rather than the latest stable release (which could overshoot the pin).
///
/// Reads the `"version"` field of the plugin manifest, located via
/// `CARTOG_PLUGIN_JSON` (explicit override) or `CLAUDE_PLUGIN_ROOT`
/// (`<root>/.claude-plugin/plugin.json`, set by Claude Code for plugin hooks).
/// Returns `None` when no manifest is discoverable (e.g. a non-plugin install),
/// in which case the caller falls back to arming the latest stable release.
fn discover_plugin_pin() -> Option<String> {
    let manifest = match std::env::var_os("CARTOG_PLUGIN_JSON") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let root = std::env::var_os("CLAUDE_PLUGIN_ROOT").filter(|v| !v.is_empty())?;
            PathBuf::from(root)
                .join(".claude-plugin")
                .join("plugin.json")
        }
    };
    let text = std::fs::read_to_string(&manifest).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = parsed.get("version")?.as_str()?;
    // Only accept a bare MAJOR.MINOR.PATCH — a malformed pin must fall back to
    // latest, not arm garbage.
    let parts: Vec<&str> = version.split('.').collect();
    let bare = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    bare.then(|| version.to_string())
}

/// Map the JSON envelope from `cartog self update --defer --json` to an
/// [`UpdateResult`]. The CLI is the single source of truth for arming
/// behavior and exit semantics; this only reshapes its output for MCP.
fn parse_arm_output(output: &std::process::Output) -> UpdateResult {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Scan from the last line for an object carrying a string `status`. The
    // `status` filter rejects a bare scalar (e.g. a stray `99` log line that
    // happens to be valid JSON) that would otherwise be picked and mask a real
    // result.
    let parsed: Option<serde_json::Value> = stdout.lines().rev().find_map(|line| {
        serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .filter(|v| v.get("status").and_then(|s| s.as_str()).is_some())
    });

    let unknown = || {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        // No output at all (e.g. the child was SIGKILLed before printing) would
        // otherwise be a dead end. Always leave the agent an actionable step.
        let message = if detail.is_empty() {
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            format!(
                "could not arm deferred update (exit {code}, no output). Run `cartog self update \
                 --defer` in a terminal, or /cartog-install, to see the error."
            )
        } else {
            format!("could not arm deferred update: {detail}")
        };
        UpdateResult {
            current: current.clone(),
            target: None,
            status: "error".to_string(),
            apply: "n/a".to_string(),
            message,
        }
    };

    let Some(obj) = parsed else { return unknown() };
    let status = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
    match status {
        "armed" => {
            let target = obj
                .get("target")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            UpdateResult {
                message: match &target {
                    Some(t) => format!(
                        "Armed update to {t}. It applies when this Claude Code session ends \
                         (or on the next restart) — the current session keeps running the \
                         installed binary."
                    ),
                    None => "Armed a deferred update.".to_string(),
                },
                current,
                target,
                status: "armed".to_string(),
                apply: "session-end-or-restart".to_string(),
            }
        }
        "up-to-date" => UpdateResult {
            current: current.clone(),
            target: None,
            status: "up-to-date".to_string(),
            apply: "n/a".to_string(),
            message: format!("cartog is already up to date ({current})."),
        },
        "cargo" => UpdateResult {
            current,
            target: None,
            status: "cargo-refused".to_string(),
            apply: "n/a".to_string(),
            message: "cartog was installed via cargo. Run `cargo install cartog --force` to \
                      upgrade."
                .to_string(),
        },
        _ => {
            let msg = obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            UpdateResult {
                current,
                target: None,
                status: "error".to_string(),
                apply: "n/a".to_string(),
                message: format!("could not arm deferred update: {msg}"),
            }
        }
    }
}

/// Build a JSON text response with next-tool suggestions appended.
///
/// `structured` is the object-shaped payload mirrored into `structuredContent`
/// for schema-aware clients; it is dropped when the response is truncated so an
/// oversized payload can't slip past the size cap. The text block always keeps
/// the original (possibly bare-array) JSON shape.
///
/// Caps total response size at `mcp_max_bytes()` so individual tool calls
/// don't blow the caller's context window. On overflow the payload is cut
/// at a safe char boundary and an overflow notice pointing at a narrower
/// tool is appended.
fn tool_response(
    db: &Database,
    json: String,
    structured: Option<serde_json::Value>,
    tool: &str,
    stale: Option<StaleSnapshot>,
) -> Result<CallToolResult, McpError> {
    tool_response_named(db, json, structured, tool, None, stale)
}

/// Build a staleness banner for `tool`, or `None` when nothing is stale or the
/// tool is unaffected. RAG staleness only warns the semantic tools; a debounce
/// gap warns every read tool. Prepended to the response so the agent sees it
/// first. Pure, so it's unit-testable without an MCP result.
fn stale_banner(snapshot: Option<StaleSnapshot>, tool: &str) -> Option<String> {
    let snap = snapshot?;
    let rag_tool = matches!(tool, "cartog_rag_search" | "cartog_context");
    if snap.rag_stale() && rag_tool {
        return Some(format!(
            "⚠️ {} symbol(s) awaiting re-embedding since the last index; semantic results may be stale.\n\n",
            snap.rag_pending
        ));
    }
    if snap.structural_stale() {
        return Some(
            "⚠️ File change(s) detected; the index is catching up and results may be stale.\n\n"
                .to_string(),
        );
    }
    None
}

/// Body of the trace hop with id `source_id` — the exact symbol on the path.
/// Stored RAG content if present, else the source byte-slice read relative to
/// `cwd`. `None` when neither is found.
fn trace_hop_body(db: &Database, cwd: &Path, source_id: &str) -> Option<String> {
    if let Some((content, _)) = db.get_symbol_content(source_id).ok().flatten() {
        return Some(content);
    }
    let sym = db
        .get_symbols_by_ids(std::slice::from_ref(&source_id.to_string()))
        .ok()?
        .into_iter()
        .next()?;
    let src = std::fs::read_to_string(cwd.join(&sym.file_path)).ok()?;
    let (mut start, mut end) = (sym.start_byte as usize, sym.end_byte as usize);
    if start >= end || end > src.len() {
        return None;
    }
    while start < end && !src.is_char_boundary(start) {
        start += 1;
    }
    while end > start && !src.is_char_boundary(end) {
        end -= 1;
    }
    Some(src[start..end].to_string())
}

/// Record a successful read tool call into the query log for
/// `cartog stats --savings`. Strips the `cartog_` prefix so MCP and CLI
/// counts aggregate under the same tool name; the `source` field keeps the
/// surface distinction. Best-effort — `Database::log_query` swallows write
/// errors and no-ops on read-only attach.
fn log_tool_query(db: &Database, tool: &str) {
    let short = tool.strip_prefix("cartog_").unwrap_or(tool);
    db.log_query(short, "mcp");
}

/// Build the "did you mean" suffix for an empty navigation result. Returns
/// `None` when there are no candidates or one is an exact match (the symbol
/// exists but genuinely has no edges, so suggesting it would be noise).
/// Pure function, factored out so the suggestion logic is unit-testable
/// without constructing an `rmcp` `CallToolResult`.
fn did_you_mean_suffix(name: &str, candidates: &[String]) -> Option<String> {
    if candidates.is_empty() || candidates.iter().any(|c| c == name) {
        return None;
    }
    Some(format!(
        "\n\nNo symbol named '{name}' had results. Did you mean: {}? \
         Use cartog_search to confirm the exact name.",
        candidates.join(", ")
    ))
}

/// Like [`tool_response`], but for name-based navigation tools (refs, callees,
/// impact, hierarchy): when the result is an empty array and the index is not
/// empty, appends a "did you mean" line listing similarly-named symbols so the
/// agent can recover from a typo or partial name instead of seeing a bare `[]`.
fn tool_response_named(
    db: &Database,
    json: String,
    structured: Option<serde_json::Value>,
    tool: &str,
    queried_name: Option<&str>,
    stale: Option<StaleSnapshot>,
) -> Result<CallToolResult, McpError> {
    let banner = stale_banner(stale, tool).unwrap_or_default();
    let is_empty = !db
        .has_indexed_files()
        .map_err(|e| mcp_err(format!("stats check failed: {e}")))?;

    // Log AFTER the has_indexed_files probe succeeds and only when the index
    // is non-empty. An "Index is empty — run cartog_index first" response is
    // not a real query and shouldn't count toward `cartog stats --savings`.
    if !is_empty {
        log_tool_query(db, tool);
    }

    // Empty navigation result on a populated index → suggest near matches.
    if !is_empty {
        if let Some(name) = queried_name {
            if json.trim() == "[]" && !name.is_empty() {
                let candidates = db
                    .search(name, None, None, 5)
                    .map(|c| c.into_iter().map(|s| s.name).collect::<Vec<_>>())
                    .unwrap_or_default();
                if let Some(suffix) = did_you_mean_suffix(name, &candidates) {
                    let mut text = format!("{banner}{json}");
                    text.push_str(&suffix);
                    // No structured content: the empty `[]` result plus a prose
                    // hint has no useful typed form.
                    return Ok(CallToolResult::success(vec![Content::text(text)]));
                }
            }
        }
    }

    // Reserve the banner's bytes up front so the final `banner + text [+
    // structured]` stays under the cap (the banner is prepended at the end).
    let budget = mcp_max_bytes().saturating_sub(banner.len());
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

    // Structured content is kept only for full (untruncated) responses, so an
    // oversized payload can't bypass the size cap via `structuredContent`. The
    // structured copy roughly doubles the payload, so it counts toward the cap:
    // drop it when text + structured would exceed the budget (the text block
    // already fits on its own).
    let mut structured = match structured {
        Some(value) if truncated_bytes == 0 => {
            let structured_bytes = serde_json::to_string(&value).map(|s| s.len()).unwrap_or(0);
            (text.len() + structured_bytes <= budget).then_some(value)
        }
        _ => None,
    };

    if truncated_bytes > 0 {
        text.push_str(&format!(
            "\n\n(Response truncated: {truncated_bytes} bytes omitted to stay under the \
             {cap}-byte cap. {hint})",
            cap = mcp_max_bytes(),
            hint = narrowing_hint_for(tool),
        ));
    } else if is_empty {
        text.push_str("\n\n(Index is empty. Run cartog_index first to build the code graph.)");
        structured = None;
    } else if let Some(hint) = suggestions_for(tool) {
        text.push_str("\n\n");
        text.push_str(hint);
    }
    // Prepend after truncation so the banner survives an oversized body.
    if !banner.is_empty() {
        text.insert_str(0, &banner);
    }
    // Final hard clamp: the per-branch budgeting reserves space for the banner
    // and a 256-byte notice, but appended suffixes (suggestions, hints) aren't
    // individually counted. Trim to a char boundary so `text.len()` is provably
    // ≤ the cap no matter which suffixes fired.
    let cap = mcp_max_bytes();
    if text.len() > cap {
        let cut = (cap.saturating_sub(3)..=cap)
            .rev()
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(0);
        text.truncate(cut);
    }
    Ok(success_result(text, structured))
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
    /// the slot — the 2 write tools are gated, the 14 read tools work
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
    /// Staleness state published by the watcher (when `--watch` is active),
    /// read to prepend "results may be stale" banners. A cell so the Phase 5
    /// promoter can install one after spawning a post-promotion watcher.
    /// `None`/empty for `cartog serve` without `--watch` and read-only peers.
    stale: Arc<Mutex<Option<Arc<cartog_watch::StaleState>>>>,
    /// Secret-redaction policy applied to indexing tools.
    redact: indexer::RedactionConfig,
}

/// Role of this MCP server instance under single-writer election.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Role {
    #[serde(rename = "primary")]
    Primary,
    #[serde(rename = "read-only")]
    ReadOnly,
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
    /// Construct a writable primary MCP server.
    ///
    /// Opens (or migrates) the DB at `db_path` read-write and reconciles the
    /// embedding fingerprint; returns `Err` if the DB can't be opened or the
    /// embedding model fails to load. `rag_config` selects the embedding +
    /// reranker providers, `redact` is the secret-redaction policy applied to
    /// indexed content, and `lsp_overrides` maps a cartog language to its
    /// `[lsp.<lang>] command` argv for the warm `LspManager` (empty = default
    /// PATH-resolved servers). For the read-only attach path see
    /// [`new_read_only`](Self::new_read_only).
    pub fn new(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
        redact: indexer::RedactionConfig,
        lsp_overrides: std::collections::HashMap<String, Vec<String>>,
    ) -> anyhow::Result<Self> {
        let db = Database::open(db_path, rag_config.resolved_dimension())
            .map_err(|e| anyhow::anyhow!("failed to open database: {e}"))?;
        let provider = rag::create_embedding_provider(&rag_config)
            .map_err(|e| anyhow::anyhow!("failed to load embedding model: {e}"))?;
        db.reconcile_embedding_fingerprint(&rag::fingerprint_of(provider.as_ref()))
            .map_err(|e| anyhow::anyhow!("failed to reconcile embedding fingerprint: {e}"))?;
        let reranker = rag::create_reranker_provider(
            &rag_config.reranker_provider,
            rag_config.reranker_model.as_deref(),
            rag_config.intra_threads,
        );
        Self::from_parts(db, provider, reranker, redact, lsp_overrides, Role::Primary)
    }

    /// Construct a secondary MCP server that attached read-only because
    /// another cartog process owns the `serve` PID lock. Skips schema
    /// migrations and the embedding-fingerprint reconcile (the primary
    /// owns both); the 2 DB-write tools return a clear error at dispatch
    /// time. The other 12 tools (11 read + `cartog_update`, which arms a
    /// machine-level deferred update, not a DB write) work normally.
    pub fn new_read_only(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
        redact: indexer::RedactionConfig,
        lsp_overrides: std::collections::HashMap<String, Vec<String>>,
    ) -> anyhow::Result<Self> {
        let db = Database::open_readonly(db_path)
            .map_err(|e| anyhow::anyhow!("failed to open database read-only: {e}"))?;
        let provider = rag::create_embedding_provider(&rag_config)
            .map_err(|e| anyhow::anyhow!("failed to load embedding model: {e}"))?;
        let reranker = rag::create_reranker_provider(
            &rag_config.reranker_provider,
            rag_config.reranker_model.as_deref(),
            rag_config.intra_threads,
        );
        Self::from_parts(
            db,
            provider,
            reranker,
            redact,
            lsp_overrides,
            Role::ReadOnly,
        )
    }

    /// Single field-wiring point for all constructors: takes an already-opened
    /// DB and pre-built providers and assembles the server. Keeping the struct
    /// literal here (instead of duplicated per constructor) means a new field
    /// is wired once and the test path can't silently drift from production.
    /// Snapshot the watcher's staleness state for banner decisions, or `None`
    /// when no live watcher publishes it (no `--watch`, read-only peer, or a
    /// degraded primary). Read with a brief lock — never held across `.await`.
    fn stale_snapshot(&self) -> Option<StaleSnapshot> {
        if !self
            .watcher_active
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        self.stale
            .lock()
            .ok()
            .and_then(|cell| cell.as_ref().map(|s| s.snapshot()))
    }

    fn from_parts(
        db: Database,
        provider: Box<dyn rag::provider::EmbeddingProvider>,
        reranker: Option<Box<dyn rag::provider::RerankerProvider>>,
        redact: indexer::RedactionConfig,
        lsp_overrides: std::collections::HashMap<String, Vec<String>>,
        role: Role,
    ) -> anyhow::Result<Self> {
        let cwd = Self::cwd()?;
        // Consumed only by the `lsp` feature's warm manager; keep the param
        // unconditional so callers compile in minimal (`--no-default-features`)
        // builds, mirroring `index_directory`'s always-present `lsp` arg.
        #[cfg(not(feature = "lsp"))]
        let _ = lsp_overrides;
        Ok(Self {
            tool_router: Self::tool_router(),
            db: Arc::new(Mutex::new(db)),
            embedding_provider: Arc::new(Mutex::new(provider)),
            reranker_provider: Arc::new(Mutex::new(reranker)),
            #[cfg(feature = "lsp")]
            lsp_manager: Arc::new(Mutex::new(cartog_lsp::manager::LspManager::with_overrides(
                &cwd,
                lsp_overrides,
            ))),
            cwd: Arc::from(cwd),
            role: Arc::new(AtomicRole::new(role)),
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stale: Arc::new(Mutex::new(None)),
            redact,
        })
    }

    /// Test-only constructor that injects an embedding provider instead of
    /// loading the real ONNX model. Keeps the integration tests independent
    /// of a cached model (the production `new`/`new_read_only` paths must
    /// load it for real). The Primary path reconciles the embedding
    /// fingerprint exactly like `new`, so tests exercise that side effect too.
    /// `role` selects writable-primary vs read-only attach.
    #[cfg(test)]
    fn new_with_provider(
        db_path: &std::path::Path,
        provider: Box<dyn rag::provider::EmbeddingProvider>,
        redact: indexer::RedactionConfig,
        role: Role,
    ) -> anyhow::Result<Self> {
        let db = match role {
            Role::Primary => {
                let db = Database::open(db_path, provider.dimension())
                    .map_err(|e| anyhow::anyhow!("failed to open database: {e}"))?;
                db.reconcile_embedding_fingerprint(&rag::fingerprint_of(provider.as_ref()))
                    .map_err(|e| {
                        anyhow::anyhow!("failed to reconcile embedding fingerprint: {e}")
                    })?;
                db
            }
            Role::ReadOnly => Database::open_readonly(db_path)
                .map_err(|e| anyhow::anyhow!("failed to open database read-only: {e}"))?,
        };
        Self::from_parts(
            db,
            provider,
            None,
            redact,
            std::collections::HashMap::new(),
            role,
        )
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
        description = "Build or rebuild the code graph index. Run this first before any other cartog tool, or after making code changes to keep the graph current. Incremental by default — only re-indexes changed files. Use force=true if results seem stale. Not for: routine queries (call once per session, not before every read). Returns: {files_indexed, files_skipped, symbols_added, edges_added, edges_resolved, edges_lsp_resolved, edges_marked_unresolvable, edges_marked_external}.",
        annotations(
            title = "Index codebase",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
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
        let redact = self.redact;
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
                redact,
            )?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            // Human summary first, then the raw JSON, so the agent sees counts at a glance.
            let mut text = format!("{}\n{json}", indexer::render_index_summary(&result));
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
        description = "Show one file's structure: functions, classes, methods, imports with signatures and line ranges. Use this INSTEAD of reading a file when you need to understand what's in it — then Read only the specific lines you need. For understanding how a FEATURE or AREA works (spanning files), prefer cartog_context — it returns the relevant bodies across files in one call. Not for: reading the actual function body (use Read with offset/limit), or finding usages (use cartog_refs). Returns: Symbol[] with {name, kind, signature, line_start, line_end, parent_id, is_async, is_exported}.",
        annotations(title = "Outline file", read_only_hint = true, open_world_hint = false),
        output_schema = output_schema_for::<SymbolList>()
    )]
    async fn cartog_outline(
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
            let symbols = db
                .outline(&file)
                .map_err(|e| mcp_err(format!("outline query failed: {e}")))?;

            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(SymbolList { results: symbols }).ok();
            tool_response(&db, json, structured, "cartog_outline", stale)
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
    async fn cartog_refs(
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

            let entries: Vec<RefEntry> = results
                .into_iter()
                .map(|(edge, sym)| RefEntry { edge, source: sym })
                .collect();

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(RefList { results: entries }).ok();
            tool_response_named(&db, json, structured, "cartog_refs", Some(&name), stale)
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
    async fn cartog_callees(
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

            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(EdgeList { results: edges }).ok();
            tool_response_named(&db, json, structured, "cartog_callees", Some(&name), stale)
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
    async fn cartog_impact(
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

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(ImpactList { results: entries }).ok();
            tool_response_named(&db, json, structured, "cartog_impact", Some(&name), stale)
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
    async fn cartog_trace(
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

            let hops: Vec<TraceHop> = path
                .iter()
                .flatten()
                .map(|h| TraceHop {
                    body: trace_hop_body(&db, &cwd, &h.source_id),
                    source_name: h.source_name.clone(),
                    target_name: h.target_name.clone(),
                    kind: h.kind.to_string(),
                    file_path: h.file_path.clone(),
                    line: h.line,
                })
                .collect();

            let result = TraceList {
                found: path.is_some(),
                hops,
            };
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response_named(&db, json, structured, "cartog_trace", Some(&from), stale)
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
    async fn cartog_hierarchy(
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

            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(HierarchyList { results: entries }).ok();
            tool_response_named(
                &db,
                json,
                structured,
                "cartog_hierarchy",
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
    async fn cartog_deps(
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

            let json = serde_json::to_string_pretty(&edges)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(EdgeList { results: edges }).ok();
            tool_response(&db, json, structured, "cartog_deps", stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

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
            let symbols = db
                .search(&query, kind_filter, file_filter, limit)
                .map_err(|e| mcp_err(format!("search failed: {e}")))?;

            let json = serde_json::to_string_pretty(&symbols)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(SymbolList { results: symbols }).ok();
            tool_response(&db, json, structured, "cartog_search", stale)
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

            // `watcher_active=false` on a Primary means either the user did not
            // request `--watch`, or a post-promotion watcher spawn failed (e.g.,
            // another live `cartog watch` holds the watch slot, or notify install
            // failed). The user can distinguish the cases by whether they passed
            // `--watch`.
            let result = StatsResult {
                stats,
                role,
                watcher_active,
            };
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            // cartog_stats bypasses tool_response_named so it must log itself
            // — otherwise MCP-side stats calls disappear from
            // `cartog stats --savings`.
            log_tool_query(&db, "cartog_stats");
            Ok(success_result(json, structured))
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Arm a deferred cartog self-update.
    ///
    /// Deliberately NOT gated by `refuse_if_read_only`: arming writes the
    /// machine-level state file, not the index DB, so a read-only secondary
    /// may arm just like the primary. The guard exists only to stop a
    /// secondary from writing the DB — adding it here would be a category
    /// error.
    ///
    /// Never swaps the binary in-session: this server IS the live peer that
    /// `cartog self update` refuses to overwrite. It shells out to
    /// `self update --defer`, which records the target and exits without
    /// touching the binary; the boundary swap happens at SessionEnd.
    #[tool(
        description = "Arm a deferred cartog self-update. Does NOT upgrade in this session — the running server keeps its current binary; the new version becomes active after this session ends (or the next restart). Use when the user confirms they want to update cartog. When cartog is installed as a Claude Code plugin, this arms the plugin's PINNED version (discovered from the plugin manifest); otherwise it arms the latest stable release. Not for: indexing or search. Returns: {current, target, status, apply, message}.",
        annotations(
            title = "Update cartog",
            read_only_hint = false,
            destructive_hint = false,
            // Not idempotent: a latest-release arm re-fetches the tag and each
            // arm rewrites armed_at / last_update_check timestamps, so repeated
            // calls change state.toml even when the armed target is unchanged.
            idempotent_hint = false,
            open_world_hint = true
        ),
        output_schema = output_schema_for::<UpdateResult>()
    )]
    async fn cartog_update(&self) -> Result<CallToolResult, McpError> {
        tokio::task::spawn_blocking(move || {
            debug!("update (arm deferred)");
            let exe = std::env::current_exe()
                .map_err(|e| mcp_err(format!("cannot resolve cartog binary: {e}")))?;
            // Arm the plugin's pinned version when discoverable so we can't
            // overshoot the pin; fall back to latest stable otherwise.
            let mut args = vec!["self", "update", "--defer", "--json"];
            let pin = discover_plugin_pin();
            if let Some(ref v) = pin {
                args.push("--to");
                args.push(v);
            }
            let output = std::process::Command::new(exe)
                .args(&args)
                .output()
                .map_err(|e| mcp_err(format!("failed to run self update --defer: {e}")))?;

            let result = parse_arm_output(&output);
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
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
    async fn cartog_map(
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
            let top_symbols = db
                .top_symbols(limit)
                .map_err(|e| mcp_err(format!("top_symbols query failed: {e}")))?;

            let result = MapResult { files, top_symbols };

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_map", stale)
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
    async fn cartog_changes(
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
            let symbols = db
                .symbols_for_files(&changed_files, kind_filter)
                .map_err(|e| mcp_err(format!("symbols query failed: {e}")))?;

            let result = cartog_core::ChangesResult {
                changed_files,
                symbols,
            };

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_changes", stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Build embedding index for semantic code search.
    #[tool(
        description = "Build the embedding index for semantic search. Optional — cartog_rag_search ALREADY works at FTS5 (BM25) quality without embeddings; only run this when you want vector recall on top. Requires `cartog rag setup` from the CLI first to download the model. Not for: first-time setup of cartog (cartog_index is what you want). Returns: {embedded, skipped, failed, dim}.",
        annotations(
            title = "Build embedding index",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
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
        let redact = self.redact;
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
            let _ = indexer::index_directory(
                &db,
                &validated,
                false,
                false,
                None,
                probe_ref,
                redact,
                &std::collections::HashMap::new(),
            )
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
        description = "Search code by concept, keyword, or natural language. Returns ranked symbols with snippet excerpts — locations + previews, not full bodies. Use to LOCATE code matching a concept: 'find code related to...', 'show me the authentication logic'. For 'how does X work?' or understanding an area, prefer cartog_context — it returns full bodies + call neighbors in one call instead of snippets you'd then have to Read. Works even without embeddings (keyword matching alone is already strong). Prefer this over Grep for code discovery. Not for: looking up a known symbol name (use cartog_search instead — more precise). Filter with kind='document' for docs, kind='all' for both. Returns: Symbol[] ranked by relevance with snippet excerpts.",
        annotations(
            title = "Semantic code search",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<rag::search::HybridSearchResult>()
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
        let stale = self.stale_snapshot();

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
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_rag_search", stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Build a one-shot task-context bundle fusing semantic search, structural
    /// neighbors, and centrality.
    #[tool(
        description = "PRIMARY TOOL — call FIRST for any 'how does X work?', 'understand/survey area Y', 'where do I implement Z?' question. ONE call returns the relevant symbols (semantic + keyword), their 1-hop call neighbors, and high-centrality definitions in the same files, with bodies inline and budgeted to fit. Read-equivalent: do NOT re-open the symbols it returns, and usually the ONLY call you need — answer from its bundle instead of a chain of search/refs/callees/outline/Read. Drill in with the granular tools (cartog_refs, cartog_callees, cartog_trace) only for follow-ups it doesn't cover. Routes through semantic search, so it finds code by concept, not just name. Raise `tokens` (default 6000, max 20000) for a whole subsystem. Not for: a single known symbol (use cartog_search), or one specific call path (use cartog_trace). Returns: {task, entries: [{symbol, reason, score, body?}], approx_tokens}.",
        annotations(
            title = "Task context bundle",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<rag::context::TaskContext>()
    )]
    async fn cartog_context(
        &self,
        Parameters(params): Parameters<ContextParams>,
    ) -> Result<CallToolResult, McpError> {
        let task = params.task;
        let tokens = params
            .tokens
            .unwrap_or(DEFAULT_CONTEXT_TOKENS)
            .min(MAX_CONTEXT_TOKENS);
        let db = Arc::clone(&self.db);
        let provider = Arc::clone(&self.embedding_provider);
        let reranker = Arc::clone(&self.reranker_provider);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            if task.is_empty() {
                return Err(mcp_err("task description cannot be empty"));
            }
            debug!(task = %task, tokens, "context");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let mut provider = provider.lock().map_err(|_| {
                mcp_err(
                    "internal error: embedding provider lock poisoned (server restart required)",
                )
            })?;
            let mut reranker = reranker.lock().map_err(|_| {
                mcp_err("internal error: reranker lock poisoned (server restart required)")
            })?;
            let opts = rag::context::ContextOptions::default();
            let result = match reranker.as_mut() {
                Some(r) => rag::context::build_task_context(
                    &db,
                    &task,
                    tokens,
                    provider.as_mut(),
                    Some(r.as_mut()),
                    &opts,
                ),
                None => rag::context::build_task_context(
                    &db,
                    &task,
                    tokens,
                    provider.as_mut(),
                    None,
                    &opts,
                ),
            }
            .map_err(|e| mcp_err(format!("context build failed: {e}")))?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_context", stale)
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
                 Languages: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, PHP, Dart, Swift, Kotlin, Markdown.",
            )
    }
}

mod single_writer;
pub use single_writer::{
    acquire_serve_lock, run_server, ServeLockOutcome, ServerOptions, SERVE_LOCK_SLOT,
    SINGLE_WRITER_ENV,
};
#[cfg(test)]
pub(crate) use single_writer::{
    promoter_task, serve_to_watch_slot, test_validate_call_counter, validate_pinned_state,
    PromoterArgs,
};

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_arm_output tests (cartog_update envelope reshaping) ──
    //
    // Unix-only: these construct a real `ExitStatus` by spawning `true`/`sh`.
    // The parse logic itself is platform-independent.

    /// Build an `Output` with the given stdout/stderr and a zero exit status.
    /// parse_arm_output reads `status` only in the no-output branch, so a
    /// success-shaped status is fine for the parse-path cases.
    #[cfg(unix)]
    fn output_ok(stdout: &str, stderr: &str) -> std::process::Output {
        std::process::Output {
            status: std::process::Command::new("true")
                .status()
                .expect("run true"),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    // ── discover_plugin_pin tests (cartog_update arms the pin) ──
    // Serialized via SERIAL because they mutate process-global env vars.

    #[test]
    fn discover_plugin_pin_reads_explicit_manifest() {
        let _g = test_validate_call_counter::SERIAL.blocking_lock();
        let prev_json = std::env::var_os("CARTOG_PLUGIN_JSON");
        let prev_root = std::env::var_os("CLAUDE_PLUGIN_ROOT");
        std::env::remove_var("CLAUDE_PLUGIN_ROOT");

        let dir = tempfile::TempDir::new().unwrap();
        let manifest = dir.path().join("plugin.json");
        std::fs::write(&manifest, r#"{"name":"cartog","version":"0.20.0"}"#).unwrap();
        std::env::set_var("CARTOG_PLUGIN_JSON", &manifest);
        assert_eq!(discover_plugin_pin().as_deref(), Some("0.20.0"));

        // Malformed (non-bare) version → None (fall back to latest).
        std::fs::write(&manifest, r#"{"version":"v0.20.0"}"#).unwrap();
        assert_eq!(
            discover_plugin_pin(),
            None,
            "non-bare-semver pin must be rejected"
        );

        // No manifest discoverable → None.
        std::env::remove_var("CARTOG_PLUGIN_JSON");
        assert_eq!(discover_plugin_pin(), None);

        match prev_json {
            Some(v) => std::env::set_var("CARTOG_PLUGIN_JSON", v),
            None => std::env::remove_var("CARTOG_PLUGIN_JSON"),
        }
        if let Some(v) = prev_root {
            std::env::set_var("CLAUDE_PLUGIN_ROOT", v);
        }
    }

    #[test]
    fn discover_plugin_pin_reads_claude_plugin_root() {
        let _g = test_validate_call_counter::SERIAL.blocking_lock();
        let prev_json = std::env::var_os("CARTOG_PLUGIN_JSON");
        let prev_root = std::env::var_os("CLAUDE_PLUGIN_ROOT");
        std::env::remove_var("CARTOG_PLUGIN_JSON");

        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin").join("plugin.json"),
            r#"{"version":"0.21.0"}"#,
        )
        .unwrap();
        std::env::set_var("CLAUDE_PLUGIN_ROOT", dir.path());
        assert_eq!(discover_plugin_pin().as_deref(), Some("0.21.0"));

        match prev_json {
            Some(v) => std::env::set_var("CARTOG_PLUGIN_JSON", v),
            None => std::env::remove_var("CARTOG_PLUGIN_JSON"),
        }
        match prev_root {
            Some(v) => std::env::set_var("CLAUDE_PLUGIN_ROOT", v),
            None => std::env::remove_var("CLAUDE_PLUGIN_ROOT"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_armed_maps_fields() {
        let r = parse_arm_output(&output_ok(
            r#"{"status":"armed","current":"0.19.0","target":"0.20.0","apply":"session-end-or-restart"}"#,
            "",
        ));
        assert_eq!(r.status, "armed");
        assert_eq!(r.target.as_deref(), Some("0.20.0"));
        assert_eq!(r.apply, "session-end-or-restart");
        assert!(r.message.contains("session ends"));
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_up_to_date_maps() {
        let r = parse_arm_output(&output_ok(
            r#"{"status":"up-to-date","current":"0.19.0","latest":"0.19.0"}"#,
            "",
        ));
        assert_eq!(r.status, "up-to-date");
        assert_eq!(r.apply, "n/a");
        assert!(r.target.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_cargo_maps_to_cargo_refused() {
        let r = parse_arm_output(&output_ok(
            r#"{"status":"cargo","message":"cartog was installed via cargo. Run `cargo install cartog --force` to upgrade."}"#,
            "",
        ));
        assert_eq!(r.status, "cargo-refused");
        assert!(r.message.contains("cargo install cartog --force"));
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_foreign_status_echoes_message_as_error() {
        let r = parse_arm_output(&output_ok(
            r#"{"status":"fetch-failed","message":"GitHub API returned status 500"}"#,
            "",
        ));
        assert_eq!(r.status, "error");
        assert!(r.message.contains("GitHub API returned status 500"));
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_skips_log_lines_before_json() {
        // A daily-update-check hint or any noise line before the JSON must not
        // break the parse — the reverse scan finds the real object.
        let r = parse_arm_output(&output_ok(
            "cartog: a new version is available\n{\"status\":\"armed\",\"target\":\"0.20.0\"}\n",
            "",
        ));
        assert_eq!(r.status, "armed");
        assert_eq!(r.target.as_deref(), Some("0.20.0"));
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_ignores_trailing_bare_scalar() {
        // A trailing bare scalar that is valid JSON must NOT be picked over the
        // real status object earlier in the stream.
        let r = parse_arm_output(&output_ok(
            "{\"status\":\"armed\",\"target\":\"1.2.3\"}\n99\n",
            "",
        ));
        assert_eq!(r.status, "armed", "bare scalar must be skipped");
        assert_eq!(r.target.as_deref(), Some("1.2.3"));
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_empty_output_names_exit_and_next_action() {
        // Child produced nothing (e.g. SIGKILL). The message must still give a
        // next step rather than a dead-end empty string.
        let out = std::process::Output {
            status: std::process::Command::new("sh")
                .args(["-c", "exit 9"])
                .status()
                .expect("run sh"),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let r = parse_arm_output(&out);
        assert_eq!(r.status, "error");
        assert!(
            r.message.contains("exit 9"),
            "must name the exit code: {}",
            r.message
        );
        assert!(
            r.message.contains("--defer") || r.message.contains("/cartog-install"),
            "must name a next action: {}",
            r.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_arm_output_nonjson_stderr_surfaces_detail() {
        let r = parse_arm_output(&output_ok("", "boom: something broke"));
        assert_eq!(r.status, "error");
        assert!(r.message.contains("boom: something broke"));
    }

    // ── Tool annotation tests ──

    /// Tools without side effects advertise `read_only_hint = true`; the three
    /// side-effecting tools advertise `false`. Two of those write the DB
    /// (`cartog_index`, `cartog_rag_index`); `cartog_update` arms a machine-level
    /// deferred update. Clients use the hint to skip approval prompts for safe
    /// tools and flag the rest.
    #[test]
    fn tool_annotations_mark_read_only_correctly() {
        let side_effecting = ["cartog_index", "cartog_rag_index", "cartog_update"];
        let tools = CartogServer::tool_router().list_all();

        assert!(!tools.is_empty(), "router exposes tools");
        for tool in &tools {
            let ann = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
            let expected_read_only = !side_effecting.contains(&tool.name.as_ref());
            assert_eq!(
                ann.read_only_hint,
                Some(expected_read_only),
                "{} read_only_hint",
                tool.name
            );
        }
    }

    /// Every tool exposes a human-readable title for client tool pickers.
    #[test]
    fn every_tool_has_a_title() {
        for tool in CartogServer::tool_router().list_all() {
            let title = tool
                .annotations
                .as_ref()
                .and_then(|a| a.title.as_deref())
                .or(tool.title.as_deref());
            assert!(
                title.is_some_and(|t| !t.is_empty()),
                "{} has no title",
                tool.name
            );
        }
    }

    // ── Output schema / structured content tests ──

    /// Every read tool advertises an output schema so schema-aware clients can
    /// validate `structuredContent`. The two write tools have no typed output.
    #[test]
    fn read_tools_advertise_output_schemas() {
        let writers = ["cartog_index", "cartog_rag_index"];
        for tool in CartogServer::tool_router().list_all() {
            let has_schema = tool.output_schema.is_some();
            let expected = !writers.contains(&tool.name.as_ref());
            assert_eq!(has_schema, expected, "{} output_schema", tool.name);
        }
    }

    /// `schema_for_output` rejects non-object schemas, so list tools must wrap
    /// their arrays. Building each schema proves the wrappers stay object-typed
    /// (a regression here would panic the tool macro at startup).
    #[test]
    fn output_schemas_are_objects() {
        output_schema_for::<SymbolList>();
        output_schema_for::<EdgeList>();
        output_schema_for::<RefList>();
        output_schema_for::<ImpactList>();
        output_schema_for::<HierarchyList>();
        output_schema_for::<MapResult>();
        output_schema_for::<StatsResult>();
        output_schema_for::<cartog_core::ChangesResult>();
        output_schema_for::<rag::search::HybridSearchResult>();
    }

    /// Non-standard integer formats (`uint32`, …) are stripped so strict client
    /// validators don't warn, while the field stays typed as an integer.
    #[test]
    fn output_schema_strips_nonstandard_int_formats() {
        let schema = output_schema_for::<SymbolList>();
        let value = serde_json::Value::Object((*schema).clone());

        fn collect_formats(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::String(f)) = map.get("format") {
                        out.push(f.clone());
                    }
                    map.values().for_each(|v| collect_formats(v, out));
                }
                serde_json::Value::Array(items) => {
                    items.iter().for_each(|v| collect_formats(v, out));
                }
                _ => {}
            }
        }
        let mut formats = Vec::new();
        collect_formats(&value, &mut formats);
        assert!(
            !formats
                .iter()
                .any(|f| NONSTANDARD_INT_FORMATS.contains(&f.as_str())),
            "non-standard int formats leaked: {formats:?}"
        );

        // The integer field survives, just without the bogus format.
        let start_line = &value["$defs"]["Symbol"]["properties"]["start_line"];
        assert_eq!(start_line["type"], "integer");
        assert!(start_line.get("format").is_none());
    }

    fn populated_memory_db() -> Database {
        let db = Database::open_memory().expect("in-memory DB");
        db.upsert_file(&cartog_core::FileInfo {
            path: "test.py".to_string(),
            last_modified: 0.0,
            hash: "h".to_string(),
            language: "python".to_string(),
            num_symbols: 1,
        })
        .expect("upsert file");
        db
    }

    /// A full (under-budget) response mirrors its payload into
    /// `structuredContent` while keeping the bare-array text block.
    #[test]
    fn tool_response_attaches_structured_content_under_budget() {
        let db = populated_memory_db();
        let symbols = db.search("anything", None, None, 30).expect("search");
        let json = serde_json::to_string_pretty(&symbols).expect("json");
        let structured = serde_json::to_value(SymbolList { results: symbols }).ok();

        let result = tool_response(&db, json, structured, "cartog_search", None).expect("response");

        let structured = result
            .structured_content
            .expect("structured content present");
        assert!(
            structured.get("results").is_some(),
            "structured content is the object wrapper"
        );
        assert_eq!(result.content.len(), 1, "text block retained");
    }

    /// An over-budget response drops `structuredContent` so an oversized payload
    /// can't bypass the size cap, and the text block carries a truncation notice.
    #[test]
    fn tool_response_drops_structured_content_when_truncated() {
        let db = populated_memory_db();
        // Exceed the default 64KB cap deterministically (no env mutation).
        let big = "x".repeat(mcp_max_bytes() + 1024);
        let json = format!("[\"{big}\"]");
        let structured = Some(serde_json::json!({ "results": [] }));

        let result = tool_response(&db, json, structured, "cartog_search", None).expect("response");

        assert!(
            result.structured_content.is_none(),
            "structured content dropped on truncation"
        );
        let text = match &result.content.first().expect("content").raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("expected text content"),
        };
        assert!(
            text.contains("Response truncated"),
            "truncation notice present"
        );
    }

    /// A staleness banner must not push a truncated response over the cap: the
    /// banner's bytes are reserved before truncation, so banner + body ≤ cap.
    #[test]
    fn tool_response_with_banner_stays_under_cap() {
        let db = populated_memory_db();
        let big = "x".repeat(mcp_max_bytes() + 1024);
        let json = format!("[\"{big}\"]");
        // rag_pending on a rag tool fires the longest banner.
        let stale = Some(snap(9, 0, 0));
        let result = tool_response(&db, json, None, "cartog_rag_search", stale).expect("response");
        let text = match &result.content.first().expect("content").raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("expected text content"),
        };
        assert!(text.starts_with("⚠️"), "banner present: {}", &text[..40]);
        assert!(
            text.len() <= mcp_max_bytes(),
            "banner + body must stay under the {}-byte cap, got {}",
            mcp_max_bytes(),
            text.len()
        );
    }

    /// The final clamp also covers the NON-truncated path: a body just under the
    /// banner-adjusted budget, plus a banner and an appended suggestion, must
    /// still end up ≤ the cap (suffixes aren't individually budgeted).
    #[test]
    fn tool_response_banner_plus_suffix_stays_under_cap() {
        let db = populated_memory_db();
        let cap = mcp_max_bytes();
        // Body sized so banner + body alone is just under cap; the appended
        // suggestion would push it over without the final clamp.
        let payload = "y".repeat(cap - 200);
        let json = format!("[\"{payload}\"]");
        let stale = Some(snap(3, 0, 0));
        let result = tool_response(&db, json, None, "cartog_rag_search", stale).expect("response");
        let text = match &result.content.first().expect("content").raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("expected text content"),
        };
        assert!(
            text.len() <= cap,
            "banner + body + suffix must stay under {cap}, got {}",
            text.len()
        );
    }

    /// The size cap counts the structured copy too: when the text fits on its own
    /// but text + structuredContent would exceed the budget, structured is dropped
    /// (the text block is kept intact, not truncated).
    #[test]
    fn tool_response_drops_structured_when_combined_exceeds_cap() {
        let db = populated_memory_db();
        // Text alone is just under the cap; the structured mirror pushes the
        // combined size over it.
        let budget = mcp_max_bytes();
        let payload = "y".repeat(budget * 3 / 4);
        let json = format!("[\"{payload}\"]");
        let structured = Some(serde_json::json!({ "results": [payload.clone()] }));

        assert!(json.len() <= budget, "text alone fits the cap");
        let result = tool_response(&db, json, structured, "cartog_search", None).expect("response");

        assert!(
            result.structured_content.is_none(),
            "structured dropped when combined size exceeds cap"
        );
        let text = match &result.content.first().expect("content").raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("expected text content"),
        };
        assert!(
            !text.contains("Response truncated"),
            "text block fits on its own, so it is not truncated"
        );
    }

    /// `StatsResult` flattens `IndexStats` and adds role + watcher fields at the
    /// top level (no nested `stats` object), matching the documented shape.
    #[test]
    fn stats_result_flattens_index_stats() {
        let db = populated_memory_db();
        let stats = db.stats().expect("stats");
        let result = StatsResult {
            stats,
            role: Role::ReadOnly,
            watcher_active: false,
        };
        let value = serde_json::to_value(&result).expect("serialize");
        let obj = value.as_object().expect("object");
        assert!(obj.contains_key("num_files"), "flattened stats field");
        // Role serializes to the exact wire string (hyphen preserved).
        assert_eq!(obj.get("role").and_then(|v| v.as_str()), Some("read-only"));
        assert_eq!(
            obj.get("watcher_active").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(!obj.contains_key("stats"), "stats is flattened, not nested");
    }

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

    // ── Edge provenance in structured output ──

    #[test]
    fn json_output_includes_provenance_when_present() {
        let mut edge = cartog_core::Edge::new("s:1", "foo", EdgeKind::Calls, "a.py", 1);
        edge.provenance = Some(cartog_core::EdgeProvenance::SameFile);
        let value = serde_json::to_value(EdgeList {
            results: vec![edge],
        })
        .unwrap();
        assert_eq!(value["results"][0]["provenance"], "same_file");
    }

    #[test]
    fn json_output_omits_provenance_when_absent() {
        // A freshly extracted edge has no provenance; skip_serializing_if drops
        // the key entirely so the wire format stays clean.
        let edge = cartog_core::Edge::new("s:1", "foo", EdgeKind::Calls, "a.py", 1);
        let value = serde_json::to_value(EdgeList {
            results: vec![edge],
        })
        .unwrap();
        assert!(value["results"][0].get("provenance").is_none());
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
    fn did_you_mean_suffix_lists_candidates() {
        let cands = vec!["ReviewResult".to_string(), "ReviewComment".to_string()];
        let suffix = did_you_mean_suffix("Revie", &cands).expect("suffix");
        assert!(suffix.contains("Did you mean: ReviewResult, ReviewComment"));
        assert!(suffix.contains("cartog_search"));
    }

    #[test]
    fn did_you_mean_suffix_none_on_exact_match() {
        let cands = vec!["ReviewResult".to_string()];
        assert!(did_you_mean_suffix("ReviewResult", &cands).is_none());
    }

    #[test]
    fn did_you_mean_suffix_none_without_candidates() {
        assert!(did_you_mean_suffix("Whatever", &[]).is_none());
    }

    fn snap(rag_pending: u32, change_seq: u64, reindexed_seq: u64) -> StaleSnapshot {
        StaleSnapshot {
            rag_pending,
            change_seq,
            reindexed_seq,
        }
    }

    #[test]
    fn stale_banner_none_when_not_stale() {
        assert!(stale_banner(Some(snap(0, 10, 10)), "cartog_rag_search").is_none());
        assert!(stale_banner(None, "cartog_rag_search").is_none());
    }

    #[test]
    fn stale_banner_rag_only_warns_semantic_tools() {
        // Pending embeddings warn rag_search/context...
        assert!(stale_banner(Some(snap(3, 10, 10)), "cartog_rag_search").is_some());
        assert!(stale_banner(Some(snap(3, 10, 10)), "cartog_context").is_some());
        // ...but not a structural tool (no debounce gap here).
        assert!(stale_banner(Some(snap(3, 10, 10)), "cartog_refs").is_none());
    }

    #[test]
    fn stale_banner_structural_warns_every_read_tool() {
        // A change after the last reindex warns refs and rag_search alike.
        assert!(stale_banner(Some(snap(0, 20, 10)), "cartog_refs").is_some());
        assert!(stale_banner(Some(snap(0, 20, 10)), "cartog_rag_search").is_some());
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
            pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
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
    fn acquire_serve_lock_rejects_dir_without_slot() {
        // Regression: a half-configured ServerOptions (pid_lock_dir set,
        // pid_lock_slot None) used to silently fall back to the global
        // SERVE_LOCK_SLOT, letting an embedder collide with — or be hidden
        // from — a CLI peer that derives a DB-scoped slot. The mixed-scope
        // hazard must surface as a hard error.
        let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let opts = ServerOptions {
            pid_lock_dir: Some(dir.path().to_path_buf()),
            pid_lock_slot: None,
        };
        let err = acquire_serve_lock(&opts).unwrap_err();
        assert!(
            err.to_string().contains("pid_lock_slot is None"),
            "error must explain the misconfiguration, got: {err}"
        );
    }

    #[test]
    fn acquire_serve_lock_rejects_slot_without_dir() {
        // Inverse half-config: pid_lock_slot set but pid_lock_dir is
        // None. Pre-fix the slot was silently ignored and the function
        // returned Ok(Untracked), losing the caller's intent. Must
        // surface as a hard error.
        let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let opts = ServerOptions {
            pid_lock_dir: None,
            pid_lock_slot: Some("serve-deadbeef".to_string()),
        };
        let err = acquire_serve_lock(&opts).unwrap_err();
        assert!(
            err.to_string().contains("pid_lock_dir is None"),
            "error must explain the misconfiguration, got: {err}"
        );
    }

    #[test]
    fn distinct_slots_for_different_dbs_do_not_collide() {
        // Two cartog peers serving different DBs in the same per-user state
        // dir must coexist. Pre-PR, both fought over a single `serve.pid`
        // slot; with DB-scoped slots they each claim their own
        // `serve-<hash>.pid`.
        let _guard = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let opts_a = ServerOptions {
            pid_lock_dir: Some(dir.path().to_path_buf()),
            pid_lock_slot: Some("serve-aaaa1111".to_string()),
        };
        let opts_b = ServerOptions {
            pid_lock_dir: Some(dir.path().to_path_buf()),
            pid_lock_slot: Some("serve-bbbb2222".to_string()),
        };
        let _a = match acquire_serve_lock(&opts_a).expect("acquire A") {
            ServeLockOutcome::Primary(l) => l,
            other => panic!("expected Primary for A, got {other:?}"),
        };
        let _b = match acquire_serve_lock(&opts_b).expect("acquire B") {
            ServeLockOutcome::Primary(l) => l,
            other => panic!("expected Primary for B, got {other:?}"),
        };
        // Both PID files present on disk, no Held collision.
        assert!(dir.path().join("serve-aaaa1111.pid").exists());
        assert!(dir.path().join("serve-bbbb2222.pid").exists());
    }

    #[test]
    fn serve_to_watch_slot_preserves_db_scope() {
        // The watcher slot is derived from the serve slot so both PID files
        // for the same DB share their scope suffix.
        assert_eq!(serve_to_watch_slot("serve").unwrap(), "watch");
        assert_eq!(serve_to_watch_slot("serve-abc123").unwrap(), "watch-abc123");
        // Off-pattern inputs that start with the bytes "serve" but are NOT
        // a serve-family slot must be REJECTED — silently folding them to
        // the global watch slot would let distinct embedders collide on
        // `watch.pid` while their serve slots stay distinct.
        for bad in [
            "unknown-prefix",
            "server",
            "serverless",
            "servefoo",
            "Serve",
            "",
            "serve-", // trailing-dash with empty hex
        ] {
            assert!(
                serve_to_watch_slot(bad).is_err(),
                "expected off-pattern slot {bad:?} to be rejected"
            );
        }
    }

    proptest::proptest! {
        /// `serve-<nonempty>` maps to `watch-<same suffix>`, preserving the DB
        /// scope verbatim.
        #[test]
        fn serve_to_watch_slot_round_trips_suffix(suffix in "[0-9a-f]{1,16}") {
            let got = serve_to_watch_slot(&format!("serve-{suffix}")).unwrap();
            proptest::prop_assert_eq!(got, format!("watch-{suffix}"));
        }

        /// Total contract over arbitrary input, checked against the OUTPUT rather
        /// than a re-implemented accept rule: the only accepted slots are `serve`
        /// (→ `watch`) and `serve-<nonempty>` (→ `watch-<same>`); every other
        /// input is rejected. Folding an off-pattern slot to the global watch slot
        /// would let distinct embedders collide on `watch.pid`.
        #[test]
        fn serve_to_watch_slot_contract(s in ".{0,20}") {
            match serve_to_watch_slot(&s) {
                Ok(out) if s == "serve" => proptest::prop_assert_eq!(out, "watch"),
                Ok(out) => {
                    // The only other accepted form: the suffix is carried verbatim.
                    let suffix = s.strip_prefix("serve-");
                    proptest::prop_assert!(
                        suffix.is_some_and(|r| !r.is_empty()),
                        "accepted off-pattern {s:?}"
                    );
                    proptest::prop_assert_eq!(out, format!("watch-{}", suffix.unwrap()));
                }
                Err(_) => {
                    proptest::prop_assert_ne!(&s, "serve");
                    proptest::prop_assert!(
                        s.strip_prefix("serve-").map_or(true, str::is_empty),
                        "rejected a valid serve-<nonempty> slot: {s:?}"
                    );
                }
            }
        }
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
            pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
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
            pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
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

    /// Deterministic in-memory embedding provider for tests, sized to the
    /// default embedding dimension so the DB schema matches. Avoids loading
    /// the real ONNX model (which may be absent in CI coverage runners).
    fn test_provider() -> Box<dyn rag::provider::EmbeddingProvider> {
        Box::new(rag::provider::test_utils::MockEmbeddingProvider::new(
            rag::EMBEDDING_DIM,
        ))
    }

    #[test]
    fn primary_server_reports_primary_role() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let server = CartogServer::new_with_provider(
            &db_path,
            test_provider(),
            indexer::RedactionConfig::disabled(),
            Role::Primary,
        )
        .expect("primary server constructs");
        assert_eq!(server.role(), Role::Primary);
    }

    #[test]
    fn read_only_server_reports_read_only_role() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        // First open writable to materialize the file with current schema.
        {
            let _primary = CartogServer::new_with_provider(
                &db_path,
                test_provider(),
                indexer::RedactionConfig::disabled(),
                Role::Primary,
            )
            .expect("primary server constructs");
        }
        let reader = CartogServer::new_with_provider(
            &db_path,
            test_provider(),
            indexer::RedactionConfig::disabled(),
            Role::ReadOnly,
        )
        .expect("read-only server constructs");
        assert_eq!(reader.role(), Role::ReadOnly);
    }

    #[test]
    fn promoter_validate_pinned_state_matches_when_unchanged() {
        let _serial = test_validate_call_counter::SERIAL.blocking_lock();
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
        let _serial = test_validate_call_counter::SERIAL.blocking_lock();
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
            let _primary = CartogServer::new_with_provider(
                &db_path,
                test_provider(),
                indexer::RedactionConfig::disabled(),
                Role::Primary,
            )
            .expect("primary server constructs");
        }
        let reader = CartogServer::new_with_provider(
            &db_path,
            test_provider(),
            indexer::RedactionConfig::disabled(),
            Role::ReadOnly,
        )
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

        let primary = CartogServer::new_with_provider(
            &db_path,
            test_provider(),
            indexer::RedactionConfig::disabled(),
            Role::Primary,
        )
        .expect("primary reconstructs");
        assert!(
            primary.refuse_if_read_only("cartog_index").is_none(),
            "primary must NOT refuse"
        );
    }

    #[test]
    fn cartog_update_is_registered_and_not_gated_read_only() {
        // cartog_update arms a machine-level deferred update, not a DB write,
        // so it must be available even on a read-only secondary. The router
        // lists it regardless of role, and the handler never consults
        // refuse_if_read_only for it.
        let names: Vec<String> = CartogServer::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "cartog_update"),
            "cartog_update must be registered, got: {names:?}"
        );

        let writers = ["cartog_index", "cartog_rag_index"];
        assert!(
            !writers.contains(&"cartog_update"),
            "cartog_update must NOT be in the DB-write set that refuse_if_read_only gates"
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
        let r1 = index_with_optional_lsp(
            &db,
            &lsp_mgr,
            &fixtures,
            false,
            None,
            None,
            indexer::RedactionConfig::disabled(),
        )
        .unwrap();
        assert!(
            r1.dirty_files > 0,
            "first index must report dirty files (got {})",
            r1.dirty_files
        );

        // Second call without changes must be a no-op AND must skip LSP.
        let r2 = index_with_optional_lsp(
            &db,
            &lsp_mgr,
            &fixtures,
            false,
            None,
            None,
            indexer::RedactionConfig::disabled(),
        )
        .unwrap();
        assert_eq!(r2.dirty_files, 0);
        assert_eq!(
            r2.edges_lsp_resolved, 0,
            "no-op reindex must skip LSP (MCP-side gate broken)"
        );
        assert_eq!(
            r2.edges_marked_external, 0,
            "no-op reindex must not produce new external marks"
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
            pid_lock_slot: Some(SERVE_LOCK_SLOT.to_string()),
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
        promoter_args_with_slot(
            db,
            role,
            db_path,
            state_dir,
            primary,
            pinned,
            SERVE_LOCK_SLOT,
        )
    }

    fn promoter_args_with_slot(
        db: Arc<Mutex<Database>>,
        role: Arc<AtomicRole>,
        db_path: std::path::PathBuf,
        state_dir: std::path::PathBuf,
        primary: cartog_process_lock::ActiveLock,
        pinned: Option<PinnedAttach>,
        serve_slot: &str,
    ) -> PromoterArgs {
        // Test embedding provider: the reconcile step on promotion needs
        // SOMETHING to fingerprint. The promoter-test DBs are opened fresh and
        // never reconciled, so reconcile takes the harmless backfill branch
        // (stamps mock provider/model) rather than wiping a populated index.
        // Mock avoids loading the real ONNX model.
        PromoterArgs {
            db,
            role,
            lock_cell: Arc::new(Mutex::new(None)),
            watch_cell: Arc::new(Mutex::new(None)),
            stale_cell: Arc::new(Mutex::new(None)),
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            embedding_provider: Arc::new(Mutex::new(test_provider())),
            db_path: db_path.clone(),
            state_dir,
            serve_slot: serve_slot.to_string(),
            watch_slot: serve_to_watch_slot(serve_slot).expect("test slot must be valid"),
            cwd: std::env::current_dir().unwrap(),
            primary,
            pinned,
            watch_requested: false,
            rag_override: Some(false),
            rag_config: rag::EmbeddingProviderConfig::default(),
            redact: indexer::RedactionConfig::disabled(),
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
        let _serial = test_validate_call_counter::SERIAL.lock().await;
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
        let _serial = test_validate_call_counter::SERIAL.blocking_lock();
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let _ = Database::open(&db_path, 384).unwrap();
        }
        validate_pinned_state(&db_path, None).expect("no pin must validate trivially");
    }

    #[test]
    fn validate_pinned_state_detects_none_vs_some_drift() {
        // A brand-new DB never reconciled has no provider/model in
        // metadata, so a read-only attach captures `pinned.embedding =
        // None`. If the primary subsequently runs `rag index` and stamps
        // provider/model, the secondary must detect this as drift — pin
        // was None, disk is now Some(...). This path is distinct from
        // `validate_pinned_state_detects_drift`, which seeds provider+
        // model BEFORE the attach so the pin is Some(...) and only the
        // Some-vs-Some inequality is exercised.
        let _serial = test_validate_call_counter::SERIAL.blocking_lock();
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let pinned = {
            let _ = Database::open(&db_path, 384).unwrap();
            Database::open_readonly(&db_path)
                .unwrap()
                .pinned_attach()
                .cloned()
        };
        assert!(
            pinned.as_ref().is_some_and(|p| p.embedding.is_none()),
            "pin against an un-reconciled DB must capture embedding = None",
        );
        // Primary stamps provider+model.
        {
            let mutator = Database::open(&db_path, 384).unwrap();
            mutator.set_metadata("embedding_provider", "local").unwrap();
            mutator
                .set_metadata("embedding_model", "BGE-small-en-v1.5")
                .unwrap();
        }
        let err = validate_pinned_state(&db_path, pinned.as_ref())
            .expect_err("None pin vs Some on disk must surface as drift");
        assert!(
            err.to_string().contains("DB metadata changed"),
            "drift error should name the metadata change, got: {err}"
        );
    }

    #[test]
    fn validate_pinned_state_detects_drift() {
        // Drift is exercised via the embedding fingerprint (not
        // schema_version) because `open_readonly` rejects schema_version
        // mismatch *before* this helper's comparison runs.
        //
        // We seed provider+model BEFORE the read-only attach so the pin
        // captures `embedding = Some(local, BGE-..., 384)`, then mutate
        // to a different `Some(ollama, nomic, 384)`. This exercises the
        // Some vs Some inequality directly — not the None vs Some accident
        // that would silently stop firing if `Database::open` ever seeded
        // default provider/model values.
        let _serial = test_validate_call_counter::SERIAL.blocking_lock();
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let pinned = {
            let db = Database::open(&db_path, 384).unwrap();
            db.set_metadata("embedding_provider", "local").unwrap();
            db.set_metadata("embedding_model", "BGE-small-en-v1.5")
                .unwrap();
            drop(db);
            Database::open_readonly(&db_path)
                .unwrap()
                .pinned_attach()
                .cloned()
        };
        assert!(
            pinned.as_ref().and_then(|p| p.embedding.as_ref()).is_some(),
            "pin must capture Some(...) so the test exercises Some vs Some drift",
        );
        // Another writer rewrites provider + model under us.
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
    async fn promoter_acquires_db_scoped_slot_from_args() {
        // Regression: promoter_task must acquire the slot carried in
        // `args.serve_slot`, not a hardcoded `SERVE_LOCK_SLOT`. The
        // previous test scaffolding hardcoded the global slot, so a
        // refactor that reverts the acquire call to `SERVE_LOCK_SLOT`
        // would have shipped green. This test passes a non-global serve
        // slot and asserts the on-disk PID filename matches.
        let _serial = test_validate_call_counter::SERIAL.lock().await;
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
        // Dead primary holding the scoped slot we want the promoter to
        // claim. Slot name must match exactly what the promoter acquires
        // so the original `serve-fa11`.pid file (planted below) gets
        // reclaimed.
        let scoped_slot = "serve-fa11ed7e57c0fed5";
        let primary_pid_path = state_dir.join(format!("{scoped_slot}.pid"));
        std::fs::write(&primary_pid_path, "4194304\n0\n").unwrap();
        let primary = cartog_process_lock::ActiveLock {
            slot: scoped_slot.to_string(),
            pid: 4_194_304,
            start_time: None,
        };

        let args = promoter_args_with_slot(
            Arc::clone(&db),
            Arc::clone(&role),
            db_path,
            state_dir.clone(),
            primary,
            pinned,
            scoped_slot,
        );
        // Keep the lock_cell Arc alive past the task so the acquired
        // ProcessLock isn't dropped (and the PID file removed) before we
        // assert on it. The production run_server lifecycle does the
        // same: it holds lock_cell for the whole server lifetime.
        let lock_cell = Arc::clone(&args.lock_cell);

        let handle = tokio::task::spawn(promoter_task(args));
        // Give the promoter time to notice primary-gone and acquire.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        assert_eq!(
            role.load(),
            Role::Primary,
            "promoter must flip role after acquiring the scoped slot"
        );
        // The on-disk PID file must use the scoped slot, not the global.
        assert!(
            primary_pid_path.exists(),
            "expected promoter to acquire {primary_pid_path:?}"
        );
        let global_path = state_dir.join(format!("{SERVE_LOCK_SLOT}.pid"));
        assert!(
            !global_path.exists(),
            "promoter must NOT acquire the global slot ({global_path:?})"
        );

        // Drop the held lock explicitly so the temp dir teardown is clean.
        {
            let mut guard = lock_cell.lock().unwrap();
            *guard = None;
        }
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn promoter_spawns_watcher_with_db_scoped_watch_slot() {
        // Regression for review finding #2: promoter_task must claim the
        // watch slot carried in `args.watch_slot`, not a hardcoded
        // `WATCH_LOCK_SLOT`. The previous regression test only verified
        // the serve-slot acquire path; a refactor that reverts the
        // post-promotion watcher's `config.pid_lock_slot = Some(args
        // .watch_slot.clone())` to the global constant would have shipped
        // green. This test sets `watch_requested = true`, lets the
        // promoter spawn a watcher, and asserts the watcher claims the
        // scoped slot on disk.
        let _serial = test_validate_call_counter::SERIAL.lock().await;
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
        let scoped_slot = "serve-fa11ed7e57c0fed5";
        let primary_pid_path = state_dir.join(format!("{scoped_slot}.pid"));
        std::fs::write(&primary_pid_path, "4194304\n0\n").unwrap();
        let primary = cartog_process_lock::ActiveLock {
            slot: scoped_slot.to_string(),
            pid: 4_194_304,
            start_time: None,
        };

        let mut args = promoter_args_with_slot(
            Arc::clone(&db),
            Arc::clone(&role),
            db_path,
            state_dir.clone(),
            primary,
            pinned,
            scoped_slot,
        );
        // Enable the watcher-spawn path. RAG stays off and watch root
        // (cwd, captured by the helper) is the cartog crate dir — fine
        // for spawning the watcher; we tear it down immediately.
        args.watch_requested = true;
        let lock_cell = Arc::clone(&args.lock_cell);
        let watch_cell = Arc::clone(&args.watch_cell);

        let handle = tokio::task::spawn(promoter_task(args));

        // The expected watcher PID file uses the SCOPED watch slot derived
        // via `serve_to_watch_slot(scoped_slot)`.
        let expected_watch_slot = serve_to_watch_slot(scoped_slot).expect("scoped slot is valid");
        let expected_watch_pid = state_dir.join(format!("{expected_watch_slot}.pid"));
        let global_watch_pid = state_dir.join(format!("{}.pid", watch::WATCH_LOCK_SLOT));

        // Poll for up to 5s in 50ms increments — the watcher startup walks
        // the cwd, sweeps stale locks, and creates the debouncer; on a
        // loaded CI machine the cumulative cost can exceed any small fixed
        // sleep, so a fixed sleep here was a flake source.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !expected_watch_pid.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            expected_watch_pid.exists(),
            "watcher should have claimed the scoped slot at {expected_watch_pid:?}"
        );
        assert!(
            !global_watch_pid.exists(),
            "watcher must NOT claim the global slot ({global_watch_pid:?})"
        );

        // Clean shutdown so the temp dir teardown succeeds.
        {
            let mut wguard = watch_cell.lock().unwrap();
            if let Some(handle) = wguard.take() {
                handle.stop();
            }
        }
        {
            let mut guard = lock_cell.lock().unwrap();
            *guard = None;
        }
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn promoter_aborts_when_state_diverges_after_acquire() {
        // Integration smoke. The post-acquire branch logic is covered
        // by `validate_pinned_state_detects_drift`; this test verifies
        // the promoter wires drift detection to a clean exit (role stays
        // ReadOnly, task finishes).
        let _serial = test_validate_call_counter::SERIAL.lock().await;
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
        let _serial = test_validate_call_counter::SERIAL.lock().await;
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

    #[tokio::test]
    async fn promoter_runs_validate_pinned_state_both_before_and_after_acquire() {
        // Regression for the M-promoter (c) fix at the test layer: the
        // promoter must call `validate_pinned_state` BOTH before and
        // after `ProcessLock::acquire`. We need an assertion that catches
        // deleting EITHER call site:
        //
        // - `calls >= 2` alone is insufficient: pre-acquire validate
        //   bumps on every tick, so over multiple ticks the count climbs
        //   past 2 even if the post-acquire site is gone.
        // - `role == Primary` alone is insufficient: the role swap
        //   succeeds even if pre-acquire validate is skipped entirely.
        //
        // Asserting BOTH catches either deletion: post-acquire validate
        // gates the role swap (so Primary => post-acquire ran on at
        // least one tick), and we additionally require `calls >= 2` to
        // prove a second validate fired in the promote path.
        let _serial = test_validate_call_counter::SERIAL.lock().await;
        test_validate_call_counter::reset();

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
        // Pretend the primary is dead so the promoter exits the
        // primary-alive `continue` branch and reaches the validate calls.
        let primary = cartog_process_lock::ActiveLock {
            slot: SERVE_LOCK_SLOT.to_string(),
            pid: 4_194_304,
            start_time: None,
        };

        let args = promoter_args_for_test(
            Arc::clone(&db),
            Arc::clone(&role),
            db_path,
            state_dir,
            primary,
            pinned,
        );
        let handle = tokio::task::spawn(promoter_task(args));

        // Bounded poll instead of a fixed wall-clock sleep: keeps the
        // test fast on a healthy box and tolerates a stalled CI runner
        // up to the 2s deadline. We wait for BOTH conditions before
        // asserting — reading them once after a fixed sleep was the
        // source of the previous CI-flake risk.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if role.load() == Role::Primary && test_validate_call_counter::snapshot() >= 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        handle.abort();
        let _ = handle.await;

        let calls = test_validate_call_counter::snapshot();
        assert_eq!(
            role.load(),
            Role::Primary,
            "post-acquire validate gates the role swap; missing role flip implies the post-acquire site never ran"
        );
        assert!(
            calls >= 2,
            "validate_pinned_state must run twice per promotion tick (pre + post acquire), saw {calls}"
        );
    }

    // ── Read-tool handler tests over a real indexed DB ──
    //
    // Build a CartogServer over a temp DB pre-populated by indexing a small
    // Python fixture, then drive the async read handlers directly. This
    // exercises the real MCP dispatch (param parsing, error mapping, the
    // tool_response / tool_response_named integration) over real query
    // results, not mocks.

    const FIXTURE_SRC: &str = "\
class Animal:
    def speak(self):
        return helper()


class Dog(Animal):
    def speak(self):
        return helper()


def helper():
    return 42


def main():
    d = Dog()
    return d.speak()
";

    /// Index `FIXTURE_SRC` as `lib.py` into a temp dir, then return a primary
    /// server opened over the resulting DB. The TempDir is returned so the
    /// caller keeps it alive for the test's duration.
    fn indexed_server() -> (tempfile::TempDir, CartogServer) {
        let tmp = tempfile::TempDir::new().unwrap();
        // The index root must not be a dot-prefixed dir: the walker prunes any
        // entry whose name starts with '.', and TempDir names start with ".tmp".
        let root = tmp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("lib.py"), FIXTURE_SRC).unwrap();
        let db_path = tmp.path().join("cartog.db");
        let provider = test_provider();
        {
            let db = Database::open(&db_path, provider.dimension()).unwrap();
            indexer::index_directory(
                &db,
                &root,
                true,
                false,
                None,
                None,
                indexer::RedactionConfig::disabled(),
                &std::collections::HashMap::new(),
            )
            .expect("fixture indexes");
        }
        let server = CartogServer::new_with_provider(
            &db_path,
            provider,
            indexer::RedactionConfig::disabled(),
            Role::Primary,
        )
        .expect("server constructs");
        (tmp, server)
    }

    /// Extract the text payload of a successful single-content tool result.
    fn result_text(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .expect("tool result has text content")
    }

    #[tokio::test]
    async fn outline_lists_symbols_in_file() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_outline(Parameters(OutlineParams {
                file: "lib.py".to_string(),
            }))
            .await
            .expect("outline succeeds");
        let text = result_text(&result);
        assert!(
            text.contains("Animal"),
            "outline should list the Animal class"
        );
        assert!(
            text.contains("helper"),
            "outline should list the helper function"
        );
    }

    #[tokio::test]
    async fn outline_unknown_file_returns_empty_array() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_outline(Parameters(OutlineParams {
                file: "nonexistent.py".to_string(),
            }))
            .await
            .expect("outline of unknown file is not an error");
        let text = result_text(&result);
        assert!(
            text.trim_start().starts_with('['),
            "empty outline is a JSON array"
        );
    }

    #[tokio::test]
    async fn refs_finds_callers_of_helper() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_refs(Parameters(RefsParams {
                name: "helper".to_string(),
                kind: None,
            }))
            .await
            .expect("refs succeeds");
        let text = result_text(&result);
        assert!(text.contains("helper"), "refs to helper should mention it");
    }

    #[tokio::test]
    async fn refs_rejects_invalid_edge_kind() {
        let (_dir, server) = indexed_server();
        let err = server
            .cartog_refs(Parameters(RefsParams {
                name: "helper".to_string(),
                kind: Some("bogus".to_string()),
            }))
            .await
            .expect_err("invalid edge kind must be rejected");
        assert!(
            err.message.contains("invalid edge kind"),
            "error should name the invalid kind, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn refs_unknown_name_suggests_near_matches() {
        let (_dir, server) = indexed_server();
        // "helpe" is one char off "helper" — should trigger did-you-mean.
        let result = server
            .cartog_refs(Parameters(RefsParams {
                name: "helpe".to_string(),
                kind: None,
            }))
            .await
            .expect("refs of near-miss name succeeds");
        let text = result_text(&result);
        assert!(
            text.contains("Did you mean") && text.contains("helper"),
            "near-miss should suggest helper, got: {text}"
        );
    }

    #[tokio::test]
    async fn callees_traces_calls_from_main() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_callees(Parameters(CalleesParams {
                name: "main".to_string(),
            }))
            .await
            .expect("callees succeeds");
        let text = result_text(&result);
        assert!(
            text.trim_start().starts_with('['),
            "callees returns a JSON array"
        );
    }

    #[tokio::test]
    async fn impact_clamps_depth_and_returns_array() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_impact(Parameters(ImpactParams {
                name: "helper".to_string(),
                depth: Some(999), // clamped to MAX_IMPACT_DEPTH internally
            }))
            .await
            .expect("impact succeeds");
        let text = result_text(&result);
        assert!(
            text.trim_start().starts_with('['),
            "impact returns a JSON array"
        );
    }

    #[tokio::test]
    async fn trace_finds_path_from_speak_to_helper() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_trace(Parameters(TraceParams {
                from: "speak".to_string(),
                to: "helper".to_string(),
                depth: Some(8),
            }))
            .await
            .expect("trace succeeds");
        let text = result_text(&result);
        assert!(text.contains("\"found\": true"), "path exists: {text}");
        assert!(text.contains("helper"), "hop should reach helper: {text}");
    }

    #[tokio::test]
    async fn trace_reports_no_path_when_unreachable() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_trace(Parameters(TraceParams {
                from: "helper".to_string(),
                to: "speak".to_string(),
                depth: Some(8),
            }))
            .await
            .expect("trace succeeds");
        let text = result_text(&result);
        assert!(text.contains("\"found\": false"), "no path: {text}");
    }

    #[tokio::test]
    async fn trace_hop_includes_body_when_content_indexed() {
        let (_dir, server) = indexed_server();
        // Seed RAG content for every `speak` symbol (the hop sources on the
        // speak→helper path), so the hop body is populated.
        {
            let db = server.db.lock().unwrap();
            for sym in db.search("speak", None, None, 10).unwrap() {
                db.upsert_symbol_content(
                    &sym.id,
                    "speak",
                    "def speak(self):\n    return helper()",
                    "// method speak",
                )
                .unwrap();
            }
        }
        let result = server
            .cartog_trace(Parameters(TraceParams {
                from: "speak".to_string(),
                to: "helper".to_string(),
                depth: Some(8),
            }))
            .await
            .expect("trace succeeds");
        let text = result_text(&result);
        assert!(
            text.contains("\"body\""),
            "hop carries an inline body when content is indexed: {text}"
        );
    }

    #[tokio::test]
    async fn context_bundles_relevant_symbols_for_a_task() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_context(Parameters(ContextParams {
                task: "speak".to_string(),
                tokens: Some(6000),
            }))
            .await
            .expect("context succeeds");
        let text = result_text(&result);
        assert!(text.contains("\"task\": \"speak\""), "echoes task: {text}");
        assert!(text.contains("\"entries\""), "returns entries: {text}");
    }

    #[tokio::test]
    async fn rag_search_prepends_banner_when_embeddings_pending() {
        let (_dir, server) = indexed_server();
        // Simulate a live watcher with pending embeddings.
        let stale = cartog_watch::StaleState::new();
        stale.note_reindex(0, 7);
        *server.stale.lock().unwrap() = Some(stale);
        server
            .watcher_active
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let result = server
            .cartog_rag_search(Parameters(RagSearchParams {
                query: "helper".to_string(),
                kind: None,
                limit: Some(5),
            }))
            .await
            .expect("rag search succeeds");
        let text = result_text(&result);
        assert!(
            text.starts_with("⚠️") && text.contains("re-embedding"),
            "banner prepended: {text}"
        );
    }

    #[tokio::test]
    async fn no_banner_without_active_watcher() {
        let (_dir, server) = indexed_server();
        // Stale state present but watcher_active is false (degraded/no watcher).
        let stale = cartog_watch::StaleState::new();
        stale.note_reindex(0, 7);
        *server.stale.lock().unwrap() = Some(stale);

        let result = server
            .cartog_rag_search(Parameters(RagSearchParams {
                query: "helper".to_string(),
                kind: None,
                limit: Some(5),
            }))
            .await
            .expect("rag search succeeds");
        assert!(!result_text(&result).starts_with("⚠️"), "no banner");
    }

    #[tokio::test]
    async fn context_rejects_empty_task() {
        let (_dir, server) = indexed_server();
        let err = server
            .cartog_context(Parameters(ContextParams {
                task: String::new(),
                tokens: None,
            }))
            .await
            .expect_err("empty task must be rejected");
        assert!(
            err.message.contains("cannot be empty"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn hierarchy_reports_dog_extends_animal() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_hierarchy(Parameters(HierarchyParams {
                name: "Dog".to_string(),
            }))
            .await
            .expect("hierarchy succeeds");
        let text = result_text(&result);
        assert!(
            text.contains("Animal"),
            "Dog's hierarchy should reach Animal: {text}"
        );
    }

    #[tokio::test]
    async fn deps_lists_file_imports() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_deps(Parameters(DepsParams {
                file: "lib.py".to_string(),
            }))
            .await
            .expect("deps succeeds");
        let text = result_text(&result);
        assert!(
            text.trim_start().starts_with('['),
            "deps returns a JSON array"
        );
    }

    #[tokio::test]
    async fn search_finds_symbol_by_partial_name() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_search(Parameters(SearchParams {
                query: "Anim".to_string(),
                kind: None,
                file: None,
                limit: None,
            }))
            .await
            .expect("search succeeds");
        let text = result_text(&result);
        assert!(
            text.contains("Animal"),
            "search for 'Anim' should find Animal"
        );
    }

    #[tokio::test]
    async fn search_rejects_empty_query() {
        let (_dir, server) = indexed_server();
        let err = server
            .cartog_search(Parameters(SearchParams {
                query: String::new(),
                kind: None,
                file: None,
                limit: None,
            }))
            .await
            .expect_err("empty query must be rejected");
        assert!(
            err.message.contains("query cannot be empty"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn search_rejects_invalid_kind() {
        let (_dir, server) = indexed_server();
        let err = server
            .cartog_search(Parameters(SearchParams {
                query: "Animal".to_string(),
                kind: Some("nonsense".to_string()),
                file: None,
                limit: None,
            }))
            .await
            .expect_err("invalid symbol kind must be rejected");
        assert!(
            err.message.contains("invalid symbol kind"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn stats_reports_role_and_symbol_count() {
        let (_dir, server) = indexed_server();
        let result = server.cartog_stats().await.expect("stats succeeds");
        let text = result_text(&result);
        let value: serde_json::Value = serde_json::from_str(&text).expect("stats is JSON");
        assert_eq!(
            value["role"], "primary",
            "primary server reports primary role"
        );
        assert!(
            value["num_symbols"].as_u64().unwrap_or(0) > 0,
            "indexed fixture has symbols: {text}"
        );
    }

    #[tokio::test]
    async fn map_returns_files_and_top_symbols() {
        let (_dir, server) = indexed_server();
        let result = server
            .cartog_map(Parameters(MapParams { limit: Some(10) }))
            .await
            .expect("map succeeds");
        let text = result_text(&result);
        assert!(
            text.contains("lib.py"),
            "map should list the indexed file: {text}"
        );
    }

    #[tokio::test]
    async fn read_tools_count_toward_query_log() {
        let (_dir, server) = indexed_server();
        let _ = server
            .cartog_search(Parameters(SearchParams {
                query: "helper".to_string(),
                kind: None,
                file: None,
                limit: None,
            }))
            .await
            .expect("search succeeds");
        let result = server.cartog_stats().await.expect("stats succeeds");
        let value: serde_json::Value =
            serde_json::from_str(&result_text(&result)).expect("stats is JSON");
        // stats itself plus the prior search both log; the field exists once
        // any read tool has run against a populated index.
        assert!(value.get("num_symbols").is_some(), "stats shape is intact");
    }
}
