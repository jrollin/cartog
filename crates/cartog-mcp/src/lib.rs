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
    handler::server::{router::tool::ToolRouter, tool::schema_for_output},
    model::*,
    tool_handler,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Serialize;
use tracing::{debug, info};

use cartog_db::{Database, DbError, PinnedAttach};
use cartog_indexer as indexer;
use cartog_rag as rag;
use cartog_watch as watch;
use cartog_watch::{StaleSnapshot, WatchConfig, WatchHandle};

mod lazy_provider;
mod progress;
mod tools;
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

/// Byte budget for the *result list* passed to [`fit_to_budget`], reserving
/// headroom under [`mcp_max_bytes`] for everything the list isn't sized
/// against: the structured wrapper (`{"results": …}`) and sibling fields
/// (`MapResult.files`, `ChangesResult.changed_files`), the staleness banner,
/// and the truncation notice. Sizing the bare array against the full cap would
/// let those push `structuredContent` (which, unlike the text block, has no
/// final clamp) past the cap.
fn mcp_list_budget() -> usize {
    mcp_max_bytes().saturating_sub(1024)
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
/// `structuredContent` is attached only when present (callers that return no
/// structured data pass `None`).
fn success_result(text: String, structured: Option<serde_json::Value>) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(text)]);
    result.structured_content = structured;
    result
}

/// Trim a result list so its serialized (bare-array) form fits `budget` bytes,
/// returning the kept items and the count dropped.
///
/// A tool with an `output_schema` must return `structuredContent` per the MCP
/// spec, so we can't byte-truncate the serialized JSON (that yields invalid,
/// non-conforming JSON) and we can't drop it entirely. Instead we bound the
/// payload at the *element* level — dropping trailing items keeps both the
/// text block and the structured wrapper valid and mutually consistent, since
/// the caller builds both from the returned slice. Every list wrapper's array
/// field is unconstrained (no `minItems`), so an empty result stays schema-valid.
///
/// Uses binary search on the kept-count so a huge list doesn't re-serialize
/// per dropped element. Falls back to keeping zero items if even one element
/// serializes larger than the budget (the notice then reports every item
/// omitted, and the final byte-clamp in [`tool_response_named`] guards the
/// text block).
#[must_use]
fn fit_to_budget<T: Serialize>(items: Vec<T>, budget: usize) -> (Vec<T>, usize) {
    let serialized_len = |n: usize| {
        serde_json::to_string_pretty(&items[..n])
            .map(|s| s.len())
            .unwrap_or(usize::MAX)
    };
    if serialized_len(items.len()) <= budget {
        return (items, 0);
    }
    // Largest prefix length whose serialized form fits the budget.
    let (mut lo, mut hi) = (0usize, items.len());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if serialized_len(mid) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let omitted = items.len() - lo;
    let mut kept = items;
    kept.truncate(lo);
    (kept, omitted)
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
/// for schema-aware clients — tools with `output_schema` must return
/// `structuredContent` per the MCP spec, so it is always kept when present.
/// The text block keeps the original (possibly bare-array) JSON shape.
///
/// `omitted` is the count of result items the caller already dropped (via
/// [`fit_to_budget`]) to keep the response under `mcp_max_bytes()`. Callers
/// bound the payload at the element level *before* building both `json` and
/// `structured` from the same trimmed slice, so text and structured stay
/// mutually consistent and both fit the cap. When `omitted > 0` an honest
/// "N result(s) omitted" notice pointing at a narrower tool is appended. A
/// non-list tool that can't overflow passes `0`.
fn tool_response(
    db: &Database,
    json: String,
    structured: Option<serde_json::Value>,
    tool: &str,
    omitted: usize,
    stale: Option<StaleSnapshot>,
) -> Result<CallToolResult, McpError> {
    tool_response_named(db, json, structured, tool, omitted, None, stale)
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
    omitted: usize,
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
                    return Ok(success_result(text, structured));
                }
            }
        }
    }

    let mut text = json;

    // Callers trim the result list to the byte budget before building both
    // `json` and `structured`, so an honest count of dropped items is known
    // up front (no byte-level guessing, and text/structured agree).
    if omitted > 0 {
        text.push_str(&format!(
            "\n\n(Response truncated: {omitted} result(s) omitted to stay under the \
             {cap}-byte cap. {hint})",
            cap = mcp_max_bytes(),
            hint = narrowing_hint_for(tool),
        ));
    } else if is_empty {
        text.push_str("\n\n(Index is empty. Run cartog_index first to build the code graph.)");
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
    /// Reranker provider, built on first semantic query rather than at start.
    /// The cross-encoder commits ~162 MB that an idle server never uses; see
    /// [`lazy_provider`].
    reranker_provider: Arc<lazy_provider::LazyReranker>,
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
    /// True when this Primary started **degraded**: no `.cartog.toml`, no
    /// existing index, and `CARTOG_AUTO_INIT` unset, so no `.cartog/` was
    /// created. The DB is an empty in-memory placeholder — read tools return
    /// empty + a "run `cartog init`" hint and the 2 write tools refuse with the
    /// no-config message. A `--watch` Primary's watcher pre-builds the index
    /// once `cartog init` runs; the running process stays degraded until the
    /// client relaunches it (no live in-place DB swap). A read-only secondary
    /// also starts degraded if it found no DB on disk (its primary is degraded).
    /// Immutable for the process lifetime — set once at construction, never swapped.
    degraded: bool,
    /// Path this server's index was opened from, as given to the constructor.
    ///
    /// Kept so `cartog_list_projects` can mark which registry row is *this*
    /// project — an agent must not re-route to itself. Compared by slot rather
    /// than by string, so a relative or symlinked path still matches. `cwd` is
    /// the wrong key: the plugin launches `serve` with no `--db`, and a session
    /// in a subdirectory has a cwd that is not the project root.
    ///
    /// Empty for an in-memory database (a degraded server or a test).
    db_path: Arc<Path>,
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
    /// exclude` globs + gitignore policy) the indexing tools apply.
    ///
    /// `allow_create` is the consent gate: when `false` and no index exists
    /// yet, the server starts **degraded** — it does NOT create a `.cartog/`,
    /// opens an empty in-memory DB instead, refuses the 2 write tools with a
    /// "run `cartog init`" message, and surfaces the degraded state in
    /// `cartog_stats`. When `true` (or an index already exists) it opens for
    /// real. For the read-only attach path see
    /// [`new_read_only`](Self::new_read_only).
    pub fn new(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
        redact: indexer::RedactionConfig,
        lsp_overrides: std::collections::HashMap<String, Vec<String>>,
        filter: indexer::WalkFilter,
        allow_create: bool,
    ) -> anyhow::Result<Self> {
        // Consent gate. When creation is not allowed, open only if a DB already
        // exists; an absent DB yields a degraded server with an empty in-memory
        // placeholder — never a freshly-created `.cartog/`.
        let (db, degraded) = if allow_create {
            let db = Database::open(db_path, rag_config.resolved_dimension())
                .map_err(|e| anyhow::anyhow!("failed to open database: {e}"))?;
            (db, false)
        } else {
            match Database::open_existing(db_path, rag_config.resolved_dimension()) {
                Ok(db) => (db, false),
                Err(DbError::NotFound { .. }) => {
                    info!(
                        "no usable .cartog.toml and no index yet — starting MCP server \
                         degraded (no .cartog/ created). Run `cartog init` to opt in, or \
                         fix the reported config error if a .cartog.toml exists; the index \
                         loads on the next Claude Code launch."
                    );
                    let db = Database::open_memory()
                        .map_err(|e| anyhow::anyhow!("failed to open in-memory database: {e}"))?;
                    (db, true)
                }
                Err(e) => return Err(anyhow::anyhow!("failed to open database: {e}")),
            }
        };
        let provider = rag::create_embedding_provider(&rag_config)
            .map_err(|e| anyhow::anyhow!("failed to load embedding model: {e}"))?;
        // Only reconcile against a real on-disk DB; an in-memory degraded
        // placeholder has nothing to reconcile and is discarded on relaunch.
        if !degraded {
            db.reconcile_embedding_fingerprint(&rag::fingerprint_of(provider.as_ref()))
                .map_err(|e| anyhow::anyhow!("failed to reconcile embedding fingerprint: {e}"))?;
        }
        let reranker = lazy_provider::lazy_reranker(rag_config.clone());
        Self::from_parts(
            db,
            provider,
            reranker,
            redact,
            lsp_overrides,
            filter,
            Role::Primary,
            degraded,
            db_path,
        )
    }

    /// Construct a secondary MCP server that attached read-only because
    /// another cartog process owns the `serve` PID lock. Skips schema
    /// migrations and the embedding-fingerprint reconcile (the primary
    /// owns both); the 2 DB-write tools return a clear error at dispatch
    /// time. The other 15 tools (14 read + `cartog_update`, which arms a
    /// machine-level deferred update, not a DB write) work normally.
    ///
    /// Absent on-disk DB ⇒ the lock-holding primary is itself degraded
    /// (config-less, un-indexed). This secondary then starts degraded too
    /// instead of failing a read-only open of a missing file, so the
    /// serve-for-all-clients flow never leaves a second client without an MCP
    /// server. Picks up the index on relaunch, like the degraded primary.
    pub fn new_read_only(
        db_path: &std::path::Path,
        rag_config: rag::EmbeddingProviderConfig,
        redact: indexer::RedactionConfig,
        lsp_overrides: std::collections::HashMap<String, Vec<String>>,
        filter: indexer::WalkFilter,
    ) -> anyhow::Result<Self> {
        // Absent DB ⇒ mirror the degraded primary rather than erroring on a
        // read-only open of a missing file.
        if !db_path.exists() {
            info!(
                "no index on disk and another cartog process holds the serve lock \
                 (a degraded primary) — starting this secondary degraded too; it \
                 loads the index on the next Claude Code launch."
            );
            let db = Database::open_memory()
                .map_err(|e| anyhow::anyhow!("failed to open in-memory database: {e}"))?;
            let provider = rag::create_embedding_provider(&rag_config)
                .map_err(|e| anyhow::anyhow!("failed to load embedding model: {e}"))?;
            let reranker = lazy_provider::lazy_reranker(rag_config.clone());
            // Primary role keeps a degraded server (no DB, refuses writes) out of
            // the ReadOnly promotion path; degraded=true gates the write tools.
            return Self::from_parts(
                db,
                provider,
                reranker,
                redact,
                lsp_overrides,
                filter,
                Role::Primary,
                true,
                db_path,
            );
        }
        let db = Database::open_readonly(db_path)
            .map_err(|e| anyhow::anyhow!("failed to open database read-only: {e}"))?;
        let provider = rag::create_embedding_provider(&rag_config)
            .map_err(|e| anyhow::anyhow!("failed to load embedding model: {e}"))?;
        let reranker = lazy_provider::lazy_reranker(rag_config.clone());
        Self::from_parts(
            db,
            provider,
            reranker,
            redact,
            lsp_overrides,
            filter,
            Role::ReadOnly,
            false, // an existing DB was found on disk
            db_path,
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
        // Degraded: results come from the empty in-memory placeholder, so a
        // "results may be stale" banner over zero hits would be misleading — the
        // cartog_stats degraded banner already says "no index yet".
        if self.is_degraded() {
            return None;
        }
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

    /// Whether the cross-encoder has been built yet.
    ///
    /// The ~162 MB it commits is the whole reason for [`lazy_provider`], and
    /// `make bench-memory` — the footprint guard — runs only on macOS and in no
    /// CI job. This is what lets a plain `cargo test` catch a regression that
    /// re-eagers the build.
    #[cfg(test)]
    pub(crate) fn reranker_is_loaded(&self) -> bool {
        self.reranker_provider.is_loaded()
    }

    #[allow(clippy::too_many_arguments)] // single field-wiring point
    fn from_parts(
        db: Database,
        provider: Box<dyn rag::provider::EmbeddingProvider>,
        reranker: lazy_provider::LazyReranker,
        redact: indexer::RedactionConfig,
        lsp_overrides: std::collections::HashMap<String, Vec<String>>,
        filter: indexer::WalkFilter,
        role: Role,
        degraded: bool,
        db_path: &Path,
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
            reranker_provider: Arc::new(reranker),
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
            degraded,
            db_path: Arc::from(db_path),
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
        // Mirror production: a ReadOnly attach against an absent DB degrades
        // (its primary is degraded) instead of failing open_readonly.
        let mut degraded = false;
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
            Role::ReadOnly if !db_path.exists() => {
                degraded = true;
                Database::open_memory()
                    .map_err(|e| anyhow::anyhow!("failed to open in-memory database: {e}"))?
            }
            Role::ReadOnly => Database::open_readonly(db_path)
                .map_err(|e| anyhow::anyhow!("failed to open database read-only: {e}"))?,
        };
        Self::from_parts(
            db,
            provider,
            lazy_provider::no_reranker(),
            redact,
            std::collections::HashMap::new(),
            filter,
            role,
            degraded,
            db_path,
        )
    }

    /// Test-only constructor for the **degraded** primary: an empty in-memory
    /// DB and `degraded = true`, with no on-disk file. Mirrors what `new`
    /// produces when consent is absent and no index exists.
    #[cfg(test)]
    fn new_degraded_for_tests(
        provider: Box<dyn rag::provider::EmbeddingProvider>,
        redact: indexer::RedactionConfig,
        filter: indexer::WalkFilter,
    ) -> anyhow::Result<Self> {
        let db = Database::open_memory()
            .map_err(|e| anyhow::anyhow!("failed to open in-memory database: {e}"))?;
        Self::from_parts(
            db,
            provider,
            lazy_provider::no_reranker(),
            redact,
            std::collections::HashMap::new(),
            filter,
            Role::Primary,
            true,
            // A degraded server has no on-disk database, so it has no identity
            // in the project registry — and registers nothing.
            std::path::Path::new(""),
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

    /// True when this server started degraded (no consent + no existing index).
    fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// If we started degraded (no `.cartog.toml`, no index, no
    /// `CARTOG_AUTO_INIT`), return an `McpError` telling the user to opt in.
    /// A tool call is **not** approval — distinct from the read-only-secondary
    /// refusal, which fires only against an existing DB owned by a peer. `None`
    /// when the server has a real index and the write should proceed.
    fn refuse_if_degraded(&self, tool: &str) -> Option<McpError> {
        if self.is_degraded() {
            // Deliberately does not claim the config is *absent*: a present but
            // rejected `.cartog.toml` also lands here (cartog won't guess a
            // `[database] path` it couldn't read), and telling a user they have
            // no config while they are looking at one is the failure this avoids.
            Some(mcp_err(format!(
                "`{tool}` is unavailable: this project has no usable .cartog.toml and no \
                 index yet, so cartog will not create one automatically. Run `cartog init` \
                 to opt in (the index builds in the background and loads on the next Claude \
                 Code launch), or set CARTOG_AUTO_INIT=1 to index with defaults without \
                 writing a config file. If a .cartog.toml does exist, check stderr for the \
                 config error it reported — fix that and relaunch."
            )))
        } else {
            None
        }
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

    /// Combined router over every per-domain tool block. Hand-written so the
    /// `tool_router` name the `#[tool_handler]` default and tests expect still
    /// resolves; each `*_router()` is generated by the `#[tool_router(router=...)]`
    /// on its tools/ submodule impl block.
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::index_router()
            + Self::graph_router()
            + Self::search_router()
            + Self::rag_router()
            + Self::manage_router()
            + Self::projects_router()
            + Self::search_all_router()
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
                 cartog_rag_search (find code by concept), cartog_search (look up an exact symbol name), \
                 cartog_list_projects (for a question about a DIFFERENT repository on this \
                 machine — it returns each project's db_path to pass as --db). \
                 Languages: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, PHP, Dart, Swift, Kotlin, C, C++, C#, Vue, Svelte, Astro, Markdown. \
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
