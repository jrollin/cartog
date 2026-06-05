//! Code indexing and change detection for cartog.
//!
//! Walks a directory tree, detects changed files (git diff, SHA-256 hash, or force),
//! extracts symbols and edges via [`cartog_languages`], and writes results to
//! [`cartog_db`]. Uses Merkle tree hashing for surgical symbol-level updates.

use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use tracing::warn;
use walkdir::WalkDir;

use cartog_core::{FileInfo, Symbol};
use cartog_db::Database;
use cartog_languages::{detect_language, get_extractor, Extractor};

thread_local! {
    /// Per-worker cache of tree-sitter extractors. Reused across files within
    /// a single rayon worker thread so the Parser is constructed once per
    /// language per thread instead of once per file.
    static THREAD_EXTRACTORS: RefCell<HashMap<&'static str, Box<dyn Extractor>>>
        = RefCell::new(HashMap::new());
}

/// Output of the parallel per-file parse phase.
enum ParseOutput {
    /// Stored hash matched — no re-parse needed.
    Skipped,
    /// File was parsed; caller must run the Merkle-diff + DB write path.
    Parsed {
        rel_path: String,
        lang: &'static str,
        source: String,
        hash: String,
        modified: f64,
        symbols: Vec<Symbol>,
        edges: Vec<cartog_core::Edge>,
    },
    /// Read or extraction failed — already logged; caller increments nothing.
    Failed,
}

fn parse_one_file(
    path: &Path,
    rel_path: &str,
    lang: &'static str,
    force: bool,
    stored_hash: Option<&str>,
    redact: RedactionConfig,
) -> ParseOutput {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return ParseOutput::Failed, // binary
        Err(e) => {
            warn!(file = %rel_path, error = %e, "cannot read file");
            return ParseOutput::Failed;
        }
    };

    let hash = file_hash(&source);

    // Hash-based skip (only when not forcing).
    if !force {
        if let Some(old) = stored_hash {
            if old == hash {
                return ParseOutput::Skipped;
            }
        }
    }

    let modified = file_modified(path);

    let extraction = extract_with_cached(lang, &source, rel_path);

    let mut extraction = match extraction {
        Some(Ok(e)) => e,
        Some(Err(err)) => {
            warn!(file = %rel_path, error = %err, "extraction failed");
            return ParseOutput::Failed;
        }
        None => {
            warn!(file = %rel_path, lang, "no extractor registered for language");
            return ParseOutput::Failed;
        }
    };

    dedup_symbol_ids(&mut extraction.symbols, &mut extraction.edges);
    compute_merkle_hashes(&mut extraction.symbols, &source);

    // Redact after hashing so content_hash stays keyed on raw signature: the
    // redaction flag never perturbs Merkle identity or stable IDs.
    if redact.enabled {
        redact_symbol_fields(&mut extraction.symbols, redact);
    }

    ParseOutput::Parsed {
        rel_path: rel_path.to_string(),
        lang,
        source,
        hash,
        modified,
        symbols: extraction.symbols,
        edges: extraction.edges,
    }
}

/// Run the per-thread cached extractor for `lang` over `source`.
///
/// Returns `None` when no extractor is registered for `lang` — which means
/// `detect_language` and `get_extractor` disagree (a bug introduced when adding
/// a language). The caller skips the file rather than panicking inside the rayon
/// worker, which would abort the whole index run.
fn extract_with_cached(
    lang: &'static str,
    source: &str,
    rel_path: &str,
) -> Option<Result<cartog_languages::ExtractionResult>> {
    THREAD_EXTRACTORS.with(|cell| {
        let mut map = cell.borrow_mut();
        let extractor = match map.entry(lang) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(get_extractor(lang)?),
        };
        Some(extractor.extract(source, rel_path))
    })
}

/// Redact secrets from each symbol's `signature` and `docstring` in place.
///
/// These fields are returned verbatim by `cartog search`/`outline`, so they are
/// an agent-facing leak surface independent of the RAG content store.
fn redact_symbol_fields(symbols: &mut [Symbol], redact: RedactionConfig) {
    for sym in symbols.iter_mut() {
        if let Some(sig) = &sym.signature {
            if let std::borrow::Cow::Owned(r) = redact.redact(sig) {
                sym.signature = Some(r);
            }
        }
        if let Some(doc) = &sym.docstring {
            if let std::borrow::Cow::Owned(r) = redact.redact(doc) {
                sym.docstring = Some(r);
            }
        }
    }
}

/// Summary of an indexing operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct IndexResult {
    pub files_indexed: u32,
    pub files_skipped: u32,
    pub files_removed: u32,
    pub symbols_added: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_modified: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_unchanged: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_removed: u32,
    pub edges_added: u32,
    pub edges_resolved: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub edges_lsp_resolved: u32,
    /// Edges LSP marked `resolution_state = 2` (definitively unresolvable) this run.
    /// They are skipped by future LSP queries until a matching symbol is added.
    #[serde(skip_serializing_if = "is_zero")]
    pub edges_marked_unresolvable: u32,
    /// Edges LSP marked `resolution_state = 3` (target lives outside the indexed
    /// root: stdlib, deps, node_modules). Same skip + reopen semantics as
    /// `edges_marked_unresolvable`.
    #[serde(skip_serializing_if = "is_zero")]
    pub edges_marked_external: u32,
    /// Files added, modified, or removed this run. Callers gate post-index passes (e.g. LSP) on this.
    #[serde(skip)]
    pub dirty_files: u32,
    /// Files seen during the walk whose extension maps to no supported language
    /// (e.g. `.kt`, `.cpp`). Surfaced so a user on a mixed/monorepo isn't
    /// misled into thinking an unsupported subtree was indexed.
    #[serde(skip_serializing_if = "is_zero")]
    pub files_unsupported: u32,
    /// Per-extension breakdown of `files_unsupported`, descending by count.
    /// Each entry is `(extension_without_dot, count)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unsupported_by_ext: Vec<(String, u32)>,
    /// Files skipped because their name matched the sensitive-file deny-list
    /// (`.env`, `*.pem`, `id_rsa`, ...). Never read, parsed, or stored.
    #[serde(skip_serializing_if = "is_zero")]
    pub files_redacted_skipped: u32,
    /// True when redaction was newly enabled on an index that already held
    /// content predating it; the run was promoted to a full re-index to scrub
    /// stored secrets. Surfaced so callers can warn the user.
    #[serde(skip_serializing_if = "is_false")]
    pub redaction_backfilled: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// Render a human-readable one-block summary of an [`IndexResult`].
///
/// Shared by the `cartog index` CLI output and the `cartog_index` MCP tool so
/// both surfaces report identical file/symbol/edge counts.
#[must_use]
pub fn render_index_summary(r: &IndexResult) -> String {
    let lsp_part = if r.edges_lsp_resolved > 0
        || r.edges_marked_unresolvable > 0
        || r.edges_marked_external > 0
    {
        let mut s = format!(
            " ({} heuristic + {} LSP",
            r.edges_resolved, r.edges_lsp_resolved
        );
        if r.edges_marked_unresolvable > 0 {
            s.push_str(&format!(
                ", {} marked unresolvable",
                r.edges_marked_unresolvable
            ));
        }
        if r.edges_marked_external > 0 {
            s.push_str(&format!(", {} external", r.edges_marked_external));
        }
        s.push(')');
        s
    } else {
        String::new()
    };
    let sym_detail = if r.symbols_modified > 0 || r.symbols_unchanged > 0 || r.symbols_removed > 0 {
        format!(
            " ({} new, {} modified, {} unchanged, {} removed)",
            r.symbols_added, r.symbols_modified, r.symbols_unchanged, r.symbols_removed
        )
    } else {
        String::new()
    };
    let unsupported = if r.files_unsupported > 0 {
        let breakdown = r
            .unsupported_by_ext
            .iter()
            .take(5)
            .map(|(ext, n)| format!("{n} .{ext}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "\n  {} files in unsupported languages not indexed ({breakdown})",
            r.files_unsupported
        )
    } else {
        String::new()
    };
    format!(
        "Indexed {} files ({} skipped, {} removed)\n  {} symbols{}, {} edges ({} resolved{}){}\n",
        r.files_indexed,
        r.files_skipped,
        r.files_removed,
        r.symbols_added + r.symbols_modified + r.symbols_unchanged,
        sym_detail,
        r.edges_added,
        r.edges_resolved + r.edges_lsp_resolved,
        lsp_part,
        unsupported,
    )
}

/// Coarse-grained progress events emitted by [`index_directory`].
///
/// Plain data — no transport or runtime types — so callers (CLI, watcher,
/// MCP, tests) can adapt these to whatever channel they like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressUpdate {
    /// Phase 1 starting: walking the directory tree, collecting candidates.
    Walking,
    /// Phase 2 starting: `total` candidates queued for parallel parse/extract.
    Parsing { total: u32 },
    /// Phase 3 starting: `total` parsed files about to be written in the
    /// indexing transaction.
    Storing { total: u32 },
    /// Phase 4 starting: LSP-based edge resolution. Slowest phase on large
    /// repos and the one most likely to look "stuck", so it gets its own event.
    ResolvingLsp,
}

impl ProgressUpdate {
    /// Lower-case, transport-neutral phase label. The single source of truth
    /// for phase wording — CLI spinners and the MCP progress forwarder both
    /// render this (applying their own casing/format), so the strings can't
    /// drift between crates.
    pub fn label(&self) -> String {
        match self {
            ProgressUpdate::Walking => "scanning files".to_string(),
            ProgressUpdate::Parsing { total } => format!("parsing {total} files"),
            ProgressUpdate::Storing { total } => format!("storing {total} files"),
            ProgressUpdate::ResolvingLsp => "resolving edges with LSP".to_string(),
        }
    }
}

/// Optional progress callback type accepted by [`index_directory`].
///
/// Called synchronously from inside the indexer (never on an async runtime).
/// Implementations must be cheap and non-blocking.
pub type ProgressCallback<'a> = &'a (dyn Fn(ProgressUpdate) + Send + Sync);

/// Optional cooperative-cancellation probe accepted by [`index_directory`].
///
/// Returns `true` when the caller wants the indexer to stop. Polled at phase
/// boundaries and once per file in the storing loop, so worst-case latency is
/// one file's write. When the probe trips, `index_directory` returns
/// `Err` whose root cause string is `"cancelled"` — the MCP layer matches on
/// this to surface a cancelled response. Behavior with `None` is unchanged.
pub type CancelProbe<'a> = &'a (dyn Fn() -> bool + Send + Sync);

/// Index a directory, updating the database incrementally.
///
/// Change detection strategy (in order):
/// 1. `force = true` → re-index everything, no checks
/// 2. Git-based → diff `last_commit..HEAD` to find changed files, skip the rest without reading
/// 3. SHA-256 fallback → read file, hash it, compare to stored hash
///
/// When `progress` is `Some`, the callback fires at each coarse phase boundary
/// (see [`ProgressUpdate`]). Pass `None` for the no-op default — behavior is
/// otherwise identical.
pub fn index_directory(
    db: &Database,
    root: &Path,
    force: bool,
    lsp: bool,
    progress: Option<ProgressCallback<'_>>,
    cancel: Option<CancelProbe<'_>>,
    redact: RedactionConfig,
) -> Result<IndexResult> {
    let emit = |u: ProgressUpdate| {
        if let Some(cb) = progress {
            cb(u);
        }
    };
    let check_cancel = || -> Result<()> {
        if cancel.is_some_and(|c| c()) {
            anyhow::bail!("cancelled");
        }
        Ok(())
    };

    let mut result = IndexResult::default();

    let root = root.canonicalize().context("Failed to resolve root path")?;

    // Redaction policy change forces a full re-index so stored content predating
    // the new policy gets scrubbed (hashes key off raw source, so a plain run
    // would skip every file and leave plaintext behind). Must run before the
    // skip-by-hash / git-diff reads below so they observe the forced value.
    let stored_redact = db.get_metadata("redact_secrets")?;
    let policy_changed = stored_redact
        .as_deref()
        .is_some_and(|v| v != redact.enabled.to_string());
    let mut force = force;
    if policy_changed {
        force = true;
        if redact.enabled {
            result.redaction_backfilled = true;
        }
        // Stale vectors were built from pre-redaction content; drop them so the
        // next `rag index` re-embeds the scrubbed text.
        db.clear_all_embeddings()?;
    }

    // Collect files that should be indexed
    let mut current_files = std::collections::HashSet::new();

    // Track files that were actually re-indexed (for scoped edge resolution)
    let mut dirty_files: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Git-based change detection: get set of files changed since last indexed commit
    let last_commit = if force {
        None
    } else {
        db.get_metadata("last_commit")?
    };
    let changed_files = if force {
        None
    } else {
        git_changed_files(&root, last_commit.as_deref())
    };

    // Pre-fetch stored file hashes in one query so workers can decide
    // skip-by-hash without touching SQLite.
    let stored_hashes = if force {
        std::collections::HashMap::new()
    } else {
        db.all_file_hashes().unwrap_or_default()
    };

    // ── Phase 1: walk + filter candidates (cheap, single-threaded) ──
    check_cancel()?;
    emit(ProgressUpdate::Walking);
    let mut candidates: Vec<(PathBuf, String, &'static str)> = Vec::new();
    let mut unsupported_ext: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for entry in WalkDir::new(&root)
        .follow_links(true)
        .max_depth(50)
        .into_iter()
        .filter_entry(|e| !is_ignored(e))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "directory walk error");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel_path = match path.strip_prefix(&root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        // Sensitive files (.env, *.pem, id_rsa, ...) are never indexed, always.
        // Checked before detect_language so it also catches files with no code
        // extension, and so they count as redacted-skips rather than
        // unsupported. Dropping before current_files.insert lets the removal
        // sweep delete any rows from a prior un-gated index.
        if redact::is_sensitive_file(&rel_path) {
            result.files_redacted_skipped += 1;
            continue;
        }

        let lang = match detect_language(Path::new(&rel_path)) {
            Some(l) => l,
            None => {
                // Tally genuine source files in unsupported languages, but skip
                // cartog's own database sidecars (.cartog.db, -wal, -shm) — they
                // aren't user code and would be noise in the breakdown.
                if let Some(ext) = Path::new(&rel_path).extension().and_then(|e| e.to_str()) {
                    if !is_db_sidecar(&rel_path) {
                        result.files_unsupported += 1;
                        *unsupported_ext.entry(ext.to_ascii_lowercase()).or_insert(0) += 1;
                    }
                }
                continue;
            }
        };

        current_files.insert(rel_path.clone());

        // Git-based skip: files not in the changed set and already indexed stay put.
        if !force {
            if let Some(ref changed) = changed_files {
                if !changed.contains(&rel_path) && stored_hashes.contains_key(&rel_path) {
                    result.files_skipped += 1;
                    continue;
                }
            }
        }

        candidates.push((path.to_path_buf(), rel_path, lang));
    }

    let mut by_ext: Vec<(String, u32)> = unsupported_ext.into_iter().collect();
    by_ext.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    result.unsupported_by_ext = by_ext;

    // ── Phase 2: parallel parse + extract (CPU-bound, rayon-worker pool) ──
    check_cancel()?;
    emit(ProgressUpdate::Parsing {
        total: candidates.len() as u32,
    });
    let parsed: Vec<ParseOutput> = candidates
        .par_iter()
        .map(|(abs, rel, lang)| {
            parse_one_file(
                abs,
                rel,
                lang,
                force,
                stored_hashes.get(rel).map(String::as_str),
                redact,
            )
        })
        .collect();

    // ── Phase 3: sequential DB writes inside one transaction ──
    //
    // All writes from here through the `last_commit` metadata update participate
    // in a single transaction. A panic, error, or hard process exit before
    // `tx.commit()` rolls everything back — the DB never sees a partial Phase 3
    // state (e.g. symbols updated but `files` row stale, or edges resolved
    // against a half-rebuilt symbol set).
    //
    // Inside the transaction, we use the `*_in_tx` variants of the batch
    // helpers — the regular versions issue their own `BEGIN` and would fail
    // here.
    // Names of symbols added this run. Used after the per-file loop to reset
    // resolution_state {2, 3} markers on edges that newly have a matching
    // target — closes the "added b.ts after a.ts's import was marked
    // unresolvable" gap, and the "vendored a dep in-tree so an external edge
    // is now internal" gap.
    let mut added_symbol_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // `total` here is the number of files that will actually be written, not
    // the parsed-vec length: `ParseOutput::Skipped` short-circuits before the
    // DB write and `ParseOutput::Failed` is dropped. Without this filter a
    // warm re-index reports `storing N` where N is the full candidate set.
    let storing_total = parsed
        .iter()
        .filter(|p| matches!(p, ParseOutput::Parsed { .. }))
        .count() as u32;
    check_cancel()?;
    emit(ProgressUpdate::Storing {
        total: storing_total,
    });
    let tx = db.begin_indexing_tx()?;
    for item in parsed {
        check_cancel()?;
        let (rel_path, lang, source, hash, modified, symbols, edges) = match item {
            ParseOutput::Skipped => {
                result.files_skipped += 1;
                continue;
            }
            ParseOutput::Failed => continue,
            ParseOutput::Parsed {
                rel_path,
                lang,
                source,
                hash,
                modified,
                symbols,
                edges,
            } => (rel_path, lang, source, hash, modified, symbols, edges),
        };

        // Force skips Merkle diff: source is unchanged on a policy-change
        // re-index, so a diff would find nothing dirty and skip the content
        // rewrite, leaving stale (un-redacted) rows.
        let old_hashes = db.get_symbol_hashes_for_file(&rel_path)?;
        let has_old_hashes =
            !force && !old_hashes.is_empty() && old_hashes.iter().any(|(_, ch, _)| ch.is_some());

        if has_old_hashes {
            // Merkle diff: surgical updates
            let diff = merkle_diff(&symbols, &old_hashes);

            dirty_files.insert(rel_path.clone());

            // Newly-added symbol names — used post-loop to retry edges that
            // were previously marked unresolvable but now have a matching target.
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
            db.insert_edges_in_tx(&edges)?;
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
                    extract_symbol_content_redacted(&source, sym, redact).map(
                        |(content, header)| (sym.id.clone(), sym.name.clone(), content, header),
                    )
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
            for sym in &symbols {
                added_symbol_names.insert(sym.name.clone());
            }
            db.clear_file_data_in_tx(&rel_path)?;

            db.insert_symbols_in_tx(&symbols)?;
            db.insert_edges_in_tx(&edges)?;

            result.symbols_added += symbols.len() as u32;
            result.edges_added += edges.len() as u32;

            let contents: Vec<(String, String, String, String)> = symbols
                .iter()
                .filter(|sym| sym.kind != cartog_core::SymbolKind::Import)
                .filter_map(|sym| {
                    extract_symbol_content_redacted(&source, sym, redact).map(
                        |(content, header)| (sym.id.clone(), sym.name.clone(), content, header),
                    )
                })
                .collect();
            if !contents.is_empty() {
                db.insert_symbol_contents_in_tx(&contents)?;
            }
        }

        let num_symbols = symbols.len() as u32;

        db.upsert_file(&FileInfo {
            path: rel_path,
            last_modified: modified,
            hash,
            language: lang.to_string(),
            num_symbols,
        })?;

        result.files_indexed += 1;
    }

    // Remove files that no longer exist. Treat deletions as "dirty" so the
    // scoped incremental-repair branch below still runs when the *only* change
    // is a file deletion — otherwise unchanged files keep dangling target_ids
    // and stale in-degrees until the next edit.
    let all_indexed = db.all_files()?;
    for indexed_path in all_indexed {
        if !current_files.contains(&indexed_path) {
            dirty_files.insert(indexed_path.clone());
            db.remove_file_in_tx(&indexed_path)?;
            result.files_removed += 1;
        }
    }

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
        db.invalidate_edges_targeting(&dirty_files)?;
        result.edges_resolved = db.resolve_edges_scoped_in_tx(&dirty_files)?;
        db.compute_in_degrees_scoped(&dirty_files)?;
    }

    // LSP-based resolution for edges the heuristic couldn't resolve.
    // Auto-detected when `lsp` feature is compiled in; silently skipped otherwise.
    // The LSP-side helpers (`unresolved_edges`, `find_symbol_at_location`,
    // `update_edge_target`) all use single-statement execs that participate in
    // the outer transaction — no extra plumbing needed.
    //
    // Skipped on no-op runs: unresolved set is identical to last run, so
    // re-querying the LSP repeats work. Use `--force` to retry.
    #[cfg(feature = "lsp")]
    if lsp && !dirty_files.is_empty() {
        emit(ProgressUpdate::ResolvingLsp);
        let stats = cartog_lsp::lsp_resolve_edges(db, &root, None)?;
        result.edges_lsp_resolved = stats.resolved;
        result.edges_marked_unresolvable = stats.marked_unresolvable;
        result.edges_marked_external = stats.marked_external;
    }
    #[cfg(not(feature = "lsp"))]
    let _ = lsp; // suppress unused warning when feature is off

    // Store the current git commit as last indexed
    if let Some(commit) = git_head_commit(&root) {
        db.set_metadata("last_commit", &commit)?;
    }

    // Record the redaction policy this index was built under so the next run
    // can detect a toggle and force a re-index.
    db.set_metadata("redact_secrets", &redact.enabled.to_string())?;

    result.dirty_files = dirty_files.len() as u32;
    tx.commit()?;
    Ok(result)
}

/// True for cartog's own SQLite database files and their WAL/SHM sidecars,
/// at either the legacy root (`.cartog.db*`) or the new layout
/// (`db.sqlite*`). These are excluded from the unsupported-language tally.
fn is_db_sidecar(rel_path: &str) -> bool {
    let name = Path::new(rel_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let stems = [cartog_db::LEGACY_DB_FILE, cartog_db::DB_FILENAME];
    stems
        .iter()
        .any(|stem| name == *stem || name.starts_with(&format!("{stem}-")))
}

/// Decides whether a walkdir entry should be excluded from indexing.
///
/// Only directories can be ignored — files are always accepted. Common non-code
/// directories (hidden dirs, `node_modules`, `vendor`, …) are excluded via
/// [`is_ignored_dirname`]. `var` and `builds` are only excluded at depth 1
/// (project root); a nested `src/var` is valid application code.
fn is_ignored(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();

    // Skip hidden directories and common non-code directories
    if entry.file_type().is_dir() {
        // "var" and "builds" are only ignored at the project root (depth 1).
        // A nested path like `src/var` is valid application code.
        if matches!(name.as_ref(), "var" | "builds") && entry.depth() != 1 {
            return false;
        }
        return is_ignored_dirname(&name);
    }

    false
}

/// Check if a directory name should be ignored during indexing.
///
/// Shared between the walkdir-based indexer and the file watcher.
pub fn is_ignored_dirname(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "node_modules"
            | "__pycache__"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".tox"
            | ".venv"
            | "venv"
            | ".env"
            | "env"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | "vendor"
            | "var"
            | "builds"
    ) || name.starts_with('.')
}

fn file_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn file_modified(path: &Path) -> f64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ── Symbol dedup ──

/// Disambiguate symbols with colliding stable IDs by appending `:N` suffixes.
///
/// When two symbols in the same file have the same `file:kind:qualified_name`
/// (e.g., conditional function definitions), the second occurrence gets `:2`, third `:3`, etc.
/// Edge source_ids and parent_ids are updated to match.
fn dedup_symbol_ids(symbols: &mut [Symbol], edges: &mut [cartog_core::Edge]) {
    use std::collections::{HashMap, HashSet};

    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut renames: HashMap<String, String> = HashMap::new();

    for sym in symbols.iter_mut() {
        let count = seen.entry(sym.id.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            let old_id = sym.id.clone();
            sym.id = format!("{old_id}:{count}");
            // First-rename wins: edges originally pointing at the collided id
            // (which the extractor produced without knowing which instance was
            // the owner) get attributed to the first renamed instance. The
            // zero-th instance keeps the short id and keeps its own edges only
            // if none collided — any ambiguity is resolved by sending the
            // ambiguous edges to the first-rename bucket, leaving the unrenamed
            // instance clean.
            renames.entry(old_id).or_insert_with(|| sym.id.clone());
        }
    }

    if !renames.is_empty() {
        for edge in edges.iter_mut() {
            if let Some(new_id) = renames.get(&edge.source_id) {
                edge.source_id = new_id.clone();
            }
        }

        for sym in symbols.iter_mut() {
            if let Some(ref pid) = sym.parent_id {
                if let Some(new_id) = renames.get(pid) {
                    sym.parent_id = Some(new_id.clone());
                }
            }
        }
    }

    // Invariant: after dedup, every edge.source_id must correspond to a
    // surviving symbol id. Broken invariants here cause foreign-key cascades
    // later and silent data loss, so bail loudly in debug builds.
    debug_assert!(
        {
            let ids: HashSet<&str> = symbols.iter().map(|s| s.id.as_str()).collect();
            edges.iter().all(|e| ids.contains(e.source_id.as_str()))
        },
        "dedup_symbol_ids left an edge with a dangling source_id"
    );
}

mod content;
mod git;
mod merkle;
mod redact;

pub(crate) use content::extract_symbol_content_redacted;
pub use git::git_recently_changed_files;
pub(crate) use git::{git_changed_files, git_head_commit};
pub(crate) use merkle::{compute_merkle_hashes, merkle_diff};
pub use redact::RedactionConfig;

/// Shared scenario bodies for the indexing benchmarks.
///
/// Both `cartog-indexer/benches/indexing.rs` (ONNX-free) and
/// `cartog/benches/queries.rs` (pulls in `cartog-rag`/ONNX) reuse these so the
/// timed work cannot drift between the two `[[bench]]` targets. The criterion
/// wiring stays in each bench (criterion is only a dev-dependency).
///
/// Each scenario function performs exactly one timed unit of work and returns
/// the [`IndexResult`] so the caller can hand it to `black_box`.
#[doc(hidden)]
pub mod bench_support {
    use super::{index_directory, IndexResult};
    use cartog_core::FileInfo;
    use cartog_db::Database;
    use std::path::{Path, PathBuf};

    /// Language tags for the per-language indexing benchmark, paired with their
    /// `benchmarks/fixtures/webapp_<tag>` directory name. Each exercises a
    /// distinct tree-sitter grammar + extractor, which is where indexing cost
    /// actually varies by language.
    pub const FIXTURE_LANGS: [&str; 10] = [
        "py", "ts", "go", "rs", "rb", "java", "php", "dart", "swift", "kt",
    ];

    /// Absolute path to `benchmarks/fixtures`, relative to either bench crate.
    fn fixtures_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("benchmarks")
            .join("fixtures")
    }

    /// Path to the `webapp_py` fixture — the dense default corpus used by the
    /// language-agnostic incremental scenarios and the query benches.
    ///
    /// # Panics
    ///
    /// Panics if the fixture directory is missing — benches are always run
    /// from a checkout that includes `benchmarks/`, so its absence is a bug.
    #[must_use]
    pub fn fixture_path() -> PathBuf {
        fixture_for("py")
    }

    /// Path to the `webapp_<lang>` fixture for one language tag.
    ///
    /// # Panics
    ///
    /// Panics if the fixture directory is missing (a checkout/setup bug).
    #[must_use]
    pub fn fixture_for(lang: &str) -> PathBuf {
        let dir = fixtures_dir().join(format!("webapp_{lang}"));
        assert!(
            dir.exists(),
            "expected fixture at {dir:?}; run from a checkout that includes benchmarks/"
        );
        dir
    }

    /// All language fixtures as `(lang_tag, path)` pairs, for parameterizing the
    /// full-index benchmark across every grammar.
    ///
    /// # Panics
    ///
    /// Panics if any fixture directory is missing (a checkout/setup bug).
    #[must_use]
    pub fn all_fixtures() -> Vec<(&'static str, PathBuf)> {
        FIXTURE_LANGS
            .iter()
            .map(|&lang| (lang, fixture_for(lang)))
            .collect()
    }

    /// Open a fresh in-memory DB and full-index the fixture (force = true).
    ///
    /// This is the timed body of the `index_full_force` benchmark; it returns
    /// the [`IndexResult`] so the caller can `black_box` it.
    ///
    /// # Panics
    ///
    /// Panics if opening the DB or indexing fails; both are setup invariants
    /// in a benchmark, not recoverable conditions.
    pub fn full_force(fixture: &Path) -> IndexResult {
        let db = Database::open_memory().expect("open in-memory DB");
        index_directory(
            &db,
            fixture,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .expect("full index")
    }

    /// Open an in-memory DB and full-index the fixture, returning the DB.
    ///
    /// Used to seed the `noop` and `one_file` benchmarks outside their timed
    /// loop (criterion setup, not measured).
    ///
    /// # Panics
    ///
    /// Panics if opening the DB or the seed index fails (setup invariants).
    #[must_use]
    pub fn seed(fixture: &Path) -> Database {
        let db = Database::open_memory().expect("open in-memory DB");
        index_directory(
            &db,
            fixture,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .expect("seed index");
        db
    }

    /// Re-index with no changes: every stored hash matches, all files skipped.
    ///
    /// # Panics
    ///
    /// Panics if re-indexing fails (a setup invariant).
    pub fn noop(db: &Database, fixture: &Path) -> IndexResult {
        index_directory(
            db,
            fixture,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .expect("noop re-index")
    }

    /// Invalidate one file's stored hash, then re-index so it re-parses and
    /// exercises the Merkle-diff path.
    ///
    /// # Panics
    ///
    /// Panics if upserting the file or re-indexing fails (setup invariants).
    pub fn one_file(db: &Database, fixture: &Path) -> IndexResult {
        db.upsert_file(&FileInfo {
            path: "auth/service.py".to_string(),
            last_modified: 0.0,
            hash: "invalidated".to_string(),
            language: "python".to_string(),
            num_symbols: 0,
        })
        .expect("invalidate file hash");
        index_directory(
            db,
            fixture,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .expect("incremental re-index")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_hash_deterministic() {
        let h1 = file_hash("def foo(): pass");
        let h2 = file_hash("def foo(): pass");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_file_hash_different_content() {
        let h1 = file_hash("def foo(): pass");
        let h2 = file_hash("def bar(): pass");
        assert_ne!(h1, h2);
    }

    #[test]
    fn extract_with_cached_returns_none_for_unregistered_language() {
        assert!(extract_with_cached("klingon", "irrelevant", "a.kl").is_none());
    }

    #[test]
    fn index_summary_reports_file_symbol_and_edge_counts() {
        let r = IndexResult {
            files_indexed: 3,
            files_skipped: 1,
            symbols_added: 12,
            edges_added: 20,
            edges_resolved: 18,
            ..Default::default()
        };
        let s = render_index_summary(&r);
        assert!(s.contains("Indexed 3 files (1 skipped, 0 removed)"));
        assert!(s.contains("12 symbols"));
        assert!(s.contains("20 edges (18 resolved)"));
    }

    #[test]
    fn index_summary_shows_detail_for_removal_only_delta() {
        // A pass that only removes symbols (no new/modified/unchanged) must
        // still report the removed count, not a bare "0 symbols".
        let r = IndexResult {
            files_indexed: 1,
            symbols_removed: 4,
            ..Default::default()
        };
        let s = render_index_summary(&r);
        assert!(
            s.contains("4 removed"),
            "removal-only delta must surface the removed count: {s}"
        );
    }

    #[test]
    fn index_summary_breaks_out_lsp_resolution_when_present() {
        let r = IndexResult {
            files_indexed: 1,
            symbols_added: 5,
            edges_added: 10,
            edges_resolved: 6,
            edges_lsp_resolved: 3,
            edges_marked_external: 1,
            ..Default::default()
        };
        let s = render_index_summary(&r);
        assert!(s.contains("9 resolved"), "6 heuristic + 3 LSP = 9");
        assert!(s.contains("6 heuristic + 3 LSP"));
        assert!(s.contains("1 external"));
    }

    #[test]
    fn index_summary_lists_unsupported_languages() {
        let r = IndexResult {
            files_indexed: 2,
            files_unsupported: 4,
            unsupported_by_ext: vec![("kt".into(), 3), ("cpp".into(), 1)],
            ..Default::default()
        };
        let s = render_index_summary(&r);
        assert!(s.contains("4 files in unsupported languages"));
        assert!(s.contains("3 .kt"));
    }

    #[test]
    fn extract_with_cached_extracts_for_known_language() {
        let result = extract_with_cached("python", "def foo():\n    pass\n", "a.py")
            .expect("python is registered")
            .expect("valid source extracts");
        assert!(
            result.symbols.iter().any(|s| s.name == "foo"),
            "expected `foo` among extracted symbols"
        );
    }

    #[test]
    fn test_is_ignored_directories() {
        let tmp = std::env::temp_dir().join("cartog_test_ignored");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let ignored_dirs = [
            ".git",
            "node_modules",
            "__pycache__",
            "target",
            "dist",
            "build",
            ".venv",
            "var",
            "builds",
        ];
        let allowed_dirs = ["src", "lib", "tests", "docs"];

        for name in ignored_dirs.iter().chain(allowed_dirs.iter()) {
            std::fs::create_dir_all(tmp.join(name)).unwrap();
        }

        let entries: Vec<_> = WalkDir::new(&tmp)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .collect();

        for entry in &entries {
            let name = entry.file_name().to_string_lossy();
            if ignored_dirs.contains(&name.as_ref()) {
                assert!(is_ignored(entry), "{name} should be ignored");
            }
            if allowed_dirs.contains(&name.as_ref()) {
                assert!(!is_ignored(entry), "{name} should NOT be ignored");
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_var_and_builds_not_ignored_when_nested() {
        // "var" and "builds" must only be ignored at depth 1 (project root).
        // A nested path like `src/var` is valid application code.
        let tmp = std::env::temp_dir().join("cartog_test_nested_var");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src/var")).unwrap();
        std::fs::create_dir_all(tmp.join("src/builds")).unwrap();

        let entries: Vec<_> = WalkDir::new(&tmp)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .collect();

        let nested_var = entries.iter().find(|e| e.path() == tmp.join("src/var"));
        let nested_builds = entries.iter().find(|e| e.path() == tmp.join("src/builds"));

        assert!(nested_var.is_some());
        assert!(
            !is_ignored(nested_var.unwrap()),
            "src/var should NOT be ignored"
        );
        assert!(nested_builds.is_some());
        assert!(
            !is_ignored(nested_builds.unwrap()),
            "src/builds should NOT be ignored"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_git_changed_files_no_commit() {
        // When last_commit is None, should return None (first index → full scan)
        let result = git_changed_files(Path::new("."), None);
        assert!(result.is_none());
    }

    #[test]
    fn test_git_changed_files_invalid_commit() {
        // A commit hash that doesn't exist should return None (fallback to hash)
        let result = git_changed_files(
            Path::new("."),
            Some("0000000000000000000000000000000000000000"),
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_git_changed_files_valid_head() {
        // If we diff HEAD against HEAD, the changed set should be empty
        // (only working tree / untracked files would appear)
        let head = git_head_commit(Path::new("."));
        if let Some(commit) = head {
            let result = git_changed_files(Path::new("."), Some(&commit));
            // Should return Some (valid commit), though the set may contain untracked/modified files
            assert!(result.is_some());
        }
    }

    #[test]
    fn db_sidecars_are_recognized() {
        assert!(is_db_sidecar(".cartog.db"));
        assert!(is_db_sidecar(".cartog.db-wal"));
        assert!(is_db_sidecar(".cartog.db-shm"));
        assert!(is_db_sidecar("sub/db.sqlite"));
        assert!(is_db_sidecar("db.sqlite-wal"));
        assert!(!is_db_sidecar("main.rs"));
        assert!(!is_db_sidecar("app.dart"));
    }

    #[test]
    fn unsupported_files_are_counted_not_silently_dropped() {
        use cartog_db::Database;
        // TempDir names start with '.', which the walker treats as hidden and
        // prunes — nest a non-dot project dir so the walk descends into it.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("proj");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("b.cs"), "class P {}\n").unwrap();
        std::fs::write(dir.join("c.cs"), "class Q {}\n").unwrap();
        std::fs::write(dir.join("d.cpp"), "int main() {}\n").unwrap();
        // cartog's own DB sidecars must NOT count as unsupported languages.
        std::fs::write(dir.join(".cartog.db"), "x").unwrap();
        std::fs::write(dir.join(".cartog.db-wal"), "x").unwrap();

        let db = Database::open_memory().unwrap();
        let r = index_directory(
            &db,
            &dir,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        assert_eq!(r.files_indexed, 1, "only a.rs is supported");
        assert_eq!(
            r.files_unsupported, 3,
            "2 csharp + 1 cpp, db sidecars excluded"
        );
        // Descending by count, ties broken alphabetically.
        assert_eq!(
            r.unsupported_by_ext,
            vec![("cs".to_string(), 2), ("cpp".to_string(), 1)]
        );
    }

    #[test]
    fn test_index_directory_force() {
        use cartog_db::Database;

        let db = Database::open_memory().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/auth");

        if fixtures.exists() {
            // First index
            let r1 = index_directory(
                &db,
                &fixtures,
                false,
                false,
                None,
                None,
                crate::RedactionConfig::disabled(),
            )
            .unwrap();
            assert!(r1.files_indexed > 0);
            assert!(r1.dirty_files > 0);

            // Second index without force — should skip all files (no-op)
            let r2 = index_directory(
                &db,
                &fixtures,
                false,
                false,
                None,
                None,
                crate::RedactionConfig::disabled(),
            )
            .unwrap();
            assert_eq!(r2.files_indexed, 0);
            assert!(r2.files_skipped > 0);
            assert_eq!(
                r2.dirty_files, 0,
                "no-op reindex must report zero dirty files — gates the LSP pass"
            );
            assert_eq!(
                r2.edges_lsp_resolved, 0,
                "no-op reindex must not run LSP resolution"
            );

            // Force re-index — dirty_files matches files_indexed
            let r3 = index_directory(
                &db,
                &fixtures,
                true,
                false,
                None,
                None,
                crate::RedactionConfig::disabled(),
            )
            .unwrap();
            assert_eq!(r3.files_indexed, r1.files_indexed);
            assert_eq!(r3.files_skipped, 0);
            assert_eq!(r3.dirty_files, r3.files_indexed);
        }
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn test_noop_reindex_does_not_run_lsp() {
        // Regression guard: no dirty files → no LSP pass.
        use cartog_db::Database;

        let db = Database::open_memory().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/auth");
        if !fixtures.exists() {
            return;
        }

        // Prime, then re-run. Don't assert LSP found anything (depends on
        // whether pyright is on PATH in CI) — only that the second pass skips it.
        let _ = index_directory(
            &db,
            &fixtures,
            false,
            true,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        let r2 = index_directory(
            &db,
            &fixtures,
            false,
            true,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();
        assert_eq!(r2.dirty_files, 0);
        assert_eq!(r2.edges_lsp_resolved, 0);
    }

    #[test]
    fn test_added_symbol_reopens_unresolvable_edges() {
        // Name-keyed reset: a new symbol whose name matches a state=2 edge
        // returns the edge to state=0 (or state=1 if the heuristic resolves it).
        use cartog_db::Database;

        let tmp = tempfile::tempdir().unwrap();
        // Rust's tempfile creates `.tmpXXXX` directories on macOS — the leading
        // dot makes is_ignored() reject the walk root. Nest a non-dotted child.
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "def caller():\n    find_user('x')\n").unwrap();

        let db = Database::open_memory().unwrap();
        let r1 = index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();
        assert!(
            r1.files_indexed >= 1,
            "expected a.py to index, got {:?}",
            r1
        );

        let before = db.unresolved_edges().unwrap();
        let find_user = before
            .iter()
            .find(|e| e.target_name == "find_user")
            .expect("find_user edge should exist as unresolved after first index");
        let edge_id = find_user.edge_id;
        db.mark_edge_unresolvable(edge_id).unwrap();
        assert!(db.is_edge_unresolvable(edge_id).unwrap());

        // Adding b.py with find_user definition should reopen the marker.
        std::fs::write(root.join("b.py"), "def find_user(name):\n    return None\n").unwrap();
        index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        assert!(
            !db.is_edge_unresolvable(edge_id).unwrap(),
            "edge must not stay state=2 after a matching target appears"
        );
    }

    #[test]
    fn reindex_invalidates_embedding_of_modified_symbol() {
        // Drift regression: a symbol whose body changes keeps its stable id, so
        // its old embedding must be dropped on re-index — otherwise
        // symbols_needing_embeddings() skips it and the vector stays stale.
        use cartog_db::Database;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        // Body must exceed MIN_CONTENT_BYTES (50) so content is extracted.
        std::fs::write(
            root.join("a.py"),
            "def greet(name):\n    message = 'hello there ' + name\n    return message\n",
        )
        .unwrap();

        let db = Database::open_memory().unwrap();
        let idx = |db: &Database| {
            index_directory(
                db,
                &root,
                false,
                false,
                None,
                None,
                crate::RedactionConfig::disabled(),
            )
            .unwrap()
        };
        idx(&db);

        // Simulate a prior `rag index`: embed the only content symbol.
        let needing = db.symbols_needing_embeddings().unwrap();
        assert_eq!(needing.len(), 1, "expected greet() to need embedding");
        let greet_id = needing[0].clone();
        let bytes: Vec<u8> = vec![0.0f32; 384]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let eid = db.get_or_create_embedding_id(&greet_id).unwrap();
        db.upsert_embedding(eid, &bytes).unwrap();
        assert!(db.symbols_needing_embeddings().unwrap().is_empty());

        // Edit the body (same name/kind/file → same stable id) and re-index.
        std::fs::write(
            root.join("a.py"),
            "def greet(name):\n    message = 'goodbye and farewell ' + name\n    return message\n",
        )
        .unwrap();
        idx(&db);

        // The drifted embedding is gone and the symbol re-enters the queue.
        assert!(
            !db.has_embedding(&greet_id).unwrap(),
            "modified symbol's stale embedding must be cleared"
        );
        assert_eq!(
            db.symbols_needing_embeddings().unwrap(),
            vec![greet_id],
            "modified symbol must re-enter the needs-embedding set"
        );
    }

    #[test]
    fn reindex_keeps_embedding_of_unchanged_sibling() {
        // The drift-clear is scoped to modified symbols: a sibling whose own
        // content is untouched keeps its embedding even when the file is dirty.
        use cartog_db::Database;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        let greet_v1 =
            "def greet(name):\n    message = 'hello there ' + name\n    return message\n";
        let farewell = "def farewell(name):\n    message = 'goodbye and take care ' + name\n    return message\n";
        std::fs::write(root.join("a.py"), format!("{greet_v1}\n\n{farewell}")).unwrap();

        let db = Database::open_memory().unwrap();
        let idx = |db: &Database| {
            index_directory(
                db,
                &root,
                false,
                false,
                None,
                None,
                crate::RedactionConfig::disabled(),
            )
            .unwrap()
        };
        idx(&db);

        let bytes: Vec<u8> = vec![0.0f32; 384]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        for id in db.symbols_needing_embeddings().unwrap() {
            let eid = db.get_or_create_embedding_id(&id).unwrap();
            db.upsert_embedding(eid, &bytes).unwrap();
        }
        let farewell_id = db
            .all_content_symbol_ids()
            .unwrap()
            .into_iter()
            .find(|id| id.contains("farewell"))
            .expect("farewell symbol id");

        // Change only greet(); farewell()'s own content is identical.
        let greet_v2 =
            "def greet(name):\n    message = 'goodbye and farewell ' + name\n    return message\n";
        std::fs::write(root.join("a.py"), format!("{greet_v2}\n\n{farewell}")).unwrap();
        idx(&db);

        assert!(
            db.has_embedding(&farewell_id).unwrap(),
            "unchanged sibling must keep its embedding"
        );
    }

    #[test]
    fn reindex_drops_stale_content_when_modified_body_shrinks_below_threshold() {
        // A modified symbol whose new body no longer yields content must not keep
        // its pre-edit content row (else it re-embeds stale text forever).
        use cartog_db::Database;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        // v1 body is well over MIN_CONTENT_BYTES (50) so content is stored.
        std::fs::write(
            root.join("a.py"),
            "def greet(name):\n    message = 'a long enough greeting for ' + name\n    return message\n",
        )
        .unwrap();

        let db = Database::open_memory().unwrap();
        let idx = |db: &Database| {
            index_directory(
                db,
                &root,
                false,
                false,
                None,
                None,
                crate::RedactionConfig::disabled(),
            )
            .unwrap()
        };
        idx(&db);
        let greet_id = db.symbols_needing_embeddings().unwrap()[0].clone();
        assert!(db.get_symbol_content(&greet_id).unwrap().is_some());

        // Shrink the body below the content threshold; same stable id (same name).
        std::fs::write(root.join("a.py"), "def greet(name):\n    pass\n").unwrap();
        idx(&db);

        assert!(
            db.get_symbol_content(&greet_id).unwrap().is_none(),
            "stale content row must be deleted when the modified body loses content"
        );
        assert!(
            !db.symbols_needing_embeddings().unwrap().contains(&greet_id),
            "a symbol with no content must not be queued for embedding"
        );
    }

    #[test]
    fn test_force_reindex_does_not_inherit_sticky_markers() {
        // End-to-end contract: --force is the documented escape hatch for
        // "retry everything". Under --force, every file is re-parsed and
        // `clear_edges_for_file` / `clear_file_data_in_tx` wipe the edge
        // rows before `resolve_edges` runs — so the post-force edges have
        // fresh auto-increment IDs at default state=0 regardless of what
        // state the pre-force edges held. This test exercises that path
        // through real indexing; the targeted unit test for the SQL filter
        // (`IN (2, 3)`) lives in cartog-db
        // (test_reset_all_unresolvable_resets_state_two_and_three).
        use cartog_db::Database;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("a.py"),
            "def caller():\n    find_x()\n    find_ext()\n",
        )
        .unwrap();

        let db = Database::open_memory().unwrap();
        index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        let pre = db.unresolved_edges().unwrap();
        let find_x_id = pre
            .iter()
            .find(|e| e.target_name == "find_x")
            .expect("find_x edge should exist")
            .edge_id;
        let find_ext_id = pre
            .iter()
            .find(|e| e.target_name == "find_ext")
            .expect("find_ext edge should exist")
            .edge_id;
        db.mark_edge_unresolvable(find_x_id).unwrap();
        db.mark_edge_external(find_ext_id).unwrap();

        // --force = true: rebuilds edges with fresh IDs, must NOT inherit state {2, 3}.
        index_directory(
            &db,
            &root,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();
        let post = db.unresolved_edges().unwrap();
        assert!(
            post.iter().any(|e| e.target_name == "find_x"),
            "after --force, find_x must be back in unresolved_edges (state=0)"
        );
        assert!(
            post.iter().any(|e| e.target_name == "find_ext"),
            "after --force, find_ext must be back in unresolved_edges (state=0)"
        );
    }

    #[test]
    fn test_name_keyed_reset_reopens_external_edges() {
        // If an edge was marked state=3 (target outside the indexed root) and
        // the user then vendors that target in-tree, indexing the new file
        // must reopen the external marker so LSP retries it.
        use cartog_db::Database;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "def caller():\n    vendored_helper()\n").unwrap();

        let db = Database::open_memory().unwrap();
        index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        let unresolved = db.unresolved_edges().unwrap();
        let edge_id = unresolved
            .iter()
            .find(|e| e.target_name == "vendored_helper")
            .expect("vendored_helper edge should exist")
            .edge_id;
        db.mark_edge_external(edge_id).unwrap();
        assert_eq!(db.edge_resolution_state(edge_id).unwrap(), 3);

        // Vendor the dep in-tree.
        std::fs::write(
            root.join("vendor.py"),
            "def vendored_helper():\n    return 1\n",
        )
        .unwrap();
        index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        // After the name-keyed reset, the edge is reopened (state=0) and the
        // heuristic resolver runs in the same indexing pass — `vendored_helper`
        // is now defined in the same directory, so the same-dir heuristic
        // resolves the edge to state=1. Asserting state=1 (not just "not 3")
        // catches a future regression that breaks the reset-then-resolve
        // pipeline (e.g. silently re-marking as state=2).
        assert_eq!(
            db.edge_resolution_state(edge_id).unwrap(),
            1,
            "vendored target should reopen the external marker AND be resolved by the heuristic"
        );
    }

    #[test]
    fn test_noop_reindex_preserves_unresolvable_and_external_markers() {
        // Defensive: a no-op reindex must not touch state {2, 3} markers — no
        // spurious resets (would burn the gate), no spurious re-marks.
        use cartog_db::Database;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("a.py"),
            "def caller():\n    find_x()\n    find_y()\n",
        )
        .unwrap();

        let db = Database::open_memory().unwrap();
        index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        let unresolved = db.unresolved_edges().unwrap();
        let burned = unresolved
            .iter()
            .find(|e| e.target_name == "find_x")
            .expect("find_x edge should exist");
        let ext = unresolved
            .iter()
            .find(|e| e.target_name == "find_y")
            .expect("find_y edge should exist");
        db.mark_edge_unresolvable(burned.edge_id).unwrap();
        db.mark_edge_external(ext.edge_id).unwrap();

        // No file changes → reindex is a no-op → markers survive.
        let r = index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();
        assert_eq!(r.dirty_files, 0);

        assert_eq!(
            db.edge_resolution_state(burned.edge_id).unwrap(),
            2,
            "no-op reindex must not reset state=2"
        );
        assert_eq!(
            db.edge_resolution_state(ext.edge_id).unwrap(),
            3,
            "no-op reindex must not reset state=3"
        );
    }

    // ── Dedup tests ──

    #[test]
    fn test_dedup_3way_collision_preserves_invariant() {
        // Three symbols with the same stable id — simulates conditional
        // redefinitions (e.g. `if/elif/else: def foo`).
        let mk_sym = || {
            cartog_core::Symbol::new(
                "foo",
                cartog_core::SymbolKind::Function,
                "test.py",
                1,
                2,
                0,
                10,
                None,
            )
        };
        let base_id = mk_sym().id.clone();
        let mut symbols = vec![mk_sym(), mk_sym(), mk_sym()];
        let mut edges = vec![
            cartog_core::Edge::new(
                base_id.clone(),
                "bar",
                cartog_core::EdgeKind::Calls,
                "test.py",
                1,
            ),
            cartog_core::Edge::new(
                base_id.clone(),
                "baz",
                cartog_core::EdgeKind::Calls,
                "test.py",
                2,
            ),
        ];

        dedup_symbol_ids(&mut symbols, &mut edges);

        // All three ids must now be distinct.
        let ids: std::collections::HashSet<_> = symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "3-way collision should produce 3 unique ids");

        // First instance keeps the short id; 2nd and 3rd get numeric suffixes.
        assert_eq!(symbols[0].id, base_id);
        assert_eq!(symbols[1].id, format!("{base_id}:2"));
        assert_eq!(symbols[2].id, format!("{base_id}:3"));

        // Invariant: every edge.source_id must resolve to a surviving symbol.
        for edge in &edges {
            assert!(
                ids.contains(edge.source_id.as_str()),
                "edge source_id {:?} has no matching symbol after dedup",
                edge.source_id
            );
        }
    }

    #[test]
    fn test_dedup_no_collision_leaves_ids_unchanged() {
        let mut symbols = vec![
            cartog_core::Symbol::new(
                "a",
                cartog_core::SymbolKind::Function,
                "f.py",
                1,
                2,
                0,
                10,
                None,
            ),
            cartog_core::Symbol::new(
                "b",
                cartog_core::SymbolKind::Function,
                "f.py",
                3,
                4,
                11,
                20,
                None,
            ),
        ];
        let id_a = symbols[0].id.clone();
        let id_b = symbols[1].id.clone();
        let mut edges: Vec<cartog_core::Edge> = vec![];
        dedup_symbol_ids(&mut symbols, &mut edges);
        assert_eq!(symbols[0].id, id_a);
        assert_eq!(symbols[1].id, id_b);
    }

    // ── Merkle hashing tests ──

    #[test]
    fn test_compute_merkle_hashes_populates_fields() {
        let source = "def foo():\n    pass\n";
        let mut symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            2,
            0,
            source.len() as u32,
            None,
        )];

        compute_merkle_hashes(&mut symbols, source);

        assert!(symbols[0].content_hash.is_some());
        assert!(symbols[0].subtree_hash.is_some());
    }

    #[test]
    fn test_merkle_hashes_stable_across_position_changes() {
        let source_v1 = "def foo():\n    pass\n";
        let source_v2 = "\n\ndef foo():\n    pass\n";

        let mut sym_v1 = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            2,
            0,
            source_v1.len() as u32,
            None,
        )];
        let mut sym_v2 = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            3,
            4,
            2,
            source_v2.len() as u32,
            None,
        )];

        compute_merkle_hashes(&mut sym_v1, source_v1);
        compute_merkle_hashes(&mut sym_v2, source_v2);

        // content_hash depends on body text — different offset means different body slice
        // but if the body text is the same, hashes should match
        // Here the body text is the same "def foo():\n    pass\n"
        assert_eq!(sym_v1[0].content_hash, sym_v2[0].content_hash);
    }

    #[test]
    fn test_merkle_diff_detects_added_symbol() {
        let old_hashes: Vec<(String, Option<String>, Option<String>)> = vec![];

        let mut new_symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            5,
            0,
            50,
            None,
        )];
        new_symbols[0].content_hash = Some("abc".to_string());
        new_symbols[0].subtree_hash = Some("def".to_string());

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 0);
        assert_eq!(diff.modified.len(), 0);
    }

    #[test]
    fn test_merkle_diff_detects_removed_symbol() {
        let old_hashes = vec![(
            "test.py:function:foo".to_string(),
            Some("abc".to_string()),
            Some("def".to_string()),
        )];

        let new_symbols: Vec<cartog_core::Symbol> = vec![];

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.added.len(), 0);
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0], "test.py:function:foo");
    }

    #[test]
    fn test_merkle_diff_detects_unchanged() {
        let old_hashes = vec![(
            "test.py:function:foo".to_string(),
            Some("abc".to_string()),
            Some("def".to_string()),
        )];

        let mut new_symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            5,
            0,
            50,
            None,
        )];
        new_symbols[0].content_hash = Some("abc".to_string());
        new_symbols[0].subtree_hash = Some("def".to_string());

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.unchanged, 1);
        assert_eq!(diff.added.len(), 0);
        assert_eq!(diff.modified.len(), 0);
    }

    #[test]
    fn test_merkle_diff_detects_modified() {
        let old_hashes = vec![(
            "test.py:function:foo".to_string(),
            Some("old_hash".to_string()),
            Some("old_subtree".to_string()),
        )];

        let mut new_symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            5,
            0,
            50,
            None,
        )];
        new_symbols[0].content_hash = Some("new_hash".to_string());
        new_symbols[0].subtree_hash = Some("new_subtree".to_string());

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(diff.unchanged, 0);
    }

    // ── Integration test: full incremental pipeline ──

    #[test]
    fn test_incremental_merkle_diff_pipeline() {
        use cartog_db::Database;

        let tmp = tempfile::TempDir::new().unwrap();
        // Create a non-dot subdirectory (tempfile may create .tmpXXX on macOS,
        // which is_ignored_dirname skips)
        let dir = tmp.path().join("project");
        std::fs::create_dir(&dir).unwrap();

        // Initial files
        let a_py = dir.join("a.py");
        let b_py = dir.join("b.py");

        std::fs::write(
            &a_py,
            r#"class Greeter:
    def hello(self):
        return "hi"
    def goodbye(self):
        return "bye"
"#,
        )
        .unwrap();

        std::fs::write(
            &b_py,
            r#"from a import Greeter
def main():
    g = Greeter()
    g.hello()
"#,
        )
        .unwrap();

        let db = Database::open_memory().unwrap();

        // ── Index 1: initial full index ──
        let r1 = index_directory(
            &db,
            &dir,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();
        assert_eq!(r1.files_indexed, 2);
        assert!(r1.symbols_added > 0, "should have symbols");

        let outline_a = db.outline("a.py").unwrap();
        assert_eq!(outline_a.len(), 3, "Greeter + hello + goodbye");
        let names_a: Vec<&str> = outline_a.iter().map(|s| s.name.as_str()).collect();
        assert!(names_a.contains(&"Greeter"));
        assert!(names_a.contains(&"hello"));
        assert!(names_a.contains(&"goodbye"));

        // Capture stable IDs
        let hello_id_v1 = outline_a
            .iter()
            .find(|s| s.name == "hello")
            .unwrap()
            .id
            .clone();
        let greeter_id_v1 = outline_a
            .iter()
            .find(|s| s.name == "Greeter")
            .unwrap()
            .id
            .clone();

        // Verify Merkle hashes populated
        let hashes = db.get_symbol_hashes_for_file("a.py").unwrap();
        assert!(
            hashes
                .iter()
                .all(|(_, ch, sh)| ch.is_some() && sh.is_some()),
            "all symbols should have hashes after indexing"
        );

        // ── Index 2: add a function to a.py ──
        std::fs::write(
            &a_py,
            r#"class Greeter:
    def hello(self):
        return "hi"
    def goodbye(self):
        return "bye"

def standalone():
    return "I am new"
"#,
        )
        .unwrap();

        let r2 = index_directory(
            &db,
            &dir,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();
        assert_eq!(r2.files_indexed, 1, "only a.py changed");
        assert!(r2.files_skipped > 0, "b.py should be skipped");
        assert_eq!(r2.symbols_added, 1, "standalone is new");
        assert!(
            r2.symbols_unchanged >= 2,
            "hello and goodbye should be unchanged, got {}",
            r2.symbols_unchanged
        );

        let outline_a2 = db.outline("a.py").unwrap();
        assert_eq!(
            outline_a2.len(),
            4,
            "Greeter + hello + goodbye + standalone"
        );
        assert!(outline_a2.iter().any(|s| s.name == "standalone"));

        // Verify ID stability: hello and Greeter keep same IDs
        let hello_id_v2 = outline_a2
            .iter()
            .find(|s| s.name == "hello")
            .unwrap()
            .id
            .clone();
        let greeter_id_v2 = outline_a2
            .iter()
            .find(|s| s.name == "Greeter")
            .unwrap()
            .id
            .clone();
        assert_eq!(hello_id_v1, hello_id_v2, "hello ID should be stable");
        assert_eq!(greeter_id_v1, greeter_id_v2, "Greeter ID should be stable");

        // ── Index 3: remove goodbye from a.py ──
        std::fs::write(
            &a_py,
            r#"class Greeter:
    def hello(self):
        return "hi"

def standalone():
    return "I am new"
"#,
        )
        .unwrap();

        let r3 = index_directory(
            &db,
            &dir,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();
        assert_eq!(r3.files_indexed, 1);
        assert!(r3.symbols_removed >= 1, "goodbye should be removed");

        let outline_a3 = db.outline("a.py").unwrap();
        assert_eq!(outline_a3.len(), 3, "Greeter + hello + standalone");
        assert!(
            !outline_a3.iter().any(|s| s.name == "goodbye"),
            "goodbye should be gone"
        );

        // hello ID still stable after removal of sibling
        let hello_id_v3 = outline_a3
            .iter()
            .find(|s| s.name == "hello")
            .unwrap()
            .id
            .clone();
        assert_eq!(
            hello_id_v1, hello_id_v3,
            "hello ID stable after sibling removal"
        );
    }

    // ── Integration test: Markdown document indexing ──

    #[test]
    fn test_markdown_indexing_end_to_end() {
        use cartog_db::Database;

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("project");
        std::fs::create_dir(&dir).unwrap();

        let md_file = dir.join("design.md");
        std::fs::write(
            &md_file,
            r#"# Architecture

This document describes the system architecture.

## Authentication

Users authenticate via JWT tokens. The server validates
the token signature and checks expiration before granting access.

## Database

We use PostgreSQL with connection pooling via pgbouncer.
"#,
        )
        .unwrap();

        let db = Database::open_memory().unwrap();
        let result = index_directory(
            &db,
            &dir,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        assert_eq!(result.files_indexed, 1);
        assert!(result.symbols_added >= 3, "should have at least 3 sections");

        // Verify file entry
        let file = db.get_file("design.md").unwrap();
        assert!(file.is_some());
        let file = file.unwrap();
        assert_eq!(file.language, "markdown");

        // Verify Document symbols exist
        let outline = db.outline("design.md").unwrap();
        assert!(
            outline.len() >= 3,
            "should have Architecture, Authentication, Database sections"
        );

        let names: Vec<&str> = outline.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Architecture"),
            "missing Architecture section"
        );
        assert!(
            names.contains(&"Authentication"),
            "missing Authentication section"
        );
        assert!(names.contains(&"Database"), "missing Database section");

        for sym in &outline {
            assert_eq!(sym.kind, cartog_core::SymbolKind::Document);
        }

        // Verify symbol_content is populated
        let auth_sym = outline.iter().find(|s| s.name == "Authentication").unwrap();
        let content = db.get_symbol_content(&auth_sym.id).unwrap();
        assert!(
            content.is_some(),
            "symbol_content should exist for document section"
        );
        let (text, header) = content.unwrap();
        assert!(
            text.contains("JWT tokens"),
            "content should include section body"
        );
        assert!(
            header.contains("Authentication"),
            "header should include section name"
        );
    }

    /// End-to-end Phase 3 atomicity test.
    ///
    /// Drives `index_directory` against a real fixture, then forces the DB to
    /// run out of pages mid-Phase-3 by capping `max_page_count`. The expected
    /// outcome: the index call returns `Err`, AND every Phase-3 write that
    /// happened before the failure has been rolled back. The seed state is
    /// established before the cap, so it must survive the rollback.
    ///
    /// This complements the cartog-db primitive tests (which prove that
    /// `begin_indexing_tx` rolls back correctly): this test proves that
    /// `index_directory` actually opens, uses, and rolls back the
    /// transaction through every code path the real pipeline exercises
    /// (per-file Merkle diff, deletion sweep, edge resolution, in-degree
    /// compute, last_commit metadata write).
    #[test]
    fn test_index_directory_rolls_back_on_disk_full() {
        use cartog_core::SymbolKind;
        use cartog_db::Database;

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("project");
        std::fs::create_dir(&dir).unwrap();

        // Seed file: get into a known indexed state under a generous page budget.
        std::fs::write(dir.join("seed.py"), "def keep_me():\n    return 1\n").unwrap();

        let db = Database::open_memory().unwrap();
        index_directory(
            &db,
            &dir,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .expect("seed index should succeed");

        // Snapshot the seed state so we can assert it is preserved across the
        // failed run.
        let seed_outline = db.outline("seed.py").unwrap();
        let seed_keep_me = seed_outline
            .iter()
            .find(|s| s.name == "keep_me")
            .expect("seed symbol must be present after the first index");
        let seed_keep_me_id = seed_keep_me.id.clone();

        // Add a second file that the next `index_directory` call will try to
        // ingest. Combined with a tight page cap, the new symbol/edge/content
        // writes will hit `SQLITE_FULL` somewhere inside Phase 3.
        std::fs::write(
            dir.join("big.py"),
            // Lots of small symbols: many independent INSERTs, so the page
            // budget runs out partway through and the outer tx must roll back.
            (0..200)
                .map(|i| format!("def fn_{i}():\n    return {i}\n\n"))
                .collect::<String>(),
        )
        .unwrap();

        // Cap the DB at a page count that holds the seed comfortably but
        // cannot fit the second file's worth of symbol/content rows.
        // The exact value is empirical; on macOS APFS with the default 4 KiB
        // page size, ~30 pages is enough to seed but not enough to ingest 200
        // new functions through Phase 3.
        db.set_max_page_count_for_tests(30).unwrap();

        let result = index_directory(
            &db,
            &dir,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        );
        assert!(
            result.is_err(),
            "Phase 3 must fail when SQLite runs out of pages; got Ok({result:?})"
        );

        // Lift the cap so post-mortem queries can run.
        db.set_max_page_count_for_tests(1_000_000).unwrap();

        // Rollback assertions:
        //
        // 1. The seed symbol must still be there (a regression that wiped
        //    pre-existing data is the worst flavor of the original bug).
        // 2. None of the symbols from the failed file may have leaked through:
        //    Phase 3 was wrapped in a single transaction, so partial writes
        //    are rolled back atomically.
        let seed_outline_after = db.outline("seed.py").unwrap();
        assert!(
            seed_outline_after.iter().any(|s| s.id == seed_keep_me_id),
            "seed symbol must survive the rolled-back run"
        );
        let big_outline_after = db.outline("big.py").unwrap();
        assert!(
            big_outline_after.is_empty(),
            "no symbols from the failed Phase 3 may persist; big.py outline: {:?}",
            big_outline_after
                .iter()
                .map(|s| &s.name)
                .collect::<Vec<_>>()
        );

        // The seed symbol is a function — a quick sanity check that the kind
        // wasn't corrupted by partial writes either.
        let kept = seed_outline_after
            .iter()
            .find(|s| s.id == seed_keep_me_id)
            .unwrap();
        assert_eq!(kept.kind, SymbolKind::Function);
    }

    // ── progress callback ──

    fn tiny_python_project() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "def f():\n    pass\n").unwrap();
        std::fs::write(root.join("b.py"), "def g():\n    pass\n").unwrap();
        (tmp, root)
    }

    #[test]
    fn progress_callback_fires_in_phase_order() {
        use cartog_db::Database;
        use std::sync::Mutex;

        let (_tmp, root) = tiny_python_project();
        let db = Database::open_memory().unwrap();

        let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
        let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
        let result = index_directory(
            &db,
            &root,
            true,
            false,
            Some(&cb),
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        assert!(result.files_indexed >= 2);
        let events = events.into_inner().unwrap();
        assert_eq!(events.len(), 3, "expected 3 phase events, got {events:?}");
        assert_eq!(events[0], ProgressUpdate::Walking);
        assert!(matches!(events[1], ProgressUpdate::Parsing { total } if total >= 2));
        assert!(matches!(events[2], ProgressUpdate::Storing { total } if total >= 2));
    }

    #[test]
    fn progress_callback_none_matches_some_for_result() {
        use cartog_db::Database;

        let (_t1, root1) = tiny_python_project();
        let db1 = Database::open_memory().unwrap();
        let r_none = index_directory(
            &db1,
            &root1,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        let (_t2, root2) = tiny_python_project();
        let db2 = Database::open_memory().unwrap();
        let cb = |_: ProgressUpdate| {};
        let r_some = index_directory(
            &db2,
            &root2,
            true,
            false,
            Some(&cb),
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        // Different temp dirs → different file modified-times can shift, but the
        // count-based fields of IndexResult are deterministic on a fresh DB.
        assert_eq!(r_none.files_indexed, r_some.files_indexed);
        assert_eq!(r_none.symbols_added, r_some.symbols_added);
        assert_eq!(r_none.edges_added, r_some.edges_added);
    }

    /// Progress callback emits Walking, then Parsing and Storing with positive
    /// totals. Uses the in-repo fixture (the old env gate was set nowhere, so
    /// the test was a silent no-op).
    #[test]
    fn progress_callback_emits_walking_then_parsing_and_storing() {
        use cartog_db::Database;
        use std::sync::Mutex;

        let (_tmp, root) = tiny_python_project();
        let db = Database::open_memory().unwrap();
        let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
        let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
        index_directory(
            &db,
            &root,
            true,
            false,
            Some(&cb),
            None,
            crate::RedactionConfig::disabled(),
        )
        .unwrap();

        let events = events.into_inner().unwrap();
        assert!(
            matches!(events.first(), Some(ProgressUpdate::Walking)),
            "first progress event must be Walking, got {:?}",
            events.first()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProgressUpdate::Parsing { total } if *total > 0)),
            "must emit a Parsing event with a positive total"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProgressUpdate::Storing { total } if *total > 0)),
            "must emit a Storing event with a positive total"
        );
    }

    #[test]
    fn cancel_probe_returning_true_aborts_with_cancelled_error() {
        use cartog_db::Database;

        let (_tmp, root) = tiny_python_project();
        let db = Database::open_memory().unwrap();

        let probe = || true;
        let err = index_directory(
            &db,
            &root,
            true,
            false,
            None,
            Some(&probe),
            crate::RedactionConfig::disabled(),
        )
        .expect_err("index must abort when probe trips at first phase boundary");
        assert!(
            err.to_string().contains("cancelled"),
            "error must mention cancellation, got: {err}"
        );
    }

    #[test]
    fn cancel_probe_returning_false_runs_to_completion() {
        use cartog_db::Database;

        let (_tmp, root) = tiny_python_project();
        let db = Database::open_memory().unwrap();

        let probe = || false;
        let result = index_directory(
            &db,
            &root,
            true,
            false,
            None,
            Some(&probe),
            crate::RedactionConfig::disabled(),
        )
        .expect("non-cancelling probe must not affect normal indexing");
        assert!(result.files_indexed >= 2);
    }

    #[test]
    fn rerun_after_cancellation_completes_normally() {
        use cartog_db::Database;
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_tmp, root) = tiny_python_project();
        let db = Database::open_memory().unwrap();

        let flag = AtomicBool::new(true);
        let probe = || flag.load(Ordering::SeqCst);
        let _ = index_directory(
            &db,
            &root,
            true,
            false,
            None,
            Some(&probe),
            crate::RedactionConfig::disabled(),
        )
        .expect_err("first run cancels");

        // Flip the probe off — second run must complete and produce a real result.
        flag.store(false, Ordering::SeqCst);
        let result = index_directory(
            &db,
            &root,
            true,
            false,
            None,
            Some(&probe),
            crate::RedactionConfig::disabled(),
        )
        .expect("re-run after cancellation must succeed");
        assert!(result.files_indexed >= 2);
    }

    // ── Secret redaction integration ──

    /// A project whose single function body embeds a GitHub PAT.
    fn project_with_secret() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("conf.py"),
            "def connect():\n    token = \"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"\n    return token\n",
        )
        .unwrap();
        (tmp, root)
    }

    fn only_content(db: &Database) -> String {
        let ids = db.all_content_symbol_ids().unwrap();
        let map = db.get_symbol_contents_batch(&ids).unwrap();
        map.values()
            .map(|(c, _)| c.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn indexing_redacts_secret_in_symbol_content() {
        let (_tmp, root) = project_with_secret();
        let db = Database::open_memory().unwrap();
        index_directory(
            &db,
            &root,
            true,
            false,
            None,
            None,
            RedactionConfig::enabled(),
        )
        .unwrap();

        let content = only_content(&db);
        assert!(content.contains("[REDACTED_SECRET]"));
        assert!(!content.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn redaction_disabled_keeps_secret_verbatim() {
        let (_tmp, root) = project_with_secret();
        let db = Database::open_memory().unwrap();
        index_directory(
            &db,
            &root,
            true,
            false,
            None,
            None,
            RedactionConfig::disabled(),
        )
        .unwrap();

        let content = only_content(&db);
        assert!(content.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(!content.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn redacted_secret_is_not_searchable_in_fts() {
        let (_tmp, root) = project_with_secret();
        let db = Database::open_memory().unwrap();
        index_directory(
            &db,
            &root,
            true,
            false,
            None,
            None,
            RedactionConfig::enabled(),
        )
        .unwrap();

        let hits = db
            .fts5_search("\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"", 10)
            .unwrap();
        assert!(
            hits.is_empty(),
            "secret must not be searchable after redaction"
        );
    }

    #[test]
    fn sensitive_file_is_never_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "def f():\n    pass\n").unwrap();
        // A code-extension file whose name matches the deny-list.
        std::fs::write(root.join("id_rsa"), "PRIVATE KEY").unwrap();
        std::fs::write(root.join(".env"), "API_KEY=ghp_xxxxxxxxxxxxxxxxxxxx").unwrap();

        let db = Database::open_memory().unwrap();
        let r = index_directory(
            &db,
            &root,
            true,
            false,
            None,
            None,
            RedactionConfig::enabled(),
        )
        .unwrap();

        assert_eq!(r.files_indexed, 1, "only a.py indexes");
        assert!(
            r.files_redacted_skipped >= 1,
            "deny-listed files are skipped"
        );
    }

    #[test]
    fn enabling_redaction_on_warm_index_reindexes_and_scrubs() {
        let (_tmp, root) = project_with_secret();
        let db = Database::open_memory().unwrap();

        // First index with redaction OFF: secret is stored verbatim.
        index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            RedactionConfig::disabled(),
        )
        .unwrap();
        assert!(only_content(&db).contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));

        // Plain re-index (no --force) with redaction ON must promote to a full
        // re-index via the policy fingerprint and scrub the stored secret.
        let r = index_directory(
            &db,
            &root,
            false,
            false,
            None,
            None,
            RedactionConfig::enabled(),
        )
        .unwrap();
        assert!(r.redaction_backfilled, "policy change must flag a backfill");
        let content = only_content(&db);
        assert!(content.contains("[REDACTED_SECRET]"));
        assert!(!content.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn content_hash_is_identical_with_redaction_on_vs_off() {
        let (_tmp, root) = project_with_secret();

        let db_on = Database::open_memory().unwrap();
        index_directory(
            &db_on,
            &root,
            true,
            false,
            None,
            None,
            RedactionConfig::enabled(),
        )
        .unwrap();

        let db_off = Database::open_memory().unwrap();
        index_directory(
            &db_off,
            &root,
            true,
            false,
            None,
            None,
            RedactionConfig::disabled(),
        )
        .unwrap();

        // Hashing keys off raw source, so redaction must not perturb identity.
        let mut ids_on = db_on.all_content_symbol_ids().unwrap();
        let mut ids_off = db_off.all_content_symbol_ids().unwrap();
        ids_on.sort();
        ids_off.sort();
        for id in &ids_on {
            let h_on = db_on.get_symbol(id).unwrap().unwrap().content_hash;
            let h_off = db_off.get_symbol(id).unwrap().unwrap().content_hash;
            assert_eq!(h_on, h_off, "content_hash must not depend on redaction");
        }
    }
}
