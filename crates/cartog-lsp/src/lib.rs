//! LSP-based edge resolution for the cartog code graph.
//!
//! Resolves edges left unresolved by the heuristic resolver in [`cartog_db`],
//! by querying real language servers (pyright, rust-analyzer, etc.) for
//! `textDocument/definition` responses. Optional — gated behind the `lsp` feature.
#![doc = ""]
#![doc = include_str!("../README.md")]

pub mod client;
pub mod manager;
mod resolve;
pub mod servers;

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use cartog_core::detect_language;
use cartog_db::{Database, UnresolvedEdge};

use manager::LspManager;
use resolve::{effective_cap, resolve_parallel, resolve_serial, ProgressSink};

/// Per-edge progress callback for [`lsp_resolve_edges`]: `(done, total)`.
/// Called synchronously from the resolution loop; keep it cheap.
pub type LspProgress<'a> = &'a (dyn Fn(u32, u32) + Send + Sync);

/// Cooperative-cancellation probe for [`lsp_resolve_edges`]. Returns `true` when
/// the caller wants resolution to stop. Polled at language/file boundaries and
/// between in-flight windows inside `definitions_batch`, so worst-case latency
/// is one window's await. On trip, `lsp_resolve_edges` returns an `Err` whose
/// root cause is `"cancelled"` (matching the indexer's `CancelProbe` contract).
pub type LspCancel<'a> = &'a (dyn Fn() -> bool + Send + Sync);

/// Summary of an LSP resolution pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct LspResolveStats {
    /// Edges flipped from `resolution_state = 0` to `1` (target_id set).
    pub resolved: u32,
    /// Edges flipped from `resolution_state = 0` to `2` (LSP definitively gave up).
    pub marked_unresolvable: u32,
    /// Edges flipped from `resolution_state = 0` to `3` (LSP located the
    /// target outside the indexed root: stdlib, deps, node_modules).
    pub marked_external: u32,
    /// Whether at least one language server started this pass. `false` lets
    /// callers re-seal reopened edges instead of leaving a state-0 backlog.
    pub any_server_started: bool,
}

/// Resolve edges that heuristic resolution left unresolved, using LSP servers.
///
/// If `shared_manager` is provided, reuses existing LSP servers (warm start) —
/// it already carries any command overrides, so `overrides` is ignored in that
/// case. Otherwise creates a temporary manager seeded with `overrides`
/// (per-language `[lsp.<lang>] command`), dropped after resolution.
///
/// Returns counts for `resolved` (state=0 → 1), `marked_unresolvable`
/// (state=0 → 2, definitive LSP negative), and `marked_external` (state=0 → 3,
/// LSP located the target outside the indexed root).
///
/// `progress`, when `Some`, fires `(done, total)` as edges are processed.
/// `done` can end below `total` when a language server fails to start or when
/// an edge's target column can't be located (those edges are never queried).
///
/// `cancel`, when `Some` and it returns `true`, aborts at the next
/// language/file/window boundary with an `Err` cause of `"cancelled"`, marking
/// nothing. Resolved-edge persistence depends on the caller's transaction: the
/// indexer runs this inside its index tx, so a cancel rolls the whole pass back;
/// a caller with no surrounding tx keeps already-resolved edges and resumes.
pub fn lsp_resolve_edges(
    db: &Database,
    root: &Path,
    shared_manager: Option<&mut LspManager>,
    overrides: &HashMap<String, Vec<String>>,
    progress: Option<LspProgress<'_>>,
    cancel: Option<LspCancel<'_>>,
    max_concurrent_servers: usize,
) -> Result<LspResolveStats> {
    let unresolved = db.unresolved_edges()?;
    if unresolved.is_empty() {
        return Ok(LspResolveStats::default());
    }

    let total = unresolved.len() as u32;

    // Group by language (derived from file extension).
    let mut by_language: HashMap<String, Vec<UnresolvedEdge>> = HashMap::new();
    for edge in unresolved {
        if let Some(lang) = detect_language(Path::new(&edge.file_path)) {
            by_language.entry(lang.to_string()).or_default().push(edge);
        }
    }
    if by_language.is_empty() {
        return Ok(LspResolveStats::default());
    }

    let sink = ProgressSink::new(total, progress);
    sink.emit(0); // phase-start tick so the total shows before the first stride

    let cap = effective_cap(max_concurrent_servers, by_language.len());

    // The shared-manager (warm MCP) path and cap<=1 stay serial. Only the
    // owned-manager path with cap>1 fans out per-language.
    let stats = match shared_manager {
        Some(m) => {
            m.ensure_root(root);
            resolve_serial(db, root, m, &by_language, &sink, cancel)
        }
        None if cap <= 1 => {
            let mut manager = LspManager::with_overrides(root, overrides.clone());
            resolve_serial(db, root, &mut manager, &by_language, &sink, cancel)
        }
        None => resolve_parallel(db, root, overrides, by_language, &sink, cap, cancel),
    }?;

    sink.emit_final(sink.processed()); // lands on total when every edge ran

    if !stats.any_server_started {
        tracing::debug!("LSP: no servers found on PATH, skipping");
    } else if stats.resolved > 0 || stats.marked_unresolvable > 0 || stats.marked_external > 0 {
        tracing::info!(
            "LSP: resolved {} additional edges, marked {} unresolvable, {} external",
            stats.resolved,
            stats.marked_unresolvable,
            stats.marked_external
        );
    } else {
        tracing::info!("LSP: no additional edges resolved");
    }

    Ok(stats) // managers shut down via Drop
}

/// Find the column (0-based UTF-16 offset) of `target_name` in the given source line.
/// Uses word-boundary matching to avoid matching inside longer identifiers.
/// LSP positions use UTF-16 code units by default.
pub(crate) fn find_column_in_line(
    lines: &[&str],
    line_1based: u32,
    target_name: &str,
) -> Option<u32> {
    let idx = line_1based.checked_sub(1)? as usize;
    let line = lines.get(idx)?;

    let mut start = 0;
    while let Some(offset) = line[start..].find(target_name) {
        let abs_offset = start + offset;
        let end_offset = abs_offset + target_name.len();

        let before_ok = abs_offset == 0
            || !line.as_bytes()[abs_offset - 1].is_ascii_alphanumeric()
                && line.as_bytes()[abs_offset - 1] != b'_';

        let after_ok = end_offset >= line.len()
            || !line.as_bytes()[end_offset].is_ascii_alphanumeric()
                && line.as_bytes()[end_offset] != b'_';

        if before_ok && after_ok {
            return Some(line[..abs_offset].encode_utf16().count() as u32);
        }

        start = abs_offset + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_column_basic() {
        let lines = vec!["    result = validate_token(tok)"];
        assert_eq!(find_column_in_line(&lines, 1, "validate_token"), Some(13));
    }

    #[test]
    fn test_find_column_multiple_occurrences_takes_first() {
        let lines = vec!["foo(foo)"];
        assert_eq!(find_column_in_line(&lines, 1, "foo"), Some(0));
    }

    #[test]
    fn test_find_column_qualified_name() {
        let lines = vec!["self.validate_token()"];
        assert_eq!(find_column_in_line(&lines, 1, "validate_token"), Some(5));
    }

    #[test]
    fn test_find_column_not_found() {
        let lines = vec!["something_else()"];
        assert_eq!(find_column_in_line(&lines, 1, "validate_token"), None);
    }

    #[test]
    fn test_find_column_line_out_of_range() {
        let lines = vec!["one line"];
        assert_eq!(find_column_in_line(&lines, 5, "one"), None);
    }

    #[test]
    fn test_find_column_zero_line() {
        let lines = vec!["one line"];
        assert_eq!(find_column_in_line(&lines, 0, "one"), None);
    }

    #[test]
    fn test_find_column_word_boundary_skips_substring() {
        // "id" inside "validate_id" should be skipped, match the standalone "id"
        let lines = vec!["validate_id(id)"];
        assert_eq!(find_column_in_line(&lines, 1, "id"), Some(12));
    }

    #[test]
    fn test_find_column_word_boundary_at_start() {
        let lines = vec!["id = 5"];
        assert_eq!(find_column_in_line(&lines, 1, "id"), Some(0));
    }

    #[test]
    fn test_find_column_word_boundary_no_standalone() {
        // "id" only appears inside "valid" — no word-boundary match
        let lines = vec!["valid()"];
        assert_eq!(find_column_in_line(&lines, 1, "id"), None);
    }

    // ── Per-language success gate ──

    #[test]
    fn test_lsp_resolve_edges_no_servers_leaves_edges_unmarked() {
        // Two-mode coverage:
        // - No pyright on PATH (typical CI): exercises the manager.start()
        //   Err branch — server never starts, no marks possible.
        // - pyright present: exercises the buffered-negative + per-language
        //   gate. find_user is undefined, so LSP returns Ok(None) for the
        //   only candidate; lang_resolved stays 0, so the gate suppresses
        //   the mark. Same end-state: edge stays unmarked.
        // Note: writing a.py under tmp would only help in the second mode;
        // the first mode (CI) exits before the file is ever read.
        use cartog_core::{Edge, EdgeKind, Symbol, SymbolKind};
        use cartog_db::Database;

        let db = Database::open_memory().unwrap();
        let src = Symbol::new("caller", SymbolKind::Function, "a.py", 1, 5, 0, 100, None);
        db.insert_symbols(std::slice::from_ref(&src)).unwrap();
        let edge = Edge::new(&src.id, "find_user", EdgeKind::Calls, "a.py", 2);
        db.insert_edge(&edge).unwrap();
        let edge_id = db.unresolved_edges().unwrap()[0].edge_id;

        let tmp = tempfile::tempdir().unwrap();
        let ticks: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());
        let cb = |done, total| ticks.lock().unwrap().push((done, total));
        let stats =
            lsp_resolve_edges(&db, tmp.path(), None, &HashMap::new(), Some(&cb), None, 0).unwrap();
        // No server ran, so no edges were processed. The phase-start emit(0) and
        // the post-loop final emit(0) carry the same value; the dedup collapses
        // them to a single (0, 1) tick rather than a duplicate pair.
        assert_eq!(ticks.into_inner().unwrap(), vec![(0, 1)]);
        assert_eq!(stats.resolved, 0, "no servers must mean zero resolutions");
        assert_eq!(
            stats.marked_unresolvable, 0,
            "no servers must mean zero unresolvable marks"
        );
        assert_eq!(
            stats.marked_external, 0,
            "no servers must mean zero external marks"
        );
        assert_eq!(
            db.edge_resolution_state(edge_id).unwrap(),
            0,
            "edge must stay at state=0 when no LSP ran"
        );
    }

    #[test]
    fn cancel_probe_aborts_with_cancelled_error_and_marks_nothing() {
        use cartog_core::{Edge, EdgeKind, Symbol, SymbolKind};
        use cartog_db::Database;

        let db = Database::open_memory().unwrap();
        let src = Symbol::new("caller", SymbolKind::Function, "a.py", 1, 5, 0, 100, None);
        db.insert_symbols(std::slice::from_ref(&src)).unwrap();
        let edge = Edge::new(&src.id, "find_user", EdgeKind::Calls, "a.py", 2);
        db.insert_edge(&edge).unwrap();
        let edge_id = db.unresolved_edges().unwrap()[0].edge_id;

        let tmp = tempfile::tempdir().unwrap();
        let cancel = || true; // trip immediately at the first language boundary
        let err = lsp_resolve_edges(
            &db,
            tmp.path(),
            None,
            &HashMap::new(),
            None,
            Some(&cancel),
            0,
        )
        .expect_err("a tripped cancel probe must abort");
        assert!(
            err.to_string().contains("cancelled"),
            "error must mention cancellation, got: {err}"
        );
        assert_eq!(
            db.edge_resolution_state(edge_id).unwrap(),
            0,
            "a cancelled pass must not mark any edge"
        );
    }

    #[test]
    fn cancel_probe_returning_false_does_not_abort() {
        use cartog_core::{Edge, EdgeKind, Symbol, SymbolKind};
        use cartog_db::Database;

        let db = Database::open_memory().unwrap();
        let src = Symbol::new("caller", SymbolKind::Function, "a.py", 1, 5, 0, 100, None);
        db.insert_symbols(std::slice::from_ref(&src)).unwrap();
        let edge = Edge::new(&src.id, "find_user", EdgeKind::Calls, "a.py", 2);
        db.insert_edge(&edge).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let cancel = || false;
        // No server on PATH → returns Ok with zero resolutions (same as the
        // no-probe path); a non-tripping probe must not change that.
        let stats = lsp_resolve_edges(
            &db,
            tmp.path(),
            None,
            &HashMap::new(),
            None,
            Some(&cancel),
            0,
        )
        .expect("a non-cancelling probe must not abort");
        assert_eq!(stats.resolved, 0);
    }
}
