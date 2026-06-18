//! Phase functions for the incremental-index pipeline.
//!
//! [`index_directory`](crate::index_directory) is a thin orchestrator over four
//! phases. Phase 1's walk lives with the filter it applies, in
//! [`walk_candidates`](crate::walk::walk_candidates); phases 2–4 live here:
//! 1. [`walk_candidates`](crate::walk::walk_candidates) — walk + filter + git/hash skip (no DB writes),
//! 2. [`parse_candidates`] — parallel parse + extract (rayon, no DB writes),
//! 3. [`store_parsed_file`] — per-file Merkle-diff + DB writes, called once per
//!    parsed file inside the orchestrator's store loop,
//! 4. [`resolve_and_finalize`] — edge resolution + LSP pass + metadata.
//!
//! Phases 3–4 take `&Database` and call only the `*_in_tx` batch helpers. The
//! transaction guard is owned by the orchestrator and never crosses a fn
//! boundary, so the single-transaction atomicity invariant is unchanged: a
//! crash before `tx.commit()` rolls back every write these fns issued.

use super::*;

/// Phase 2: parse + extract every candidate in parallel.
///
/// CPU-bound, runs on a dedicated rayon pool sized to `filter.jobs` (falls back
/// to the global pool if it can't be built) so the `--jobs` cap applies on every
/// index. DB-free: workers decide hash-skip from the pre-fetched `stored_hashes`.
/// Emits climbing `Parsing` progress without ever stepping backward despite
/// out-of-order worker completion.
#[must_use]
pub(crate) fn parse_candidates(
    candidates: &[(PathBuf, String, &'static str)],
    force: bool,
    redact: RedactionConfig,
    stored_hashes: &std::collections::HashMap<String, String>,
    jobs: usize,
    emit: &(dyn Fn(ProgressUpdate) + Send + Sync),
) -> Vec<ParseOutput> {
    let parse_total = candidates.len() as u32;
    emit(ProgressUpdate::Parsing {
        done: 0,
        total: parse_total,
    });
    // Rayon workers finish out of order. `parsed_count` assigns each a unique n;
    // `reported_high` is a running max so a late straggler never emits a `done`
    // below one already shown (the spinner would otherwise flicker backward).
    use std::sync::atomic::{AtomicU32, Ordering};
    let parsed_count = AtomicU32::new(0);
    let reported_high = AtomicU32::new(0);
    let run_parse = || -> Vec<ParseOutput> {
        candidates
            .par_iter()
            .map(|(abs, rel, lang)| {
                let out = parse_one_file(
                    abs,
                    rel,
                    lang,
                    force,
                    stored_hashes.get(rel).map(String::as_str),
                    redact,
                );
                let n = parsed_count.fetch_add(1, Ordering::Relaxed) + 1;
                if n % PROGRESS_STRIDE == 0 || n == parse_total {
                    // fetch_max returns the prior high; only emit if we raised it,
                    // so emitted `done` is non-decreasing despite out-of-order calls.
                    if reported_high.fetch_max(n, Ordering::Relaxed) < n {
                        emit(ProgressUpdate::Parsing {
                            done: n,
                            total: parse_total,
                        });
                    }
                }
                out
            })
            .collect()
    };
    // Run on a dedicated pool sized to `jobs` so the cap applies on every call;
    // fall back to the global pool if it can't be built.
    match parse_pool(jobs) {
        Some(pool) => pool.install(run_parse),
        None => run_parse(),
    }
}

/// Phase 3 (per file): Merkle-diff the parsed symbols against the stored set and
/// apply surgical DB updates inside the caller's open transaction.
///
/// `db` is the same connection the caller opened the indexing tx on; every write
/// here uses a `*_in_tx` helper so it participates in that transaction. Marks the
/// file dirty (for scoped resolution), folds per-file counts into `result`, and
/// records newly-added symbol names in `added_symbol_names` (used post-loop to
/// reopen state {2, 3} markers that now have a matching target).
#[allow(clippy::too_many_arguments)]
pub(crate) fn store_parsed_file(
    db: &Database,
    rel_path: String,
    lang: &str,
    source: &str,
    hash: String,
    modified: f64,
    symbols: &[Symbol],
    edges: &[cartog_core::Edge],
    force: bool,
    redact: RedactionConfig,
    dirty_files: &mut std::collections::HashSet<String>,
    added_symbol_names: &mut std::collections::HashSet<String>,
    result: &mut IndexResult,
) -> Result<()> {
    // Force skips Merkle diff: source is unchanged on a policy-change
    // re-index, so a diff would find nothing dirty and skip the content
    // rewrite, leaving stale (un-redacted) rows.
    let old_hashes = db.get_symbol_hashes_for_file(&rel_path)?;
    let has_old_hashes =
        !force && !old_hashes.is_empty() && old_hashes.iter().any(|(_, ch, _)| ch.is_some());

    if has_old_hashes {
        // Merkle diff: surgical updates
        let diff = merkle_diff(symbols, &old_hashes);

        dirty_files.insert(rel_path.clone());

        for &i in &diff.added {
            added_symbol_names.insert(symbols[i].name.clone());
        }

        db.delete_symbols_in_tx(&diff.removed)?;
        result.symbols_removed += diff.removed.len() as u32;

        let mut changed: Vec<cartog_core::Symbol> = Vec::with_capacity(
            diff.added.len() + diff.modified.len() + diff.children_changed.len(),
        );
        changed.extend(diff.added.iter().map(|&i| symbols[i].clone()));
        changed.extend(diff.modified.iter().map(|&i| symbols[i].clone()));
        changed.extend(diff.children_changed.iter().map(|&i| symbols[i].clone()));
        db.insert_symbols_in_tx(&changed)?;

        // A modified symbol keeps its stable id, so its old embedding lingers
        // and `symbols_needing_embeddings` would skip it — drop the drifted
        // vector so it re-embeds. children_changed symbols have unchanged own
        // content, so their embeddings are still valid. Skip the work entirely
        // for repos with no embeddings (the common non-RAG case).
        let modified_ids: Vec<String> = diff
            .modified
            .iter()
            .map(|&i| symbols[i].id.clone())
            .collect();
        if !modified_ids.is_empty() && db.embedding_count()? > 0 {
            db.clear_embeddings_for_symbols_in_tx(&modified_ids)?;
        }

        result.symbols_added += diff.added.len() as u32;
        result.symbols_modified += diff.modified.len() as u32;
        result.symbols_unchanged += diff.unchanged as u32;

        db.clear_edges_for_file(&rel_path)?;
        db.insert_edges_in_tx(edges)?;
        result.edges_added += edges.len() as u32;

        let dirty_indices: Vec<usize> = diff
            .added
            .iter()
            .chain(diff.modified.iter())
            .copied()
            .collect();
        let contents: Vec<(String, String, String, String)> = dirty_indices
            .iter()
            .map(|&i| &symbols[i])
            .filter(|sym| sym.kind != cartog_core::SymbolKind::Import)
            .filter_map(|sym| {
                extract_symbol_content_redacted(source, sym, redact)
                    .map(|(content, header)| (sym.id.clone(), sym.name.clone(), content, header))
            })
            .collect();
        // A modified symbol whose new body no longer yields content is absent
        // from `contents`, so its pre-edit content row would linger and
        // re-embed stale text. Delete content for any modified id not rewritten.
        let rewritten: std::collections::HashSet<&str> =
            contents.iter().map(|(id, ..)| id.as_str()).collect();
        let lost_content: Vec<String> = modified_ids
            .iter()
            .filter(|id| !rewritten.contains(id.as_str()))
            .cloned()
            .collect();
        db.clear_content_for_symbols_in_tx(&lost_content)?;
        if !contents.is_empty() {
            db.insert_symbol_contents_in_tx(&contents)?;
        }
    } else {
        // No stored hashes (first index or post-migration): full insert
        dirty_files.insert(rel_path.clone());
        // Every symbol in a freshly-inserted file is "new" wrt the marker.
        for sym in symbols {
            added_symbol_names.insert(sym.name.clone());
        }
        db.clear_file_data_in_tx(&rel_path)?;

        db.insert_symbols_in_tx(symbols)?;
        db.insert_edges_in_tx(edges)?;

        result.symbols_added += symbols.len() as u32;
        result.edges_added += edges.len() as u32;

        let contents: Vec<(String, String, String, String)> = symbols
            .iter()
            .filter(|sym| sym.kind != cartog_core::SymbolKind::Import)
            .filter_map(|sym| {
                extract_symbol_content_redacted(source, sym, redact)
                    .map(|(content, header)| (sym.id.clone(), sym.name.clone(), content, header))
            })
            .collect();
        if !contents.is_empty() {
            db.insert_symbol_contents_in_tx(&contents)?;
        }
    }

    db.upsert_file(&FileInfo {
        path: rel_path,
        last_modified: modified,
        hash,
        language: lang.to_string(),
        num_symbols: symbols.len() as u32,
    })?;

    Ok(())
}

/// Phase 4: resolve edges (heuristic, then LSP), seal the backlog, and write the
/// run's metadata — all inside the caller's open transaction.
///
/// Runs after the per-file store loop and the removal sweep. `lsp` and
/// `lsp_overrides` only matter when the `lsp` feature is compiled in. Mutates
/// `result`'s resolution counters; returns nothing the caller can't read off
/// `result`. The transaction is committed by the caller, not here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_and_finalize(
    db: &Database,
    root: &Path,
    force: bool,
    lsp: bool,
    lsp_overrides: &std::collections::HashMap<String, Vec<String>>,
    filter: &WalkFilter,
    dirty_files: &std::collections::HashSet<String>,
    current_files: &std::collections::HashSet<String>,
    added_symbol_names: &std::collections::HashSet<String>,
    redact: RedactionConfig,
    emit: &(dyn Fn(ProgressUpdate) + Send + Sync),
    cancel: Option<CancelProbe<'_>>,
    result: &mut IndexResult,
) -> Result<()> {
    // Force-reindex must retry edges previously marked unresolvable OR
    // external — otherwise --force would silently honor a stale state {2, 3}
    // marker.
    if force {
        db.reset_all_unresolvable()?;
    }

    // Reopen state {2, 3} markers whose target_name was just added. Runs
    // BEFORE the heuristic + LSP passes so reopened edges flow through both.
    if !added_symbol_names.is_empty() {
        let names: Vec<String> = added_symbol_names.iter().cloned().collect();
        db.reset_unresolvable_for_names(&names)?;
    }

    // Resolve edges — scoped to dirty files for incremental, global for force/first-index
    if force || dirty_files.len() == current_files.len() {
        result.edges_resolved = db.resolve_edges_in_tx()?;
        db.compute_in_degrees()?;
    } else if !dirty_files.is_empty() {
        // Invalidate edges from unchanged files that pointed to symbols in dirty files
        // (those symbol IDs may have changed even with stable IDs if a symbol was renamed/removed)
        db.invalidate_edges_targeting(dirty_files)?;
        result.edges_resolved = db.resolve_edges_scoped_in_tx(dirty_files)?;
        db.compute_in_degrees_scoped(dirty_files)?;
    }

    // LSP-based resolution for edges the heuristic couldn't resolve.
    // Auto-detected when `lsp` feature is compiled in; silently skipped otherwise.
    // The LSP-side helpers (`unresolved_edges`, `find_symbol_at_location`,
    // `update_edge_target`) all use single-statement execs that participate in
    // the outer transaction — no extra plumbing needed.
    //
    // Skipped on no-op runs: unresolved set is identical to last run, so
    // re-querying the LSP repeats work. Use `--force` to retry.
    let lsp_ran;
    #[cfg(feature = "lsp")]
    {
        lsp_ran = lsp && !dirty_files.is_empty();
        if lsp_ran {
            // Watch (lsp=false) may have sealed edges at state=4; give LSP a shot.
            db.reopen_heuristic_exhausted()?;
            // No phase event is forced here: lsp_resolve_edges fires the first
            // ResolvingLsp tick only when there are edges to resolve. A run with
            // zero unresolved edges intentionally shows no "resolving" phase
            // rather than a misleading "resolving 0 edges" marker.
            let lsp_progress = |done: u32, total: u32| {
                emit(ProgressUpdate::ResolvingLsp { done, total });
            };
            // Thread the caller's cancel probe so Ctrl-C interrupts the LSP
            // phase (the dominant cost) between files/windows, not just the
            // store loop.
            let stats = cartog_lsp::lsp_resolve_edges(
                db,
                root,
                None,
                lsp_overrides,
                Some(&lsp_progress),
                cancel,
                filter.lsp_max_servers,
            )
            .with_context(|| format!("resolving LSP edges for root {}", root.display()))?;
            result.edges_lsp_resolved = stats.resolved;
            result.edges_marked_unresolvable = stats.marked_unresolvable;
            result.edges_marked_external = stats.marked_external;
        }
    }
    #[cfg(not(feature = "lsp"))]
    {
        lsp_ran = false;
        let _ = (lsp, lsp_overrides, filter, emit, cancel); // unused when lsp feature off
    }

    // No LSP pass ran, so the heuristic was the only resolver: seal remaining
    // state=0 edges at state=4 to keep the next re-index's resolution scan from
    // re-walking the permanent-failure backlog (#109 watch amplification).
    if !lsp_ran && !dirty_files.is_empty() {
        db.mark_heuristic_exhausted_in_tx()?;
    }

    // Store the current git commit as last indexed
    if let Some(commit) = git_head_commit(root) {
        db.set_metadata("last_commit", &commit)?;
    }

    // Record the redaction policy this index was built under so the next run
    // can detect a toggle and force a re-index.
    db.set_metadata("redact_secrets", &redact.enabled.to_string())?;

    Ok(())
}
