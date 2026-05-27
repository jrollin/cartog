//! LSP-based edge resolution for the cartog code graph.
//!
//! Resolves edges left unresolved by the heuristic resolver in [`cartog_db`],
//! by querying real language servers (pyright, rust-analyzer, etc.) for
//! `textDocument/definition` responses. Optional — gated behind the `lsp` feature.

pub mod client;
pub mod manager;
pub mod servers;

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use cartog_core::detect_language;
use cartog_db::{Database, UnresolvedEdge};

use manager::{DefinitionOutcome, LspManager};

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
}

/// Resolve edges that heuristic resolution left unresolved, using LSP servers.
///
/// If `shared_manager` is provided, reuses existing LSP servers (warm start).
/// Otherwise creates a temporary manager that is dropped after resolution.
///
/// Returns counts for `resolved` (state=0 → 1), `marked_unresolvable`
/// (state=0 → 2, definitive LSP negative), and `marked_external` (state=0 → 3,
/// LSP located the target outside the indexed root).
pub fn lsp_resolve_edges(
    db: &Database,
    root: &Path,
    shared_manager: Option<&mut LspManager>,
) -> Result<LspResolveStats> {
    let unresolved = db.unresolved_edges()?;

    if unresolved.is_empty() {
        return Ok(LspResolveStats::default());
    }

    // Group by language (derived from file extension)
    let mut by_language: HashMap<String, Vec<UnresolvedEdge>> = HashMap::new();
    for edge in unresolved {
        let path = Path::new(&edge.file_path);
        if let Some(lang) = detect_language(path) {
            by_language.entry(lang.to_string()).or_default().push(edge);
        }
    }

    if by_language.is_empty() {
        return Ok(LspResolveStats::default());
    }

    // Use shared manager if provided, otherwise create a temporary one
    let mut owned_manager;
    let manager: &mut LspManager = match shared_manager {
        Some(m) => {
            m.ensure_root(root);
            m
        }
        None => {
            owned_manager = LspManager::new(root);
            &mut owned_manager
        }
    };

    let mut resolved = 0u32;
    let mut marked_unresolvable = 0u32;
    let mut marked_external = 0u32;
    let mut any_server_started = false;

    for (language, edges) in &by_language {
        match manager.start(language) {
            Ok(()) => {
                any_server_started = true;
            }
            Err(e) => {
                tracing::info!("LSP: {language} — {e:#} ({} unresolved edges)", edges.len());
                continue;
            }
        }

        // Group edges by file for batched didOpen
        let mut by_file: HashMap<&str, Vec<&UnresolvedEdge>> = HashMap::new();
        for edge in edges {
            by_file.entry(&edge.file_path).or_default().push(edge);
        }

        tracing::info!(
            "LSP: resolving {} unresolved {language} edges across {} files...",
            edges.len(),
            by_file.len()
        );

        // Buffer "definitive negative" marks here and only commit them at the
        // end of the language loop *if* the language proved healthy by resolving
        // at least one edge. Catches half-loaded rust-analyzer cases where the
        // server returns Ok(None) or out-of-root locations before its index is
        // ready — without this gate we would burn good edges with sticky markers.
        let mut pending_unresolvable: Vec<i64> = Vec::new();
        let mut pending_external: Vec<i64> = Vec::new();
        let mut lang_resolved: u32 = 0;
        let mut server_died = false;

        for (file_path, file_edges) in by_file {
            let abs_path = root.join(file_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("cannot read {file_path}: {e}");
                    continue;
                }
            };

            if let Err(e) = manager.open_file(language, file_path, &content) {
                tracing::debug!("didOpen failed for {file_path}: {e:#}");
                if !manager.is_alive(language) {
                    tracing::warn!(
                        "{language} LSP server died during didOpen — remaining {language} edges \
                         resolved via heuristics only. Rerun with --no-lsp to skip LSP entirely."
                    );
                    server_died = true;
                    break;
                }
                continue;
            }

            let lines: Vec<&str> = content.lines().collect();

            for edge in file_edges {
                let col = match find_column_in_line(&lines, edge.line, &edge.target_name) {
                    Some(c) => c,
                    None => continue,
                };

                let lsp_line = edge.line.saturating_sub(1); // cartog 1-based → LSP 0-based

                match manager.definition(language, file_path, lsp_line, col) {
                    Ok(Some(DefinitionOutcome::InRoot(loc))) => {
                        match db.find_symbol_at_location(&loc.file_path, loc.line) {
                            Ok(Some(symbol_id)) => {
                                match db.update_edge_target(edge.edge_id, &symbol_id) {
                                    Ok(()) => {
                                        resolved += 1;
                                        lang_resolved += 1;
                                    }
                                    Err(e) => tracing::debug!(
                                        "failed to update edge {}: {e:#}",
                                        edge.edge_id
                                    ),
                                }
                            }
                            Ok(None) => {
                                // LSP located the target inside the root but
                                // cartog has no extracted symbol covering that
                                // line (cartog extraction gap, unindexed
                                // language, top-level statement between
                                // symbols). This is NOT external — the target
                                // is in-root. Treat as unresolvable so the
                                // state=3 "external" bucket stays semantically
                                // clean (stdlib/deps only).
                                tracing::debug!(
                                    "no cartog symbol at {}:{}",
                                    loc.file_path,
                                    loc.line
                                );
                                pending_unresolvable.push(edge.edge_id);
                            }
                            Err(e) => return Err(e), // DB errors propagate
                        }
                    }
                    Ok(Some(DefinitionOutcome::External)) => {
                        // LSP located the target outside the indexed root
                        // (stdlib, deps, node_modules). Buffer for state=3.
                        pending_external.push(edge.edge_id);
                    }
                    Ok(None) => {
                        // LSP definitively answered "no definition". Buffer it
                        // until we know the language server resolved at least
                        // one edge this run (per-language success gate).
                        pending_unresolvable.push(edge.edge_id);
                    }
                    Err(e) => {
                        // Transient: server crash, didOpen race, IO. NEVER mark
                        // — the marker is sticky and a transient failure must
                        // not burn this edge for future runs.
                        tracing::debug!(
                            "definition failed for {} at {file_path}:{}: {e:#}",
                            edge.target_name,
                            edge.line
                        );
                        if !manager.is_alive(language) {
                            tracing::warn!(
                                "{language} LSP server died — remaining {language} edges resolved \
                                 via heuristics only. Rerun with --no-lsp to skip LSP entirely."
                            );
                            server_died = true;
                            break;
                        }
                    }
                }
            }

            // Close the file to free server memory
            let _ = manager.close_file(language, file_path);

            if server_died {
                break;
            }
        }

        // Unresolvable marks come from `Ok(None)` — an answer a half-loaded
        // server can fabricate before its index is ready. Gate behind
        // `lang_resolved > 0` to avoid burning good edges as sticky state=2
        // when the server is unhealthy.
        if !server_died && lang_resolved > 0 {
            for edge_id in &pending_unresolvable {
                if let Err(e) = db.mark_edge_unresolvable(*edge_id) {
                    tracing::debug!("failed to mark edge {edge_id} unresolvable: {e:#}");
                    continue;
                }
                marked_unresolvable += 1;
            }
        } else if !pending_unresolvable.is_empty() {
            tracing::info!(
                "LSP: {language} produced {} unresolvable answers but no successes — \
                 not marking (server may be half-loaded or unhealthy)",
                pending_unresolvable.len(),
            );
        }

        // External marks come from positive LSP answers (a concrete URI
        // outside the indexed root). A half-loaded server cannot fabricate
        // those, so the lang_resolved gate is unnecessary. Commit whenever
        // the server stayed alive — a stdlib-only file would otherwise
        // re-query the LSP forever.
        if !server_died {
            for edge_id in &pending_external {
                if let Err(e) = db.mark_edge_external(*edge_id) {
                    tracing::debug!("failed to mark edge {edge_id} external: {e:#}");
                    continue;
                }
                marked_external += 1;
            }
        } else if !pending_external.is_empty() {
            tracing::info!(
                "LSP: {language} produced {} external answers but server died — \
                 not marking",
                pending_external.len(),
            );
        }
    }

    if !any_server_started {
        tracing::debug!("LSP: no servers found on PATH, skipping");
    } else if resolved > 0 || marked_unresolvable > 0 || marked_external > 0 {
        tracing::info!(
            "LSP: resolved {resolved} additional edges, \
             marked {marked_unresolvable} unresolvable, {marked_external} external"
        );
    } else {
        tracing::info!("LSP: no additional edges resolved");
    }

    // manager.shutdown_all() called via Drop
    Ok(LspResolveStats {
        resolved,
        marked_unresolvable,
        marked_external,
    })
}

/// Find the column (0-based UTF-16 offset) of `target_name` in the given source line.
/// Uses word-boundary matching to avoid matching inside longer identifiers.
/// LSP positions use UTF-16 code units by default.
fn find_column_in_line(lines: &[&str], line_1based: u32, target_name: &str) -> Option<u32> {
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
        let stats = lsp_resolve_edges(&db, tmp.path(), None).unwrap();
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
}
