use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{EdgeKindFilter, SymbolKindFilter};
use crate::config::CartogConfig;
use cartog_core::{EdgeKind, SymbolKind};
use cartog_db::{Database, MAX_SEARCH_LIMIT};
use cartog_indexer as indexer;
use cartog_rag as rag;
use cartog_watch::{self as watch, WatchConfig};

/// Stderr spinner for long-running CLI commands.
///
/// No-op in non-TTY contexts (piped output, CI logs) so logs stay clean.
struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    fn start(label: &'static str) -> Option<Self> {
        if !std::io::stderr().is_terminal() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0usize;
            let start = std::time::Instant::now();
            while !stop_clone.load(Ordering::Relaxed) {
                let elapsed = start.elapsed().as_secs();
                let mut err = std::io::stderr().lock();
                // \r + clear-to-eol + frame + label + elapsed
                let _ = write!(err, "\r\x1b[K{} {} ({elapsed}s)", FRAMES[i], label);
                let _ = err.flush();
                i = (i + 1) % FRAMES.len();
                std::thread::sleep(Duration::from_millis(100));
            }
            // Clear the spinner line on exit.
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[K");
            let _ = err.flush();
        });
        Some(Self {
            stop,
            handle: Some(handle),
        })
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn open_db(path: &Path, embedding_dim: usize) -> Result<Database> {
    Database::open(path, embedding_dim).context("Failed to open cartog database")
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
pub fn cmd_index(
    db_path: &Path,
    path: &str,
    force: bool,
    lsp: bool,
    json: bool,
    embedding_dim: usize,
) -> Result<()> {
    let root = Path::new(path);
    let db = open_db(db_path, embedding_dim)?;

    let spinner = if json {
        None
    } else {
        Spinner::start("Indexing")
    };
    let result = indexer::index_directory(&db, root, force, lsp);
    if let Some(s) = spinner {
        s.stop();
    }
    let result = result?;

    output(&result, json, None, |r| {
        let lsp_part = if r.edges_lsp_resolved > 0 {
            format!(
                " ({} heuristic + {} LSP)",
                r.edges_resolved, r.edges_lsp_resolved
            )
        } else {
            String::new()
        };
        let sym_detail = if r.symbols_modified > 0 || r.symbols_unchanged > 0 {
            format!(
                " ({} new, {} modified, {} unchanged, {} removed)",
                r.symbols_added, r.symbols_modified, r.symbols_unchanged, r.symbols_removed
            )
        } else {
            String::new()
        };
        format!(
            "Indexed {} files ({} skipped, {} removed)\n  {} symbols{}, {} edges ({} resolved{})\n",
            r.files_indexed,
            r.files_skipped,
            r.files_removed,
            r.symbols_added + r.symbols_modified + r.symbols_unchanged,
            sym_detail,
            r.edges_added,
            r.edges_resolved + r.edges_lsp_resolved,
            lsp_part,
        )
    })
}

/// Show symbols and structure of a file.
pub fn cmd_outline(
    db_path: &Path,
    file: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
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
pub fn cmd_callees(
    db_path: &Path,
    name: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
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
pub fn cmd_impact(
    db_path: &Path,
    name: &str,
    depth: u32,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let results = db.impact(name, depth)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct ImpactEntry {
        edge: cartog_core::Edge,
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
    db_path: &Path,
    name: &str,
    kind: Option<EdgeKindFilter>,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let kind_filter = kind.map(EdgeKind::from);
    let results = db.refs(name, kind_filter)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct RefEntry {
        edge: cartog_core::Edge,
        source: Option<cartog_core::Symbol>,
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
pub fn cmd_hierarchy(
    db_path: &Path,
    name: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
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
pub fn cmd_deps(
    db_path: &Path,
    file: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
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
#[allow(clippy::too_many_arguments)]
pub fn cmd_search(
    db_path: &Path,
    query: &str,
    kind: Option<SymbolKindFilter>,
    file: Option<&str>,
    limit: u32,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let kind_filter = match kind {
        Some(SymbolKindFilter::All) | None => None,
        Some(k) => Some(cartog_core::SymbolKind::from(k)),
    };
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
pub fn cmd_stats(db_path: &Path, json: bool, embedding_dim: usize) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
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
pub fn cmd_map(db_path: &Path, tokens: u32, json: bool, embedding_dim: usize) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
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
            top_symbols: Vec<cartog_core::Symbol>,
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
    db_path: &Path,
    commits: u32,
    kind: Option<SymbolKindFilter>,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
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

    let kind_filter = match kind {
        Some(SymbolKindFilter::All) | None => None,
        Some(k) => Some(cartog_core::SymbolKind::from(k)),
    };
    let symbols = db.symbols_for_files(&changed_files, kind_filter)?;

    let result = cartog_core::ChangesResult {
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
    let spinner = if json {
        None
    } else {
        Spinner::start("Downloading models")
    };
    // Download bi-encoder (embeddings)
    let embed_result = rag::setup::download_model();
    // Download cross-encoder (re-ranking)
    let rerank_result = rag::setup::download_cross_encoder();
    if let Some(s) = spinner {
        s.stop();
    }
    let embed_result = embed_result?;
    let rerank_result = rerank_result?;

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
pub fn cmd_rag_index(
    db_path: &Path,
    path: &str,
    force: bool,
    json: bool,
    provider_config: &rag::EmbeddingProviderConfig,
) -> Result<()> {
    let root = Path::new(path);
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;

    let spinner = if json {
        None
    } else {
        Spinner::start("Indexing code graph")
    };
    let index_res = indexer::index_directory(&db, root, false, false);
    if let Some(s) = spinner {
        s.stop();
    }
    let _index_result = index_res?;

    let spinner = if json {
        None
    } else {
        Spinner::start("Embedding symbols")
    };
    let embed_res = rag::indexer::index_embeddings(&db, provider.as_mut(), force);
    if let Some(s) = spinner {
        s.stop();
    }
    let result = embed_res?;

    output(&result, json, None, |r| {
        format!(
            "Embedded {} symbols ({} skipped, {} total with content)\n",
            r.symbols_embedded, r.symbols_skipped, r.total_content_symbols
        )
    })
}

/// Semantic search over code symbols.
#[allow(clippy::too_many_arguments)]
pub fn cmd_rag_search(
    db_path: &Path,
    query: &str,
    kind: Option<SymbolKindFilter>,
    limit: u32,
    json: bool,
    token_budget: Option<u32>,
    provider_config: &rag::EmbeddingProviderConfig,
    tuning: &rag::search::SearchTuning,
) -> Result<()> {
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;
    let kind_filter = match kind {
        Some(SymbolKindFilter::All) => rag::search::KindFilter::All,
        Some(k) => rag::search::KindFilter::Exact(cartog_core::SymbolKind::from(k)),
        None => rag::search::KindFilter::CodeOnly,
    };

    let mut reranker = rag::create_reranker_provider(&provider_config.reranker_provider);
    let search_result = match reranker.as_mut() {
        Some(r) => rag::search::hybrid_search_tuned(
            &db,
            query,
            limit,
            kind_filter,
            provider.as_mut(),
            Some(r.as_mut()),
            tuning,
        ),
        None => rag::search::hybrid_search_tuned(
            &db,
            query,
            limit,
            kind_filter,
            provider.as_mut(),
            None,
            tuning,
        ),
    }?;
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

/// Display the current configuration with default-value indicators.
pub fn cmd_config(
    config: &CartogConfig,
    config_path: Option<&Path>,
    db_path: &Path,
    json: bool,
) -> Result<()> {
    use crate::config::{
        DEFAULT_EMBEDDING_PROVIDER, DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_MODEL,
        DEFAULT_RERANKER_PROVIDER,
    };

    let embed = config.embedding.as_ref();
    let ollama = embed.and_then(|e| e.ollama.as_ref());
    let local = embed.and_then(|e| e.local.as_ref());
    let reranker = config.reranker.as_ref();

    let display = ConfigDisplay {
        config_file: config_path.map(|p| p.to_string_lossy().into_owned()),
        db_path: db_path.to_string_lossy().into_owned(),
        embedding: EmbeddingDisplay {
            provider: ValueDisplay {
                value: embed.map_or(DEFAULT_EMBEDDING_PROVIDER.into(), |e| {
                    e.provider().to_string()
                }),
                is_default: embed.map_or(true, |e| e.provider.is_none()),
                default: DEFAULT_EMBEDDING_PROVIDER.into(),
            },
            model: embed.and_then(|e| e.model.clone()),
            dimension: embed.and_then(|e| e.dimension),
            local: LocalEmbeddingDisplay {
                query_prefix: local.and_then(|l| l.query_prefix.clone()),
                document_prefix: local.and_then(|l| l.document_prefix.clone()),
            },
            ollama: OllamaDisplay {
                base_url: ValueDisplay {
                    value: ollama
                        .map_or(DEFAULT_OLLAMA_BASE_URL.into(), |o| o.base_url().to_string()),
                    is_default: ollama.map_or(true, |o| o.base_url.is_none()),
                    default: DEFAULT_OLLAMA_BASE_URL.into(),
                },
                model: ValueDisplay {
                    value: ollama.map_or(DEFAULT_OLLAMA_MODEL.into(), |o| o.model().to_string()),
                    is_default: ollama.map_or(true, |o| o.model.is_none()),
                    default: DEFAULT_OLLAMA_MODEL.into(),
                },
            },
        },
        reranker: RerankerDisplay {
            provider: ValueDisplay {
                value: reranker.map_or(DEFAULT_RERANKER_PROVIDER.into(), |r| {
                    r.provider().to_string()
                }),
                is_default: reranker.map_or(true, |r| r.provider.is_none()),
                default: DEFAULT_RERANKER_PROVIDER.into(),
            },
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&display)?);
    } else {
        print!("{}", format_config_human(&display));
    }
    Ok(())
}

fn format_value(v: &ValueDisplay) -> String {
    if v.is_default {
        format!("{} (default)", v.value)
    } else {
        format!("{} (default: {})", v.value, v.default)
    }
}

fn format_optional(v: &Option<String>) -> &str {
    match v {
        Some(s) => s.as_str(),
        None => "-",
    }
}

fn format_config_human(d: &ConfigDisplay) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    writeln!(
        out,
        "Config file: {}",
        d.config_file.as_deref().unwrap_or("none")
    )
    .unwrap();
    writeln!(out, "Database:    {}", d.db_path).unwrap();

    writeln!(out, "\n[embedding]").unwrap();
    writeln!(
        out,
        "  provider:          {}",
        format_value(&d.embedding.provider)
    )
    .unwrap();
    writeln!(
        out,
        "  model:             {}",
        format_optional(&d.embedding.model)
    )
    .unwrap();
    writeln!(
        out,
        "  dimension:         {}",
        d.embedding.dimension.map_or("-".into(), |v| v.to_string())
    )
    .unwrap();

    writeln!(out, "\n[embedding.local]").unwrap();
    writeln!(
        out,
        "  query_prefix:      {}",
        format_optional(&d.embedding.local.query_prefix)
    )
    .unwrap();
    writeln!(
        out,
        "  document_prefix:   {}",
        format_optional(&d.embedding.local.document_prefix)
    )
    .unwrap();

    writeln!(out, "\n[embedding.ollama]").unwrap();
    writeln!(
        out,
        "  base_url:          {}",
        format_value(&d.embedding.ollama.base_url)
    )
    .unwrap();
    writeln!(
        out,
        "  model:             {}",
        format_value(&d.embedding.ollama.model)
    )
    .unwrap();

    writeln!(out, "\n[reranker]").unwrap();
    writeln!(
        out,
        "  provider:          {}",
        format_value(&d.reranker.provider)
    )
    .unwrap();

    out
}

#[derive(Serialize)]
struct ConfigDisplay {
    config_file: Option<String>,
    db_path: String,
    embedding: EmbeddingDisplay,
    reranker: RerankerDisplay,
}

#[derive(Serialize)]
struct EmbeddingDisplay {
    provider: ValueDisplay,
    model: Option<String>,
    dimension: Option<usize>,
    local: LocalEmbeddingDisplay,
    ollama: OllamaDisplay,
}

#[derive(Serialize)]
struct LocalEmbeddingDisplay {
    query_prefix: Option<String>,
    document_prefix: Option<String>,
}

#[derive(Serialize)]
struct OllamaDisplay {
    base_url: ValueDisplay,
    model: ValueDisplay,
}

#[derive(Serialize)]
struct RerankerDisplay {
    provider: ValueDisplay,
}

#[derive(Serialize)]
struct ValueDisplay {
    value: String,
    is_default: bool,
    default: String,
}

// ── Doctor Command ──

#[derive(Serialize)]
struct CheckResult {
    name: String,
    status: CheckStatus,
    message: String,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum CheckStatus {
    Ok,
    Warn,
    Error,
}

impl CheckStatus {
    fn icon(self) -> &'static str {
        match self {
            CheckStatus::Ok => "+",
            CheckStatus::Warn => "!",
            CheckStatus::Error => "x",
        }
    }
}

#[derive(Serialize)]
struct DoctorReport {
    checks: Vec<CheckResult>,
    summary: DoctorSummary,
}

#[derive(Serialize)]
struct DoctorSummary {
    total: usize,
    ok: usize,
    warn: usize,
    error: usize,
}

fn check_git_repo() -> CheckResult {
    let mut dir = std::env::current_dir().unwrap_or_default();
    loop {
        if dir.join(".git").exists() {
            return CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: format!("git repository at {}", dir.display()),
            };
        }
        if !dir.pop() {
            break;
        }
    }
    CheckResult {
        name: "git".into(),
        status: CheckStatus::Error,
        message: "not inside a git repository".into(),
    }
}

fn check_config(config_path: Option<&Path>) -> CheckResult {
    match config_path {
        Some(p) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Ok,
            message: format!("loaded from {}", p.display()),
        },
        None => CheckResult {
            name: "config".into(),
            status: CheckStatus::Warn,
            message: "no .cartog.toml found (using defaults)".into(),
        },
    }
}

fn check_database(db_path: &Path, embedding_dim: usize) -> CheckResult {
    if !db_path.exists() {
        return CheckResult {
            name: "database".into(),
            status: CheckStatus::Warn,
            message: format!(
                "database not found at {}, run 'cartog index'",
                db_path.display()
            ),
        };
    }
    match Database::open(db_path, embedding_dim) {
        Ok(db) => match db.stats() {
            Ok(stats) if stats.num_files > 0 => CheckResult {
                name: "database".into(),
                status: CheckStatus::Ok,
                message: format!(
                    "{} files, {} symbols at {}",
                    stats.num_files,
                    stats.num_symbols,
                    db_path.display()
                ),
            },
            Ok(_) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "database exists but is empty, run 'cartog index' ({})",
                    db_path.display()
                ),
            },
            Err(e) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Error,
                message: format!("failed to query database at {}: {e}", db_path.display()),
            },
        },
        Err(e) => CheckResult {
            name: "database".into(),
            status: CheckStatus::Error,
            message: format!("failed to open database at {}: {e}", db_path.display()),
        },
    }
}

/// Parse "http://host:port" into a "host:port" string for TCP probing.
fn parse_host_port(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let host_port = without_scheme.trim_end_matches('/');
    if host_port.contains(':') {
        Some(host_port.to_string())
    } else {
        Some(format!("{host_port}:80"))
    }
}

fn check_embedding_provider(config: &rag::EmbeddingProviderConfig) -> CheckResult {
    match config.provider.as_str() {
        "local" => {
            if rag::is_embedding_model_cached() {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Ok,
                    message: "local model cached".into(),
                }
            } else {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Warn,
                    message: "local model not downloaded, run 'cartog rag setup'".into(),
                }
            }
        }
        "ollama" => {
            let base_url = config
                .base_url
                .as_deref()
                .unwrap_or(rag::providers::DEFAULT_OLLAMA_BASE_URL);
            match parse_host_port(base_url) {
                Some(addr) => {
                    let resolve_result: Result<std::net::SocketAddr, _> =
                        std::net::ToSocketAddrs::to_socket_addrs(&addr.as_str())
                            .map(|mut addrs| addrs.next())
                            .and_then(|opt| {
                                opt.ok_or_else(|| {
                                    std::io::Error::new(
                                        std::io::ErrorKind::AddrNotAvailable,
                                        format!("no addresses resolved for {addr}"),
                                    )
                                })
                            });
                    let socket_addr = match resolve_result {
                        Ok(sa) => sa,
                        Err(e) => {
                            return CheckResult {
                                name: "embedding".into(),
                                status: CheckStatus::Error,
                                message: format!("cannot resolve ollama host '{addr}': {e}"),
                            };
                        }
                    };
                    match std::net::TcpStream::connect_timeout(&socket_addr, Duration::from_secs(3))
                    {
                        Ok(_) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Ok,
                            message: format!("ollama reachable at {base_url}"),
                        },
                        Err(e) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Error,
                            message: format!("cannot reach ollama at {base_url}: {e}"),
                        },
                    }
                }
                None => CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Error,
                    message: format!("cannot parse ollama URL: {base_url}"),
                },
            }
        }
        other => CheckResult {
            name: "embedding".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
        },
    }
}

fn check_reranker(config: &rag::EmbeddingProviderConfig) -> CheckResult {
    match config.reranker_provider.as_str() {
        "none" => CheckResult {
            name: "reranker".into(),
            status: CheckStatus::Ok,
            message: "disabled".into(),
        },
        "local" => {
            if rag::is_reranker_model_cached() {
                CheckResult {
                    name: "reranker".into(),
                    status: CheckStatus::Ok,
                    message: "local model cached".into(),
                }
            } else {
                CheckResult {
                    name: "reranker".into(),
                    status: CheckStatus::Warn,
                    message: "local model not downloaded, run 'cartog rag setup'".into(),
                }
            }
        }
        other => CheckResult {
            name: "reranker".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
        },
    }
}

fn build_report(checks: Vec<CheckResult>) -> DoctorReport {
    let ok = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Ok)
        .count();
    let warn = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let error = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Error)
        .count();

    DoctorReport {
        summary: DoctorSummary {
            total: checks.len(),
            ok,
            warn,
            error,
        },
        checks,
    }
}

fn format_report_human(report: &DoctorReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    for check in &report.checks {
        writeln!(
            out,
            "  [{}] {}: {}",
            check.status.icon(),
            check.name,
            check.message,
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    let s = &report.summary;
    if s.error > 0 {
        writeln!(
            out,
            "{} checks passed, {} warnings, {} errors",
            s.ok, s.warn, s.error
        )
        .unwrap();
    } else if s.warn > 0 {
        writeln!(out, "{} checks passed, {} warnings", s.ok, s.warn).unwrap();
    } else {
        writeln!(out, "All {} checks passed", s.ok).unwrap();
    }

    out
}

/// Check that requirements are met and everything is working.
pub fn cmd_doctor(
    config: &CartogConfig,
    config_path: Option<&Path>,
    db_path: &Path,
    json: bool,
    embedding_dim: usize,
    provider_config: &rag::EmbeddingProviderConfig,
) -> Result<()> {
    let _ = config; // config is read indirectly via provider_config

    let checks = vec![
        check_git_repo(),
        check_config(config_path),
        check_database(db_path, embedding_dim),
        check_embedding_provider(provider_config),
        check_reranker(provider_config),
    ];

    let report = build_report(checks);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", format_report_human(&report));
    }

    if report.summary.error > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Watch for file changes and auto-re-index.
pub fn cmd_watch(
    db_path: &Path,
    path: &str,
    debounce: u64,
    rag: bool,
    rag_delay: u64,
    provider_config: rag::EmbeddingProviderConfig,
) -> Result<()> {
    let mut config = WatchConfig::new(PathBuf::from(path));
    config.debounce = Duration::from_secs(debounce);
    config.rag = rag;
    config.rag_delay = Duration::from_secs(rag_delay);
    config.rag_config = provider_config;

    let db_path_str = db_path.to_string_lossy();
    watch::run_watch(config, &db_path_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

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

    // ── Config display tests ──

    fn default_config_display() -> ConfigDisplay {
        use crate::config::{
            DEFAULT_EMBEDDING_PROVIDER, DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_MODEL,
            DEFAULT_RERANKER_PROVIDER,
        };
        ConfigDisplay {
            config_file: None,
            db_path: "/tmp/test.db".into(),
            embedding: EmbeddingDisplay {
                provider: ValueDisplay {
                    value: DEFAULT_EMBEDDING_PROVIDER.into(),
                    is_default: true,
                    default: DEFAULT_EMBEDDING_PROVIDER.into(),
                },
                model: None,
                dimension: None,
                local: LocalEmbeddingDisplay {
                    query_prefix: None,
                    document_prefix: None,
                },
                ollama: OllamaDisplay {
                    base_url: ValueDisplay {
                        value: DEFAULT_OLLAMA_BASE_URL.into(),
                        is_default: true,
                        default: DEFAULT_OLLAMA_BASE_URL.into(),
                    },
                    model: ValueDisplay {
                        value: DEFAULT_OLLAMA_MODEL.into(),
                        is_default: true,
                        default: DEFAULT_OLLAMA_MODEL.into(),
                    },
                },
            },
            reranker: RerankerDisplay {
                provider: ValueDisplay {
                    value: DEFAULT_RERANKER_PROVIDER.into(),
                    is_default: true,
                    default: DEFAULT_RERANKER_PROVIDER.into(),
                },
            },
        }
    }

    #[test]
    fn test_format_config_human_all_defaults() {
        let d = default_config_display();
        let out = format_config_human(&d);
        assert!(out.contains("Config file: none"));
        assert!(out.contains("Database:    /tmp/test.db"));
        assert!(out.contains("local (default)"));
        assert!(out.contains("model:             -"));
        assert!(out.contains("dimension:         -"));
        assert!(out.contains("query_prefix:      -"));
        assert!(out.contains("document_prefix:   -"));
    }

    #[test]
    fn test_format_config_human_custom_values() {
        let mut d = default_config_display();
        d.config_file = Some("/project/.cartog.toml".into());
        d.embedding.provider = ValueDisplay {
            value: "ollama".into(),
            is_default: false,
            default: "local".into(),
        };
        d.embedding.model = Some("nomic-embed-text".into());
        d.embedding.dimension = Some(768);

        let out = format_config_human(&d);
        assert!(out.contains("Config file: /project/.cartog.toml"));
        assert!(out.contains("ollama (default: local)"));
        assert!(out.contains("model:             nomic-embed-text"));
        assert!(out.contains("dimension:         768"));
    }

    #[test]
    fn test_format_value_default() {
        let v = ValueDisplay {
            value: "local".into(),
            is_default: true,
            default: "local".into(),
        };
        assert_eq!(format_value(&v), "local (default)");
    }

    #[test]
    fn test_format_value_overridden() {
        let v = ValueDisplay {
            value: "ollama".into(),
            is_default: false,
            default: "local".into(),
        };
        assert_eq!(format_value(&v), "ollama (default: local)");
    }

    #[test]
    fn test_format_optional_some() {
        let v = Some("value".to_string());
        assert_eq!(format_optional(&v), "value");
    }

    #[test]
    fn test_format_optional_none() {
        let v: Option<String> = None;
        assert_eq!(format_optional(&v), "-");
    }

    #[test]
    fn test_config_display_json_serialization() {
        let d = default_config_display();
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["db_path"], "/tmp/test.db");
        assert_eq!(json["embedding"]["provider"]["value"], "local");
        assert_eq!(json["embedding"]["provider"]["is_default"], true);
        assert!(json["config_file"].is_null());
        assert!(json["embedding"]["model"].is_null());
    }

    #[test]
    fn test_truncate_to_budget_unicode() {
        // Each emoji is 4 bytes
        let text = "Hello 🌍🌍🌍🌍🌍🌍🌍🌍🌍🌍";
        let result = truncate_to_budget(text, 5);
        assert!(result.ends_with("... (truncated to fit token budget)"));
        // Should not panic on char boundary issues
    }

    // ── Doctor check tests ──

    #[test]
    #[serial]
    fn test_check_git_repo_inside_git() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let subdir = dir.path().join("sub");
        std::fs::create_dir(&subdir).unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&subdir).unwrap();
        let result = check_git_repo();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result.status, CheckStatus::Ok);
        assert_eq!(result.name, "git");
    }

    #[test]
    #[serial]
    fn test_check_git_repo_outside_git() {
        let dir = tempfile::TempDir::new().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = check_git_repo();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result.status, CheckStatus::Error);
    }

    #[test]
    fn test_check_config_present() {
        let result = check_config(Some(Path::new("/project/.cartog.toml")));
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains(".cartog.toml"));
    }

    #[test]
    fn test_check_config_absent() {
        let result = check_config(None);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("defaults"));
    }

    #[test]
    fn test_check_database_missing() {
        let result = check_database(Path::new("/nonexistent/path.db"), 384);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("not found"));
    }

    #[test]
    fn test_check_database_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let _db = Database::open(&db_path, 384).unwrap();
        let result = check_database(&db_path, 384);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("empty"));
    }

    #[test]
    fn test_check_reranker_disabled() {
        let config = rag::EmbeddingProviderConfig {
            reranker_provider: "none".into(),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("disabled"));
    }

    #[test]
    fn test_check_reranker_unknown_provider() {
        let config = rag::EmbeddingProviderConfig {
            reranker_provider: "foobar".into(),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("foobar"));
    }

    #[test]
    fn test_check_embedding_unknown_provider() {
        let config = rag::EmbeddingProviderConfig {
            provider: "unknown".into(),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("unknown"));
    }

    #[test]
    fn test_check_embedding_ollama_unreachable() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: Some("http://127.0.0.1:19999".into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("cannot reach"));
    }

    #[test]
    fn test_check_status_icons() {
        assert_eq!(CheckStatus::Ok.icon(), "+");
        assert_eq!(CheckStatus::Warn.icon(), "!");
        assert_eq!(CheckStatus::Error.icon(), "x");
    }

    #[test]
    fn test_parse_host_port_standard() {
        assert_eq!(
            parse_host_port("http://localhost:11434"),
            Some("localhost:11434".into())
        );
    }

    #[test]
    fn test_parse_host_port_no_port() {
        assert_eq!(
            parse_host_port("http://example.com"),
            Some("example.com:80".into())
        );
    }

    #[test]
    fn test_parse_host_port_https() {
        assert_eq!(
            parse_host_port("https://example.com:443"),
            Some("example.com:443".into())
        );
    }

    #[test]
    fn test_parse_host_port_trailing_slash() {
        assert_eq!(
            parse_host_port("http://localhost:11434/"),
            Some("localhost:11434".into())
        );
    }

    #[test]
    fn test_parse_host_port_no_scheme() {
        assert_eq!(parse_host_port("localhost:11434"), None);
    }

    #[test]
    fn test_doctor_report_json_serialization() {
        let report = DoctorReport {
            checks: vec![
                CheckResult {
                    name: "git".into(),
                    status: CheckStatus::Ok,
                    message: "git repository".into(),
                },
                CheckResult {
                    name: "config".into(),
                    status: CheckStatus::Warn,
                    message: "no config".into(),
                },
            ],
            summary: DoctorSummary {
                total: 2,
                ok: 1,
                warn: 1,
                error: 0,
            },
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["checks"][0]["status"], "ok");
        assert_eq!(json["checks"][1]["status"], "warn");
        assert_eq!(json["summary"]["total"], 2);
        assert_eq!(json["summary"]["ok"], 1);
    }

    // ── build_report tests ──

    #[test]
    fn test_build_report_all_ok() {
        let checks = vec![
            CheckResult {
                name: "a".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
        ];
        let report = build_report(checks);
        assert_eq!(report.summary.total, 2);
        assert_eq!(report.summary.ok, 2);
        assert_eq!(report.summary.warn, 0);
        assert_eq!(report.summary.error, 0);
    }

    #[test]
    fn test_build_report_mixed() {
        let checks = vec![
            CheckResult {
                name: "a".into(),
                status: CheckStatus::Ok,
                message: "fine".into(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Warn,
                message: "meh".into(),
            },
            CheckResult {
                name: "c".into(),
                status: CheckStatus::Error,
                message: "bad".into(),
            },
        ];
        let report = build_report(checks);
        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.ok, 1);
        assert_eq!(report.summary.warn, 1);
        assert_eq!(report.summary.error, 1);
    }

    #[test]
    fn test_build_report_empty() {
        let report = build_report(vec![]);
        assert_eq!(report.summary.total, 0);
        assert_eq!(report.summary.ok, 0);
        assert_eq!(report.summary.warn, 0);
        assert_eq!(report.summary.error, 0);
    }

    // ── format_report_human tests ──

    #[test]
    fn test_format_report_human_all_ok() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "git repository".into(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Ok,
                message: "42 files".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[+] git: git repository"));
        assert!(out.contains("[+] db: 42 files"));
        assert!(out.contains("All 2 checks passed"));
    }

    #[test]
    fn test_format_report_human_with_warnings() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "config".into(),
                status: CheckStatus::Warn,
                message: "missing".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[!] config: missing"));
        assert!(out.contains("1 checks passed, 1 warnings"));
        assert!(!out.contains("errors"));
    }

    #[test]
    fn test_format_report_human_with_errors() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "embed".into(),
                status: CheckStatus::Warn,
                message: "not cached".into(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Error,
                message: "broken".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[x] db: broken"));
        assert!(out.contains("1 checks passed, 1 warnings, 1 errors"));
    }

    // ── check_database with indexed data ──

    #[test]
    fn test_check_database_with_data() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        // Insert a minimal file so stats.num_files > 0
        db.upsert_file(&cartog_core::FileInfo {
            path: "test.py".into(),
            last_modified: 0.0,
            hash: "abc123".into(),
            language: "python".into(),
            num_symbols: 0,
        })
        .unwrap();
        drop(db);

        let result = check_database(&db_path, 384);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("1 files"));
    }

    // ── check_embedding_provider local variants ──

    #[test]
    fn test_check_embedding_local_cached() {
        // This test reflects actual machine state — the local model is cached on CI/dev
        let config = rag::EmbeddingProviderConfig::default();
        let result = check_embedding_provider(&config);
        // Either Ok (cached) or Warn (not cached) — never Error for "local"
        assert_ne!(result.status, CheckStatus::Error);
        assert_eq!(result.name, "embedding");
    }

    #[test]
    fn test_check_reranker_local() {
        let config = rag::EmbeddingProviderConfig::default();
        let result = check_reranker(&config);
        // Either Ok (cached) or Warn (not cached) — never Error for "local"
        assert_ne!(result.status, CheckStatus::Error);
        assert_eq!(result.name, "reranker");
    }

    // ── check_embedding_provider ollama with bad URL ──

    #[test]
    fn test_check_embedding_ollama_bad_url() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: Some("not-a-url".into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("cannot parse"));
    }

    // ── check_embedding_provider ollama with default URL (unreachable in test) ──

    #[test]
    fn test_check_embedding_ollama_default_url() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: None,
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        // On machines without ollama running, this will be Error
        // On machines with ollama running, this will be Ok
        assert_eq!(result.name, "embedding");
        assert!(
            result.status == CheckStatus::Ok || result.status == CheckStatus::Error,
            "ollama check should be Ok or Error, not Warn"
        );
    }
}
