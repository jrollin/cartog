//! Semantic-search commands backed by embeddings: `rag setup` (model download),
//! `rag index` (embed the graph), `rag search` (hybrid FTS5 + vector), and
//! `context` (token-budgeted task bundle).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::progress::{install_cancel_probe, spinner_callback, stop_spinner, Spinner};
use super::shared::{empty_index_hint, open_db, output};
use cartog_core::Compact;
use cartog_indexer as indexer;
use cartog_rag as rag;

/// Download the embedding model.
pub fn cmd_rag_setup(json: bool, provider_config: &rag::EmbeddingProviderConfig) -> Result<()> {
    let reranker_model = provider_config.reranker_model.as_deref();
    let spinner = if json {
        None
    } else {
        // One-time notice so the multi-hundred-MB download isn't a silent wait.
        eprintln!(
            "Downloading embedding (~80MB) + re-ranker ({}) models, one-time…",
            reranker_model.unwrap_or(rag::DEFAULT_RERANKER_MODEL)
        );
        Spinner::start("Downloading models")
    };
    // Download bi-encoder (embeddings)
    let embed_result = rag::setup::download_model();
    // Download cross-encoder (re-ranking) — the configured model, not always the default.
    let rerank_result =
        rag::setup::download_cross_encoder(reranker_model, provider_config.intra_threads);
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
    redact: indexer::RedactionConfig,
    filter: &indexer::WalkFilter,
) -> Result<()> {
    let root = Path::new(path);
    // Install the handler first so Ctrl-C also covers the (potentially long,
    // first-run) embedding-model download inside create_embedding_provider.
    let cancel = install_cancel_probe();
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;
    db.reconcile_embedding_fingerprint(&rag::fingerprint_of(provider.as_ref()))
        .context("failed to reconcile embedding fingerprint")?;

    // Progress on stderr; `Spinner::start` self-gates (TTY or CARTOG_PROGRESS).
    let spinner = Spinner::start("Indexing code graph").map(Arc::new);
    let ix_cb = spinner_callback(&spinner, indexer::ProgressUpdate::label);
    let ix_cb_ref: Option<indexer::ProgressCallback<'_>> =
        ix_cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let ix_cancel: indexer::CancelProbe<'_> = &cancel;
    let index_res = indexer::index_directory(
        &db,
        root,
        false,
        false,
        ix_cb_ref,
        Some(ix_cancel),
        redact,
        &std::collections::HashMap::new(),
        filter,
    );
    drop(ix_cb);
    stop_spinner(spinner);
    match index_res {
        Ok(_) => {}
        Err(e) if indexer::is_cancelled(&e) => {
            if !json {
                eprintln!("Indexing cancelled; the index was left unchanged.");
            }
            return Ok(());
        }
        Err(e) => return Err(e),
    }

    let spinner = Spinner::start("Embedding symbols").map(Arc::new);
    let rag_cb = spinner_callback(&spinner, rag::indexer::ProgressUpdate::label);
    let rag_cb_ref: Option<rag::indexer::ProgressCallback<'_>> =
        rag_cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let rag_cancel: rag::indexer::CancelProbe<'_> = &cancel;
    let embed_res =
        rag::indexer::index_embeddings(&db, provider.as_mut(), force, rag_cb_ref, Some(rag_cancel));
    drop(rag_cb);
    stop_spinner(spinner);
    let result = match embed_res {
        Ok(r) => r,
        // Flushed batches persist on an incremental run; a --force/upgrade run
        // cleared up front, so re-run to rebuild. Either way, re-run to finish.
        Err(e) if indexer::is_cancelled(&e) => {
            if !json {
                eprintln!("Embedding cancelled; re-run to finish.");
            }
            return Ok(());
        }
        Err(e) => return Err(e),
    };

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
    kind: Option<crate::cli::SymbolKindFilter>,
    limit: u32,
    json: bool,
    compact: bool,
    token_budget: Option<u32>,
    provider_config: &rag::EmbeddingProviderConfig,
    tuning: &rag::search::SearchTuning,
) -> Result<()> {
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;
    // NOTE: `cartog rag search` deliberately does NOT call
    // `reconcile_embedding_fingerprint`. The reconcile is destructive
    // (drops `symbol_vec` on mismatch) and can race a primary
    // `cartog serve` writer if the user's `.cartog.toml` changed since
    // last index. Search is read-only by nature; if the fingerprint
    // mismatches, the user gets the embeddings produced by the previous
    // provider — possibly poor results, but no data loss. Re-embedding
    // is `cartog rag index`'s job, which DOES reconcile.
    let kind_filter = match kind {
        Some(crate::cli::SymbolKindFilter::All) => rag::search::KindFilter::All,
        Some(k) => rag::search::KindFilter::Exact(cartog_core::SymbolKind::from(k)),
        None => rag::search::KindFilter::CodeOnly,
    };

    // Lazy reranker: the cross-encoder ONNX model is loaded only if retrieval
    // produced enough candidates for `rerank_min` to fire. For a one-shot CLI
    // command that may return fewer than `rerank_min` hits, this avoids
    // ~100-200ms of model-load latency + memory on every invocation.
    let reranker_factory = if provider_config.reranker_provider == "none" {
        None
    } else {
        let name = provider_config.reranker_provider.clone();
        let model = provider_config.reranker_model.clone();
        let threads = provider_config.intra_threads;
        Some(move || rag::create_reranker_provider(&name, model.as_deref(), threads))
    };
    let mut search_result = rag::search::hybrid_search_tuned_lazy(
        &db,
        query,
        limit,
        kind_filter,
        provider.as_mut(),
        reranker_factory,
        tuning,
    )?;
    if compact {
        search_result.compact_in_place();
    }
    db.log_query("rag_search", "cli");
    let query = query.to_string();
    // Gate the "build the index" hint on whether embeddings actually exist, not
    // on the result counts: kind-filtered retrieval can legitimately yield
    // fts/vec_count == 0 for a code query that only matched docs, even with a
    // fully-built index.
    let embeddings_built = db.embedding_count().map(|n| n > 0).unwrap_or(false);

    output(&search_result, json, token_budget, |sr| {
        render_rag_search(sr, &query, embeddings_built)
    })
}

/// Render the human-readable `rag search` output.
///
/// `embeddings_built` gates the index hints: `vec_count == 0` alone is not a
/// reliable signal — kind-filtered retrieval can legitimately yield zero
/// vector hits on a fully-built index.
fn render_rag_search(
    sr: &rag::search::HybridSearchResult,
    query: &str,
    embeddings_built: bool,
) -> String {
    if sr.results.is_empty() {
        let mut out = format!("No results found for '{query}'\n");
        if !embeddings_built {
            out.push_str("Hint: run 'cartog rag index' to build the semantic search index.\n");
        }
        return out;
    }
    let mut out = format!(
        "Found {} results (FTS: {}, vector: {}, merged: {})\n",
        sr.results.len(),
        sr.fts_count,
        sr.vec_count,
        sr.merged_count
    );
    if !embeddings_built {
        out.push_str(
            "Hint: keyword (FTS) matches only — run 'cartog rag index' to enable semantic search.\n",
        );
    }
    out.push('\n');
    for (i, r) in sr.results.iter().enumerate() {
        let sources = r
            .sources
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("+");
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
}

/// Build a token-budgeted task-context bundle for a natural-language task.
pub fn cmd_context(
    db_path: &Path,
    task: &str,
    tokens: u32,
    json: bool,
    compact: bool,
    provider_config: &rag::EmbeddingProviderConfig,
    tuning: &rag::search::SearchTuning,
) -> Result<()> {
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;

    // Build the bundle in its own scope so the provider/reranker borrows end
    // before the `output` closure (which re-borrows `&db`) runs.
    let mut ctx = {
        let mut reranker = if provider_config.reranker_provider == "none" {
            None
        } else {
            rag::create_reranker_provider(
                &provider_config.reranker_provider,
                provider_config.reranker_model.as_deref(),
                provider_config.intra_threads,
            )
        };
        let opts = rag::context::ContextOptions {
            tuning: *tuning,
            ..Default::default()
        };
        // `match` (not `as_deref_mut`) keeps the reranker borrow scoped to the
        // call so it drops before `output` re-borrows `db`.
        match reranker.as_mut() {
            Some(r) => rag::context::build_task_context(
                &db,
                task,
                tokens,
                provider.as_mut(),
                Some(r.as_mut()),
                &opts,
            ),
            None => {
                rag::context::build_task_context(&db, task, tokens, provider.as_mut(), None, &opts)
            }
        }?
    };
    if !ctx.entries.is_empty() {
        db.log_query("context", "cli");
    }
    // Compact trims the per-entry symbol noise but keeps the budgeted bodies —
    // a context bundle's whole value is its inline bodies.
    if compact {
        ctx.compact_in_place();
    }
    let task = task.to_string();

    output(&ctx, json, None, |ctx| {
        if ctx.entries.is_empty() {
            return format!("No context found for '{task}'{}\n", empty_index_hint(&db));
        }
        let mut out = format!(
            "Context for '{task}' ({} symbols, ~{} tokens)\n\n",
            ctx.entries.len(),
            ctx.approx_tokens
        );
        for entry in &ctx.entries {
            out.push_str(&format!(
                "[{reason:?}] {kind} {name}  {file}:{line}\n",
                reason = entry.reason,
                kind = entry.symbol.kind,
                name = entry.symbol.name,
                file = entry.symbol.file_path,
                line = entry.symbol.start_line,
            ));
            if let Some(body) = &entry.content {
                for l in body.lines() {
                    out.push_str(&format!("    {l}\n"));
                }
                out.push('\n');
            }
        }
        out
    })
}

// No CLI test for `cmd_context`: it builds a real embedding provider
// (ONNX model), so it can't run model-independently in CI. The fusion
// logic is covered by `cartog_rag::context` unit tests (MockEmbeddingProvider)
// and the `cartog_context` MCP tool test (test_provider).

#[cfg(test)]
mod tests {
    use super::*;

    fn fts_only_search_result() -> rag::search::HybridSearchResult {
        rag::search::HybridSearchResult {
            results: vec![rag::search::SearchResult {
                symbol: cartog_core::Symbol::new(
                    "foo",
                    cartog_core::SymbolKind::Function,
                    "a.py",
                    1,
                    10,
                    0,
                    100,
                    None,
                ),
                content: None,
                rrf_score: 0.016,
                rerank_score: None,
                sources: vec![],
            }],
            fts_count: 1,
            vec_count: 0,
            merged_count: 1,
        }
    }

    #[test]
    fn rag_search_render_hints_when_results_found_but_embeddings_missing() {
        let out = render_rag_search(&fts_only_search_result(), "foo", false);
        assert!(
            out.contains("cartog rag index"),
            "FTS-only results without embeddings must hint at 'cartog rag index'; got:\n{out}"
        );
    }

    #[test]
    fn rag_search_render_no_hint_when_embeddings_built() {
        let out = render_rag_search(&fts_only_search_result(), "foo", true);
        assert!(
            !out.contains("Hint:"),
            "vec_count == 0 with a built index is legitimate, no hint expected; got:\n{out}"
        );
    }
}
