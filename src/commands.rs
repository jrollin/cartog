use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{EdgeKindFilter, SymbolKindFilter};
use crate::db::{Database, DB_FILE, MAX_SEARCH_LIMIT};
use crate::indexer;
use crate::rag;
use crate::types::{EdgeKind, SymbolKind};
use crate::watch::{self, WatchConfig};

fn open_db() -> Result<Database> {
    Database::open(DB_FILE).context("Failed to open cartog database")
}

/// Estimate token count from a string using chars/4 approximation.
#[cfg(test)]
fn estimate_tokens(s: &str) -> u32 {
    (s.len() as u32).div_ceil(4)
}

/// Truncate a string to fit within a token budget, appending a truncation notice.
fn truncate_to_budget(s: &str, max_tokens: u32) -> String {
    let max_bytes = (max_tokens as usize) * 4;
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Find a char boundary at or before max_bytes, leaving room for notice
    let notice = "\n... (truncated to fit token budget)";
    let target = max_bytes.saturating_sub(notice.len());
    // UTF-8 chars are at most 4 bytes, so we only need to check 4 positions back.
    let cut = (target.saturating_sub(3)..=target)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    let mut out = s[..cut].to_string();
    out.push_str(notice);
    out
}

/// Print `data` as pretty JSON if `json` is true, otherwise call `human_fmt`.
/// When `token_budget` is Some, truncate human-readable output to fit.
fn output<T: Serialize>(
    data: &T,
    json: bool,
    token_budget: Option<u32>,
    human_fmt: impl FnOnce(&T) -> String,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(data)?);
    } else {
        let text = human_fmt(data);
        match token_budget {
            Some(budget) => print!("{}", truncate_to_budget(&text, budget)),
            None => print!("{}", text),
        }
    }
    Ok(())
}

/// Build or rebuild the code graph index.
pub fn cmd_index(path: &str, force: bool, json: bool) -> Result<()> {
    let root = Path::new(path);
    let db = open_db()?;

    let result = indexer::index_directory(&db, root, force)?;

    output(&result, json, None, |r| {
        format!(
            "Indexed {} files ({} skipped, {} removed)\n  {} symbols, {} edges ({} resolved)\n",
            r.files_indexed,
            r.files_skipped,
            r.files_removed,
            r.symbols_added,
            r.edges_added,
            r.edges_resolved
        )
    })
}

/// Show symbols and structure of a file.
pub fn cmd_outline(file: &str, json: bool, token_budget: Option<u32>) -> Result<()> {
    let db = open_db()?;
    let symbols = db.outline(file)?;
    let file = file.to_string();

    output(&symbols, json, token_budget, |syms| {
        if syms.is_empty() {
            return format!("No symbols found in {file}\n");
        }
        let mut out = String::new();
        for sym in syms {
            let indent = if sym.parent_id.is_some() { "  " } else { "" };
            let async_prefix = if sym.is_async { "async " } else { "" };
            match sym.kind {
                SymbolKind::Import => {
                    let text = sym.signature.as_deref().unwrap_or(&sym.name);
                    out.push_str(&format!("{indent}{text}  L{}\n", sym.start_line));
                }
                _ => {
                    let sig = sym.signature.as_deref().unwrap_or("");
                    out.push_str(&format!(
                        "{indent}{async_prefix}{kind} {name}{sig}  L{start}-{end}\n",
                        kind = sym.kind,
                        name = sym.name,
                        start = sym.start_line,
                        end = sym.end_line,
                    ));
                }
            }
        }
        out
    })
}

/// Find what a symbol calls.
pub fn cmd_callees(name: &str, json: bool, token_budget: Option<u32>) -> Result<()> {
    let db = open_db()?;
    let edges = db.callees(name)?;
    let name = name.to_string();

    output(&edges, json, token_budget, |edges| {
        if edges.is_empty() {
            return format!("No callees found for '{name}'\n");
        }
        let mut out = String::new();
        for edge in edges {
            out.push_str(&format!(
                "{target}  {file}:{line}\n",
                target = edge.target_name,
                file = edge.file_path,
                line = edge.line,
            ));
        }
        out
    })
}

/// Transitive impact analysis — what breaks if this changes?
pub fn cmd_impact(name: &str, depth: u32, json: bool, token_budget: Option<u32>) -> Result<()> {
    let db = open_db()?;
    let results = db.impact(name, depth)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct ImpactEntry {
        edge: crate::types::Edge,
        depth: u32,
    }

    let items: Vec<ImpactEntry> = results
        .into_iter()
        .map(|(edge, d)| ImpactEntry { edge, depth: d })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!("No impact found for '{name}'\n");
        }
        let mut out = String::new();
        for entry in items {
            let indent = "  ".repeat(entry.depth as usize);
            out.push_str(&format!(
                "{indent}{kind}  {source}  {file}:{line}\n",
                kind = entry.edge.kind,
                source = entry.edge.source_id,
                file = entry.edge.file_path,
                line = entry.edge.line,
            ));
        }
        out
    })
}

/// All references to a symbol (calls, imports, inherits, references, raises).
pub fn cmd_refs(
    name: &str,
    kind: Option<EdgeKindFilter>,
    json: bool,
    token_budget: Option<u32>,
) -> Result<()> {
    let db = open_db()?;
    let kind_filter = kind.map(EdgeKind::from);
    let results = db.refs(name, kind_filter)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct RefEntry {
        edge: crate::types::Edge,
        source: Option<crate::types::Symbol>,
    }

    let items: Vec<RefEntry> = results
        .into_iter()
        .map(|(edge, sym)| RefEntry { edge, source: sym })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!("No references found for '{name}'\n");
        }
        let mut out = String::new();
        for entry in items {
            let source_name = entry
                .source
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or(&entry.edge.source_id);
            out.push_str(&format!(
                "{kind}  {source}  {file}:{line}\n",
                kind = entry.edge.kind,
                source = source_name,
                file = entry.edge.file_path,
                line = entry.edge.line,
            ));
        }
        out
    })
}

/// Show inheritance hierarchy for a class.
pub fn cmd_hierarchy(name: &str, json: bool, token_budget: Option<u32>) -> Result<()> {
    let db = open_db()?;
    let pairs = db.hierarchy(name)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct HierarchyEntry {
        child: String,
        parent: String,
    }

    let items: Vec<HierarchyEntry> = pairs
        .into_iter()
        .map(|(child, parent)| HierarchyEntry { child, parent })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!("No hierarchy found for '{name}'\n");
        }
        let mut out = String::new();
        for entry in items {
            out.push_str(&format!("{} -> {}\n", entry.child, entry.parent));
        }
        out
    })
}

/// File-level import dependencies.
pub fn cmd_deps(file: &str, json: bool, token_budget: Option<u32>) -> Result<()> {
    let db = open_db()?;
    let edges = db.file_deps(file)?;
    let file = file.to_string();

    output(&edges, json, token_budget, |edges| {
        if edges.is_empty() {
            return format!("No dependencies found for '{file}'\n");
        }
        let mut out = String::new();
        for edge in edges {
            out.push_str(&format!(
                "{target}  L{line}\n",
                target = edge.target_name,
                line = edge.line
            ));
        }
        out
    })
}

/// Search for symbols by name (case-insensitive prefix + substring match).
pub fn cmd_search(
    query: &str,
    kind: Option<SymbolKindFilter>,
    file: Option<&str>,
    limit: u32,
    json: bool,
    token_budget: Option<u32>,
) -> Result<()> {
    let db = open_db()?;
    let kind_filter = kind.map(crate::types::SymbolKind::from);
    let limit = limit.min(MAX_SEARCH_LIMIT);
    let symbols = db.search(query, kind_filter, file, limit)?;
    let query = query.to_string();

    output(&symbols, json, token_budget, |syms| {
        if syms.is_empty() {
            return format!("No symbols found matching '{query}'\n");
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

/// Index statistics summary.
pub fn cmd_stats(json: bool) -> Result<()> {
    let db = open_db()?;
    let stats = db.stats()?;

    output(&stats, json, None, |stats| {
        let mut out = String::new();
        out.push_str(&format!("Files:    {}\n", stats.num_files));
        out.push_str(&format!("Symbols:  {}\n", stats.num_symbols));
        out.push_str(&format!(
            "Edges:    {} ({} resolved)\n",
            stats.num_edges, stats.num_resolved
        ));
        if !stats.languages.is_empty() {
            out.push_str("Languages:\n");
            for (lang, count) in &stats.languages {
                out.push_str(&format!("  {lang}: {count} files\n"));
            }
        }
        if !stats.symbol_kinds.is_empty() {
            out.push_str("Symbols by kind:\n");
            for (kind, count) in &stats.symbol_kinds {
                out.push_str(&format!("  {kind}: {count}\n"));
            }
        }
        out
    })
}

/// Token-budget-aware codebase summary: file tree + top symbols ranked by centrality.
pub fn cmd_map(tokens: u32, json: bool) -> Result<()> {
    let db = open_db()?;
    let files = db.all_files()?;

    if files.is_empty() {
        if json {
            println!("{{}}");
        } else {
            println!("No files indexed. Run 'cartog index .' first.");
        }
        return Ok(());
    }

    if json {
        // For JSON, return structured data without budget constraints
        let symbols = db.top_symbols(200)?;

        #[derive(Serialize)]
        struct MapResult {
            files: Vec<String>,
            top_symbols: Vec<crate::types::Symbol>,
        }

        let result = MapResult {
            files,
            top_symbols: symbols,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    // Human-readable: build file tree, then fill remaining budget with symbols
    let budget_bytes = (tokens as usize) * 4;

    // Phase 1: file tree
    let mut out = String::new();
    out.push_str(&format!("# Codebase Map ({} files)\n\n", files.len()));
    for file in &files {
        out.push_str(&format!("  {file}\n"));
    }

    let tree_bytes = out.len();
    let remaining = budget_bytes.saturating_sub(tree_bytes);

    if remaining < 100 {
        print!("{}", truncate_to_budget(&out, tokens));
        return Ok(());
    }

    // Phase 2: top symbols by centrality, grouped by file
    out.push_str("\n# Top Symbols (by reference count)\n\n");

    let symbols = db.top_symbols(500)?;
    let mut current_file = "";

    for sym in &symbols {
        if out.len() >= budget_bytes {
            break;
        }

        if sym.file_path != current_file {
            let header = format!("\n{}:\n", sym.file_path);
            if out.len() + header.len() > budget_bytes {
                break;
            }
            out.push_str(&header);
            current_file = &sym.file_path;
        }

        let sig = sym.signature.as_deref().unwrap_or("");
        let line = format!(
            "  {kind} {name}{sig}  L{start}-{end}  ({refs} refs)\n",
            kind = sym.kind,
            name = sym.name,
            start = sym.start_line,
            end = sym.end_line,
            refs = sym.in_degree,
        );

        if out.len() + line.len() > budget_bytes {
            break;
        }
        out.push_str(&line);
    }

    print!("{out}");
    Ok(())
}

/// Show symbols affected by recent git changes.
pub fn cmd_changes(
    commits: u32,
    kind: Option<SymbolKindFilter>,
    json: bool,
    token_budget: Option<u32>,
) -> Result<()> {
    let db = open_db()?;
    let root = std::env::current_dir()?;

    let changed_files = indexer::git_recently_changed_files(&root, commits)?;

    if changed_files.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No files changed in the last {commits} commits.");
        }
        return Ok(());
    }

    let kind_filter = kind.map(crate::types::SymbolKind::from);
    let symbols = db.symbols_for_files(&changed_files, kind_filter)?;

    let result = crate::types::ChangesResult {
        changed_files,
        symbols,
    };

    output(&result, json, token_budget, |r| {
        let mut out = format!(
            "{} files changed in last {} commits, {} symbols affected\n\n",
            r.changed_files.len(),
            commits,
            r.symbols.len()
        );
        let mut current_file = "";
        for sym in &r.symbols {
            if sym.file_path != current_file {
                current_file = &sym.file_path;
                out.push_str(&format!("{current_file}:\n"));
            }
            let sig = sym.signature.as_deref().unwrap_or("");
            out.push_str(&format!(
                "  {kind} {name}{sig}  L{start}-{end}\n",
                kind = sym.kind,
                name = sym.name,
                start = sym.start_line,
                end = sym.end_line,
            ));
        }
        let files_with_symbols: std::collections::HashSet<&str> =
            r.symbols.iter().map(|s| s.file_path.as_str()).collect();
        let unindexed: Vec<_> = r
            .changed_files
            .iter()
            .filter(|f| !files_with_symbols.contains(f.as_str()))
            .collect();
        if !unindexed.is_empty() {
            out.push_str(&format!(
                "\n{} changed files not in index:\n",
                unindexed.len()
            ));
            for f in unindexed {
                out.push_str(&format!("  {f}\n"));
            }
        }
        out
    })
}

// ── RAG Commands ──

/// Download the embedding model.
pub fn cmd_rag_setup(json: bool) -> Result<()> {
    // Download bi-encoder (embeddings)
    let embed_result = rag::setup::download_model()?;
    // Download cross-encoder (re-ranking)
    let rerank_result = rag::setup::download_cross_encoder()?;

    #[derive(serde::Serialize)]
    struct CombinedSetup {
        embedding: rag::setup::SetupResult,
        reranker: rag::setup::SetupResult,
    }

    let combined = CombinedSetup {
        embedding: embed_result,
        reranker: rerank_result,
    };

    output(&combined, json, None, |c| {
        format!(
            "Embedding model: {}\nRe-ranker model: {}\nModels ready. You can now run 'cartog rag index'.\n",
            c.embedding.model_dir, c.reranker.model_dir
        )
    })
}

/// Build embedding index for semantic search.
pub fn cmd_rag_index(path: &str, force: bool, json: bool) -> Result<()> {
    // First ensure the standard code graph index is up to date
    let root = Path::new(path);
    let db = open_db()?;
    let _index_result = indexer::index_directory(&db, root, false)?;

    let result = rag::indexer::index_embeddings(&db, force)?;

    output(&result, json, None, |r| {
        format!(
            "Embedded {} symbols ({} skipped, {} total with content)\n",
            r.symbols_embedded, r.symbols_skipped, r.total_content_symbols
        )
    })
}

/// Semantic search over code symbols.
pub fn cmd_rag_search(
    query: &str,
    kind: Option<SymbolKindFilter>,
    limit: u32,
    json: bool,
    token_budget: Option<u32>,
) -> Result<()> {
    let db = open_db()?;
    let kind_filter = kind.map(crate::types::SymbolKind::from);

    let search_result = rag::search::hybrid_search(&db, query, limit, kind_filter)?;
    let query = query.to_string();

    output(&search_result, json, token_budget, |sr| {
        if sr.results.is_empty() {
            let mut out = format!("No results found for '{query}'\n");
            if sr.fts_count == 0 && sr.vec_count == 0 {
                out.push_str("Hint: run 'cartog rag index' to build the semantic search index.\n");
            }
            return out;
        }
        let mut out = format!(
            "Found {} results (FTS: {}, vector: {}, merged: {})\n\n",
            sr.results.len(),
            sr.fts_count,
            sr.vec_count,
            sr.merged_count
        );
        for (i, r) in sr.results.iter().enumerate() {
            let sources = r.sources.join("+");
            let rerank_str = r
                .rerank_score
                .map(|s| format!(" rerank={s:.2}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "{}. {} {}  {}:{}-{}  [{}] score={:.4}{rerank_str}\n",
                i + 1,
                r.symbol.kind,
                r.symbol.name,
                r.symbol.file_path,
                r.symbol.start_line,
                r.symbol.end_line,
                sources,
                r.rrf_score,
            ));
            if let Some(ref content) = r.content {
                let preview: String = content
                    .lines()
                    .take(3)
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                out.push_str(&format!("{preview}\n\n"));
            }
        }
        out
    })
}

/// Watch for file changes and auto-re-index.
pub fn cmd_watch(path: &str, debounce: u64, rag: bool, rag_delay: u64) -> Result<()> {
    let mut config = WatchConfig::new(PathBuf::from(path));
    config.debounce = Duration::from_secs(debounce);
    config.rag = rag;
    config.rag_delay = Duration::from_secs(rag_delay);

    watch::run_watch(config, DB_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn test_truncate_to_budget_within_limit() {
        let text = "short text";
        let result = truncate_to_budget(text, 100);
        assert_eq!(result, text);
    }

    #[test]
    fn test_truncate_to_budget_exceeds_limit() {
        let text = "a".repeat(200);
        let result = truncate_to_budget(&text, 10);
        assert!(result.len() <= 40 + 50); // budget bytes + notice
        assert!(result.ends_with("... (truncated to fit token budget)"));
    }

    #[test]
    fn test_truncate_to_budget_exact_boundary() {
        let text = "abcd"; // 4 bytes = 1 token
        let result = truncate_to_budget(text, 1);
        assert_eq!(result, "abcd");
    }

    #[test]
    fn test_truncate_to_budget_unicode() {
        // Each emoji is 4 bytes
        let text = "Hello 🌍🌍🌍🌍🌍🌍🌍🌍🌍🌍";
        let result = truncate_to_budget(text, 5);
        assert!(result.ends_with("... (truncated to fit token budget)"));
        // Should not panic on char boundary issues
    }
}
