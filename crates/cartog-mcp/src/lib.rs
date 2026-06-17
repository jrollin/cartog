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
use serde::Serialize;
use tracing::{debug, info};

use cartog_core::{Compact, EdgeKind};
use cartog_db::{Database, PinnedAttach, MAX_SEARCH_LIMIT};
use cartog_indexer as indexer;
use cartog_rag as rag;
use cartog_watch as watch;
use cartog_watch::{StaleSnapshot, WatchConfig, WatchHandle};

mod progress;
mod types;

use types::*;

#[cfg(test)]
mod e2e_progress;

const MAX_IMPACT_DEPTH: u32 = 10;
const MAX_TRACE_DEPTH: u32 = 20;
const DEFAULT_CONTEXT_TOKENS: u32 = 6000;
const MAX_CONTEXT_TOKENS: u32 = 20000;

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

/// Outcome of one warm LSP resolution pass (see [`warm_lsp_pass`]).
#[cfg(feature = "lsp")]
#[derive(Debug)]
struct WarmPassOutcome {
    /// State-4 (heuristic-exhausted) edges reopened to state 0 before the pass.
    reopened: u32,
    /// Edges re-sealed at state 4 because no language server started.
    resealed: u32,
    /// Resolution counts from `lsp_resolve_edges`.
    stats: cartog_lsp::LspResolveStats,
}

/// One warm LSP pass: reopen state-4 seals (else the pass queries nothing —
/// the #114 regression), resolve, re-seal if no server started (#109).
/// LSP-server failures degrade to a warning; DB failures and cancel are errors.
#[cfg(feature = "lsp")]
fn warm_lsp_pass(
    db: &Database,
    mgr: &mut cartog_lsp::manager::LspManager,
    root: &Path,
    progress: Option<cartog_lsp::LspProgress<'_>>,
    cancel: Option<indexer::CancelProbe<'_>>,
) -> Result<WarmPassOutcome, McpError> {
    let reopened = db
        .reopen_heuristic_exhausted()
        .map_err(|e| mcp_err(format!("reopening heuristic-exhausted edges failed: {e:#}")))?;
    // Overrides live on the shared `mgr` (set at construction), so the map
    // passed here is ignored — pass empty.
    let stats = match cartog_lsp::lsp_resolve_edges(
        db,
        root,
        Some(mgr),
        &std::collections::HashMap::new(),
        progress,
        cancel,
        1, // warm pass shares one manager → serial (cap ignored on this path)
    ) {
        Ok(stats) => stats,
        // A cancel must surface as an error (the MCP cancellation contract),
        // not be swallowed as a warning like a genuine LSP-server failure.
        Err(e) if cartog_indexer::is_cancelled(&e) => {
            // No surrounding tx here (unlike the indexer path): restore the
            // pre-pass seal so the backlog stays visible to the no-op
            // catch-up. Best effort — the cancel must still surface.
            if reopened > 0 {
                if let Err(seal_err) = db.mark_heuristic_exhausted_in_tx() {
                    tracing::warn!("re-sealing after a cancelled LSP pass failed: {seal_err:#}");
                }
            }
            return Err(mcp_err("indexing cancelled"));
        }
        Err(e) => {
            tracing::warn!("LSP resolution failed: {e:#}");
            cartog_lsp::LspResolveStats::default()
        }
    };
    let resealed = if !stats.any_server_started && reopened > 0 {
        db.mark_heuristic_exhausted_in_tx().map_err(|e| {
            mcp_err(format!(
                "re-sealing heuristic-exhausted edges failed: {e:#}"
            ))
        })?
    } else {
        0
    };
    Ok(WarmPassOutcome {
        reopened,
        resealed,
        stats,
    })
}

/// Run `index_directory` followed by an optional LSP resolution pass.
///
/// Exposed as a free function (rather than inlined in the `cartog_index`
/// tool handler) so integration tests can exercise the LSP gate without
/// constructing a full `CartogServer` (which loads ONNX models).
///
/// LSP pass is skipped on no-op runs (`dirty_files == 0`) — see
/// `cartog-indexer` for the gate rationale. On dirty runs the pass goes
/// through [`warm_lsp_pass`], which reopens the state-4 seals left by the
/// internal `lsp=false` index run.
#[cfg(feature = "lsp")]
#[allow(clippy::too_many_arguments)] // mirrors index_directory's order-stable knobs
fn index_with_optional_lsp(
    db: &Arc<Mutex<Database>>,
    lsp_manager: &Arc<Mutex<cartog_lsp::manager::LspManager>>,
    root: &Path,
    force: bool,
    progress_tx: Option<tokio::sync::mpsc::Sender<progress::Phase>>,
    cancel: Option<indexer::CancelProbe<'_>>,
    redact: indexer::RedactionConfig,
    filter: &indexer::WalkFilter,
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
            filter,
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
        let outcome = warm_lsp_pass(&db, &mut mgr, root, lsp_progress_ref, cancel)?;
        if outcome.reopened > 0 {
            tracing::debug!(
                reopened = outcome.reopened,
                resealed = outcome.resealed,
                "reopened heuristic-exhausted edges for the warm LSP pass"
            );
        }
        result.edges_lsp_resolved = outcome.stats.resolved;
        result.edges_marked_unresolvable = outcome.stats.marked_unresolvable;
        result.edges_marked_external = outcome.stats.marked_external;
        if outcome.stats.resolved > 0 {
            let _ = db.compute_in_degrees();
        }
    }

    Ok(result)
}

#[cfg(not(feature = "lsp"))]
#[allow(clippy::too_many_arguments)] // mirrors index_directory's order-stable knobs
fn index_with_optional_lsp(
    db: &Arc<Mutex<Database>>,
    _lsp_manager: &(),
    root: &Path,
    force: bool,
    progress_tx: Option<tokio::sync::mpsc::Sender<progress::Phase>>,
    cancel: Option<indexer::CancelProbe<'_>>,
    redact: indexer::RedactionConfig,
    filter: &indexer::WalkFilter,
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
        filter,
    )
    .map_err(|e| mcp_err(format!("indexing failed: {e}")))
}

/// Warm-LSP catch-up for no-op reindexes: a sealed (state-4) backlog — left by
/// a peer-deferred CLI index or by watcher reindexes — resolves here instead of
/// waiting for the next dirty index. Latches `lsp_unavailable` after a
/// serverless pass so LSP-less environments don't retry every call.
#[cfg(feature = "lsp")]
fn catch_up_lsp(
    db: &Arc<Mutex<Database>>,
    lsp_manager: &Arc<Mutex<cartog_lsp::manager::LspManager>>,
    lsp_unavailable: &std::sync::atomic::AtomicBool,
    root: &Path,
    progress_tx: Option<tokio::sync::mpsc::Sender<progress::Phase>>,
    cancel: Option<indexer::CancelProbe<'_>>,
    mut result: indexer::IndexResult,
) -> Result<indexer::IndexResult, McpError> {
    use std::sync::atomic::Ordering;
    // Dirty runs already did a (global) warm pass in index_with_optional_lsp.
    if result.dirty_files > 0 || lsp_unavailable.load(Ordering::Acquire) {
        return Ok(result);
    }
    // Lock ordering: lsp_manager → db (see CartogServer doc).
    let mut mgr = lsp_manager.lock().map_err(|_| {
        mcp_err("internal error: LSP manager lock poisoned (server restart required)")
    })?;
    let db = db
        .lock()
        .map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;
    let sealed = db
        .has_heuristic_exhausted()
        .map_err(|e| mcp_err(format!("querying the sealed-edge backlog failed: {e:#}")))?;
    if !sealed {
        return Ok(result);
    }
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
    let outcome = warm_lsp_pass(&db, &mut mgr, root, lsp_progress_ref, cancel)?;
    if !outcome.stats.any_server_started {
        lsp_unavailable.store(true, Ordering::Release);
    }
    result.edges_lsp_resolved += outcome.stats.resolved;
    result.edges_marked_unresolvable += outcome.stats.marked_unresolvable;
    result.edges_marked_external += outcome.stats.marked_external;
    if outcome.stats.resolved > 0 {
        let _ = db.compute_in_degrees();
    }
    Ok(result)
}

#[cfg(not(feature = "lsp"))]
fn catch_up_lsp(
    _db: &Arc<Mutex<Database>>,
    _lsp_manager: &(),
    _lsp_unavailable: &(),
    _root: &Path,
    _progress_tx: Option<tokio::sync::mpsc::Sender<progress::Phase>>,
    _cancel: Option<indexer::CancelProbe<'_>>,
    result: indexer::IndexResult,
) -> Result<indexer::IndexResult, McpError> {
    Ok(result)
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

/// Whether MCP tools strip heavy fields from their JSON output.
///
/// Agents are the only MCP consumer and the server already assumes token
/// pressure (the [`mcp_max_bytes`] cap). Compact is therefore the default:
/// drop docstrings and cache hashes from symbols, and bound `rag_search` bodies
/// to a snippet. Set `CARTOG_MCP_COMPACT=0` (or `false`/`no`/`off`) to restore
/// full bodies.
fn mcp_compact() -> bool {
    match std::env::var("CARTOG_MCP_COMPACT") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
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
    /// Latched true after a warm-LSP catch-up where no server started, so an
    /// LSP-less environment doesn't retry the catch-up on every index call.
    #[cfg(feature = "lsp")]
    lsp_unavailable: Arc<std::sync::atomic::AtomicBool>,
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
    /// Walk filter (`[index] exclude` globs + gitignore policy), applied by
    /// the indexing tools. `Arc` so `#[derive(Clone)]` stays cheap (WalkFilter
    /// is not Copy).
    walk_filter: Arc<indexer::WalkFilter>,
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
    /// indexed content, `lsp_overrides` maps a cartog language to its
    /// `[lsp.<lang>] command` argv for the warm `LspManager` (empty = default
    /// PATH-resolved servers), and `filter` is the walk filter (`[index]
    /// exclude` globs + gitignore policy) the indexing tools apply. For the
    /// read-only attach path see [`new_read_only`](Self::new_read_only).
    pub fn new(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
        redact: indexer::RedactionConfig,
        lsp_overrides: std::collections::HashMap<String, Vec<String>>,
        filter: indexer::WalkFilter,
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
        Self::from_parts(
            db,
            provider,
            reranker,
            redact,
            lsp_overrides,
            filter,
            Role::Primary,
        )
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
        filter: indexer::WalkFilter,
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
            filter,
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
        filter: indexer::WalkFilter,
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
            #[cfg(feature = "lsp")]
            lsp_unavailable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cwd: Arc::from(cwd),
            role: Arc::new(AtomicRole::new(role)),
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stale: Arc::new(Mutex::new(None)),
            redact,
            walk_filter: Arc::new(filter),
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
        filter: indexer::WalkFilter,
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
            filter,
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
        let walk_filter = Arc::clone(&self.walk_filter);
        #[cfg(feature = "lsp")]
        let lsp_manager = Arc::clone(&self.lsp_manager);
        #[cfg(not(feature = "lsp"))]
        let lsp_manager: () = ();
        #[cfg(feature = "lsp")]
        let lsp_unavailable = Arc::clone(&self.lsp_unavailable);
        #[cfg(not(feature = "lsp"))]
        let lsp_unavailable: () = ();

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
                progress_tx.clone(),
                probe_ref,
                redact,
                walk_filter.as_ref(),
            )?;
            let result = catch_up_lsp(
                &db,
                &lsp_manager,
                &lsp_unavailable,
                &validated,
                progress_tx,
                probe_ref,
                result,
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
            let mut symbols = db
                .outline(&file)
                .map_err(|e| mcp_err(format!("outline query failed: {e}")))?;
            if mcp_compact() {
                symbols.compact_in_place();
            }

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
            let mut symbols = db
                .search(&query, kind_filter, file_filter, limit)
                .map_err(|e| mcp_err(format!("search failed: {e}")))?;
            if mcp_compact() {
                symbols.compact_in_place();
            }

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
            let mut top_symbols = db
                .top_symbols(limit)
                .map_err(|e| mcp_err(format!("top_symbols query failed: {e}")))?;
            if mcp_compact() {
                top_symbols.compact_in_place();
            }

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
            let mut symbols = db
                .symbols_for_files(&changed_files, kind_filter)
                .map_err(|e| mcp_err(format!("symbols query failed: {e}")))?;
            if mcp_compact() {
                symbols.compact_in_place();
            }

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
        let walk_filter = Arc::clone(&self.walk_filter);
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
                walk_filter.as_ref(),
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
            let mut result = match reranker.as_mut() {
                Some(r) => rag::search::hybrid_search(
                    &db, &query, limit, kind_filter, provider.as_mut(), Some(r.as_mut()),
                ),
                None => rag::search::hybrid_search(
                    &db, &query, limit, kind_filter, provider.as_mut(), None,
                ),
            }.map_err(|e| mcp_err(format!("semantic search failed: {e}")))?;
            // Compact-by-default: bound each body to a snippet (the tool advertises
            // "snippet excerpts"). CARTOG_MCP_COMPACT=0 restores full bodies.
            if mcp_compact() {
                result.snippet_in_place();
            }

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
            // Compact trims per-entry symbol noise but KEEPS the budgeted bodies —
            // this tool's whole value is its inline bodies.
            let mut result = result;
            if mcp_compact() {
                result.compact_in_place();
            }

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
                 Languages: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, PHP, Dart, Swift, Kotlin, Vue, Svelte, Astro, Markdown. \
                 Frameworks: React, Vue, Svelte, Astro — JSX/SFC component-usage edges.",
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
mod tests;
