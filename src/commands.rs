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

/// Print `data` as pretty JSON if `json` is true, otherwise call `human_fmt`.
fn output<T: Serialize>(data: &T, json: bool, human_fmt: impl FnOnce(&T)) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(data)?);
    } else {
        human_fmt(data);
    }
    Ok(())
}

/// Build or rebuild the code graph index.
pub fn cmd_index(path: &str, force: bool, json: bool) -> Result<()> {
    let root = Path::new(path);
    let db = open_db()?;

    let result = indexer::index_directory(&db, root, force)?;

    output(&result, json, |r| {
        println!(
            "Indexed {} files ({} skipped, {} removed)",
            r.files_indexed, r.files_skipped, r.files_removed
        );
        println!(
            "  {} symbols, {} edges ({} resolved)",
            r.symbols_added, r.edges_added, r.edges_resolved
        );
    })
}

/// Show symbols and structure of a file.
pub fn cmd_outline(file: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let symbols = db.outline(file)?;

    output(&symbols, json, |syms| {
        if syms.is_empty() {
            println!("No symbols found in {file}");
            return;
        }
        for sym in syms {
            let indent = if sym.parent_id.is_some() { "  " } else { "" };
            let async_prefix = if sym.is_async { "async " } else { "" };
            match sym.kind {
                SymbolKind::Import => {
                    let text = sym.signature.as_deref().unwrap_or(&sym.name);
                    println!("{indent}{text}  L{}", sym.start_line);
                }
                _ => {
                    let sig = sym.signature.as_deref().unwrap_or("");
                    println!(
                        "{indent}{async_prefix}{kind} {name}{sig}  L{start}-{end}",
                        kind = sym.kind,
                        name = sym.name,
                        start = sym.start_line,
                        end = sym.end_line,
                    );
                }
            }
        }
    })
}

/// Find what a symbol calls.
pub fn cmd_callees(name: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let edges = db.callees(name)?;

    output(&edges, json, |edges| {
        if edges.is_empty() {
            println!("No callees found for '{name}'");
            return;
        }
        for edge in edges {
            println!(
                "{target}  {file}:{line}",
                target = edge.target_name,
                file = edge.file_path,
                line = edge.line,
            );
        }
    })
}

/// Transitive impact analysis — what breaks if this changes?
pub fn cmd_impact(name: &str, depth: u32, json: bool) -> Result<()> {
    let db = open_db()?;
    let results = db.impact(name, depth)?;

    if json {
        let items: Vec<_> = results
            .iter()
            .map(|(edge, d)| {
                serde_json::json!({
                    "edge": edge,
                    "depth": d,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if results.is_empty() {
            println!("No impact found for '{name}'");
            return Ok(());
        }
        for (edge, depth) in &results {
            let indent = "  ".repeat(*depth as usize);
            println!(
                "{indent}{kind}  {source}  {file}:{line}",
                kind = edge.kind,
                source = edge.source_id,
                file = edge.file_path,
                line = edge.line,
            );
        }
    }

    Ok(())
}

/// All references to a symbol (calls, imports, inherits, references, raises).
pub fn cmd_refs(name: &str, kind: Option<EdgeKindFilter>, json: bool) -> Result<()> {
    let db = open_db()?;
    let kind_filter = kind.map(EdgeKind::from);
    let results = db.refs(name, kind_filter)?;

    if json {
        let items: Vec<_> = results
            .iter()
            .map(|(edge, sym)| {
                serde_json::json!({
                    "edge": edge,
                    "source": sym,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if results.is_empty() {
            println!("No references found for '{name}'");
            return Ok(());
        }
        for (edge, sym) in &results {
            let source_name = sym
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or(&edge.source_id);
            println!(
                "{kind}  {source}  {file}:{line}",
                kind = edge.kind,
                source = source_name,
                file = edge.file_path,
                line = edge.line,
            );
        }
    }

    Ok(())
}

/// Show inheritance hierarchy for a class.
pub fn cmd_hierarchy(name: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let pairs = db.hierarchy(name)?;

    if json {
        let items: Vec<_> = pairs
            .iter()
            .map(|(child, parent)| {
                serde_json::json!({
                    "child": child,
                    "parent": parent,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if pairs.is_empty() {
            println!("No hierarchy found for '{name}'");
            return Ok(());
        }
        for (child, parent) in &pairs {
            println!("{child} -> {parent}");
        }
    }

    Ok(())
}

/// File-level import dependencies.
pub fn cmd_deps(file: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let edges = db.file_deps(file)?;

    output(&edges, json, |edges| {
        if edges.is_empty() {
            println!("No dependencies found for '{file}'");
            return;
        }
        for edge in edges {
            println!(
                "{target}  L{line}",
                target = edge.target_name,
                line = edge.line
            );
        }
    })
}

/// Search for symbols by name (case-insensitive prefix + substring match).
pub fn cmd_search(
    query: &str,
    kind: Option<SymbolKindFilter>,
    file: Option<&str>,
    limit: u32,
    json: bool,
) -> Result<()> {
    let db = open_db()?;
    let kind_filter = kind.map(crate::types::SymbolKind::from);
    let limit = limit.min(MAX_SEARCH_LIMIT);
    let symbols = db.search(query, kind_filter, file, limit)?;

    output(&symbols, json, |syms| {
        if syms.is_empty() {
            println!("No symbols found matching '{query}'");
            return;
        }
        for sym in syms {
            println!(
                "{kind}  {name}  {file}:{line}",
                kind = sym.kind,
                name = sym.name,
                file = sym.file_path,
                line = sym.start_line,
            );
        }
    })
}

/// Index statistics summary.
pub fn cmd_stats(json: bool) -> Result<()> {
    let db = open_db()?;
    let stats = db.stats()?;

    output(&stats, json, |stats| {
        println!("Files:    {}", stats.num_files);
        println!("Symbols:  {}", stats.num_symbols);
        println!(
            "Edges:    {} ({} resolved)",
            stats.num_edges, stats.num_resolved
        );
        if !stats.languages.is_empty() {
            println!("Languages:");
            for (lang, count) in &stats.languages {
                println!("  {lang}: {count} files");
            }
        }
        if !stats.symbol_kinds.is_empty() {
            println!("Symbols by kind:");
            for (kind, count) in &stats.symbol_kinds {
                println!("  {kind}: {count}");
            }
        }
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

    output(&combined, json, |c| {
        println!("Embedding model: {}", c.embedding.model_dir);
        println!("Re-ranker model: {}", c.reranker.model_dir);
        println!("Models ready. You can now run 'cartog rag index'.");
    })
}

/// Build embedding index for semantic search.
pub fn cmd_rag_index(path: &str, force: bool, json: bool) -> Result<()> {
    // First ensure the standard code graph index is up to date
    let root = Path::new(path);
    let db = open_db()?;
    let _index_result = indexer::index_directory(&db, root, false)?;

    let result = rag::indexer::index_embeddings(&db, force)?;

    output(&result, json, |r| {
        println!(
            "Embedded {} symbols ({} skipped, {} total with content)",
            r.symbols_embedded, r.symbols_skipped, r.total_content_symbols
        );
    })
}

/// Semantic search over code symbols.
pub fn cmd_rag_search(
    query: &str,
    kind: Option<SymbolKindFilter>,
    limit: u32,
    json: bool,
) -> Result<()> {
    let db = open_db()?;
    let kind_filter = kind.map(crate::types::SymbolKind::from);

    let search_result = rag::search::hybrid_search(&db, query, limit, kind_filter)?;

    output(&search_result, json, |sr| {
        if sr.results.is_empty() {
            println!("No results found for '{query}'");
            if sr.fts_count == 0 && sr.vec_count == 0 {
                println!("Hint: run 'cartog rag index' to build the semantic search index.");
            }
            return;
        }
        println!(
            "Found {} results (FTS: {}, vector: {}, merged: {})\n",
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
            println!(
                "{}. {} {}  {}:{}-{}  [{}] score={:.4}{rerank_str}",
                i + 1,
                r.symbol.kind,
                r.symbol.name,
                r.symbol.file_path,
                r.symbol.start_line,
                r.symbol.end_line,
                sources,
                r.rrf_score,
            );
            if let Some(ref content) = r.content {
                // Show first 3 lines of content as preview
                let preview: String = content
                    .lines()
                    .take(3)
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                println!("{preview}\n");
            }
        }
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
