//! `cartog search` — name-based symbol lookup (case-insensitive prefix +
//! substring match). Semantic/RAG search lives in [`super::rag`].

use std::path::Path;

use anyhow::Result;

use super::shared::{empty_index_hint, open_db, output};
use crate::cli::SymbolKindFilter;
use cartog_core::Compact;
use cartog_db::MAX_SEARCH_LIMIT;

/// Search for symbols by name (case-insensitive prefix + substring match).
#[allow(clippy::too_many_arguments)]
pub fn cmd_search(
    db_path: &Path,
    query: &str,
    kind: Option<SymbolKindFilter>,
    file: Option<&str>,
    limit: u32,
    json: bool,
    compact: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let kind_filter = match kind {
        Some(SymbolKindFilter::All) | None => None,
        Some(k) => Some(cartog_core::SymbolKind::from(k)),
    };
    let limit = limit.min(MAX_SEARCH_LIMIT);
    let mut symbols = db.search(query, kind_filter, file, limit)?;
    if !symbols.is_empty() {
        db.log_query("search", "cli");
    }
    if compact {
        symbols.compact_in_place();
    }
    let query = query.to_string();

    output(&symbols, json, token_budget, |syms| {
        if syms.is_empty() {
            return format!(
                "No symbols found matching '{query}'{}\n",
                empty_index_hint(&db)
            );
        }
        let mut out = String::new();
        for sym in syms {
            out.push_str(&format!(
                "{kind}  {name}  {file}:{line}\n",
                kind = sym.kind,
                name = sym.name,
                file = sym.file_path,
                line = sym.start_line,
            ));
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{indexed_db, queries_logged};
    use super::*;

    #[test]
    fn cmd_search_runs_a_query_for_each_filter_and_budget_branch() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_search(&db, "Anim", None, None, 30, false, false, None, 384).expect("search ok");
        cmd_search(
            &db,
            "speak",
            Some(SymbolKindFilter::Method),
            Some("lib.py"),
            30,
            false,
            false,
            None,
            384,
        )
        .expect("search with kind + file filter ok");
        // Token-budget branch.
        cmd_search(&db, "e", None, None, 30, false, false, Some(50), 384)
            .expect("search --tokens ok");
        assert_eq!(
            queries_logged(&db),
            before + 3,
            "each search invocation must run a query"
        );
    }

    #[test]
    fn cmd_search_empty_result_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_search(&db, "zzz_no_match", None, None, 30, false, false, None, 384)
            .expect("empty search is ok");
    }

    #[test]
    fn cmd_search_json_compact_branch_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_search(&db, "Anim", None, None, 30, true, true, None, 384)
            .expect("search --json --compact ok");
    }
}
