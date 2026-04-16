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

use manager::LspManager;

/// Resolve edges that heuristic resolution left unresolved, using LSP servers.
///
/// If `shared_manager` is provided, reuses existing LSP servers (warm start).
/// Otherwise creates a temporary manager that is dropped after resolution.
///
/// Returns the number of edges resolved by LSP.
pub fn lsp_resolve_edges(
    db: &Database,
    root: &Path,
    shared_manager: Option<&mut LspManager>,
) -> Result<u32> {
    let unresolved = db.unresolved_edges()?;

    if unresolved.is_empty() {
        return Ok(0);
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
        return Ok(0);
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
                    tracing::warn!("{language} server died during didOpen");
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
                    Ok(Some(loc)) => {
                        match db.find_symbol_at_location(&loc.file_path, loc.line) {
                            Ok(Some(symbol_id)) => {
                                match db.update_edge_target(edge.edge_id, &symbol_id) {
                                    Ok(()) => resolved += 1,
                                    Err(e) => tracing::debug!(
                                        "failed to update edge {}: {e:#}",
                                        edge.edge_id
                                    ),
                                }
                            }
                            Ok(None) => {
                                tracing::debug!(
                                    "no cartog symbol at {}:{}",
                                    loc.file_path,
                                    loc.line
                                );
                            }
                            Err(e) => return Err(e.into()), // DB errors propagate
                        }
                    }
                    Ok(None) => {} // LSP couldn't resolve either
                    Err(e) => {
                        tracing::debug!(
                            "definition failed for {} at {file_path}:{}: {e:#}",
                            edge.target_name,
                            edge.line
                        );
                        if !manager.is_alive(language) {
                            tracing::warn!("{language} server died, skipping remaining edges");
                            break;
                        }
                    }
                }
            }

            // Close the file to free server memory
            let _ = manager.close_file(language, file_path);
        }
    }

    if !any_server_started {
        tracing::debug!("LSP: no servers found on PATH, skipping");
    } else if resolved > 0 {
        tracing::info!("LSP: resolved {resolved} additional edges");
    } else {
        tracing::info!("LSP: no additional edges resolved");
    }

    // manager.shutdown_all() called via Drop
    Ok(resolved)
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
}
