//! Code indexing and change detection for cartog.
//!
//! Walks a directory tree, detects changed files (git diff, SHA-256 hash, or force),
//! extracts symbols and edges via [`cartog_languages`], and writes results to
//! [`cartog_db`]. Uses Merkle tree hashing for surgical symbol-level updates.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = ""]
#![doc = include_str!("../README.md")]

use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use tracing::warn;

use cartog_core::{FileInfo, Symbol, PROGRESS_STRIDE};
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

/// Resolve a configured job count into a concrete pool size. `0` = auto
/// (`available_parallelism`), then clamped `1..=64` so a bad config never yields
/// a zero-thread pool or an unbounded thread explosion.
fn clamp_jobs(n: usize) -> usize {
    let resolved = if n == 0 {
        std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(1)
    } else {
        n
    };
    resolved.clamp(1, 64)
}

/// Pools keyed by resolved thread count, reused across `index_directory` calls.
/// Reuse keeps each pool's worker threads (and their warm `THREAD_EXTRACTORS`
/// cache) alive between re-indexes; a fresh pool per call would re-pay parser
/// construction every incremental pass.
static PARSE_POOLS: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, Arc<rayon::ThreadPool>>>> =
    std::sync::OnceLock::new();

/// A dedicated parse pool sized to `jobs`. Unlike the rayon global pool this
/// applies on every call (so the cap is honored under serve/watch and is
/// unaffected by another subsystem initializing the global pool first). Falls
/// back to the global pool only if the sized pool cannot be built.
fn parse_pool(jobs: usize) -> Option<Arc<rayon::ThreadPool>> {
    let size = clamp_jobs(jobs);
    let mut pools = PARSE_POOLS
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .ok()?;
    if let Some(p) = pools.get(&size) {
        return Some(Arc::clone(p));
    }
    match rayon::ThreadPoolBuilder::new().num_threads(size).build() {
        Ok(pool) => {
            let pool = Arc::new(pool);
            pools.insert(size, Arc::clone(&pool));
            Some(pool)
        }
        Err(e) => {
            warn!("failed to build parse pool ({size} threads): {e:#}; using default pool");
            None
        }
    }
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
    /// True when the CLI skipped its local LSP pass in favor of a live
    /// `cartog serve` peer (set by `cmd_index` only, never on the MCP path).
    #[serde(skip_serializing_if = "is_false")]
    pub lsp_deferred_to_peer: bool,
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
    /// Phase 2 in progress: `done` of `total` candidates parsed/extracted.
    /// `done == 0` marks the phase start; subsequent events climb toward `total`.
    Parsing { done: u32, total: u32 },
    /// Phase 3 in progress: `done` of `total` parsed files written in the
    /// indexing transaction. `done == 0` marks the phase start.
    Storing { done: u32, total: u32 },
    /// Phase 4 in progress: LSP-based edge resolution, `done` of `total`
    /// unresolved edges queried. Slowest phase on large repos and the one most
    /// likely to look "stuck". `done == 0` marks the phase start.
    ResolvingLsp { done: u32, total: u32 },
}

impl ProgressUpdate {
    /// Lower-case, transport-neutral phase label. The single source of truth
    /// for phase wording — CLI spinners and the MCP progress forwarder both
    /// render this (applying their own casing/format), so the strings can't
    /// drift between crates.
    pub fn label(&self) -> String {
        match self {
            ProgressUpdate::Walking => "scanning files".to_string(),
            // At phase start (done == 0) keep the terse "parsing N files"; once
            // files complete, show the climbing "parsing M/N files" counter.
            ProgressUpdate::Parsing { done: 0, total } => format!("parsing {total} files"),
            ProgressUpdate::Parsing { done, total } => format!("parsing {done}/{total} files"),
            ProgressUpdate::Storing { done: 0, total } => format!("storing {total} files"),
            ProgressUpdate::Storing { done, total } => format!("storing {done}/{total} files"),
            ProgressUpdate::ResolvingLsp { done: 0, total } => {
                format!("resolving {total} edges with LSP")
            }
            ProgressUpdate::ResolvingLsp { done, total } => {
                format!("resolving {done}/{total} edges with LSP")
            }
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
/// one file's write. When the probe trips, `index_directory` returns an `Err`
/// carrying [`cartog_core::CANCELLED_MSG`] as its root cause — callers detect it
/// with [`cartog_core::is_cancelled`]. Behavior with `None` is unchanged.
pub type CancelProbe<'a> = &'a (dyn Fn() -> bool + Send + Sync);

pub use cartog_core::{is_cancelled, CANCELLED_MSG};

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
///
/// `lsp_overrides` maps a language to its `[lsp.<lang>] command` argv; it only
/// takes effect when `lsp` is `true` and the `lsp` feature is compiled in. Pass
/// an empty map for the default (PATH-resolved) servers.
///
/// `filter` controls which files are walked: `.gitignore`/`.cartogignore`
/// (unless [`WalkFilter::respect_gitignore`] is false) plus `[index] exclude`
/// globs. The hardcoded floor (`node_modules`, `target`, …) always applies on
/// top. Pass [`WalkFilter::unrestricted`] for the default (no excludes,
/// gitignore honored).
#[allow(clippy::too_many_arguments)] // named, order-stable knobs; a struct would churn 51 call sites
pub fn index_directory(
    db: &Database,
    root: &Path,
    force: bool,
    lsp: bool,
    progress: Option<ProgressCallback<'_>>,
    cancel: Option<CancelProbe<'_>>,
    redact: RedactionConfig,
    lsp_overrides: &std::collections::HashMap<String, Vec<String>>,
    filter: &WalkFilter,
) -> Result<IndexResult> {
    let emit = |u: ProgressUpdate| {
        if let Some(cb) = progress {
            cb(u);
        }
    };
    let check_cancel = || -> Result<()> {
        if cancel.is_some_and(|c| c()) {
            anyhow::bail!(cartog_core::CANCELLED_MSG);
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
    // `ignore` honors .gitignore (incl. nested) + .cartogignore. require_git
    // makes it apply even without a .git dir. parents/git_global are OFF so only
    // ignore files INSIDE the indexed tree count — never an ancestor or
    // $HOME/.gitignore (which would silently drop files when indexing a subdir).
    // The hardcoded floor + `[index] exclude` run as a `filter_entry` on top.
    let mut walker = WalkBuilder::new(&root);
    walker
        .follow_links(true)
        .max_depth(Some(50))
        .hidden(false)
        .parents(false)
        .require_git(false)
        .git_global(false)
        .git_ignore(filter.respect_gitignore)
        .git_exclude(filter.respect_gitignore)
        .add_custom_ignore_filename(".cartogignore");
    let filter_root = root.clone();
    let filter_exclude = filter.exclude.clone();
    walker.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy();
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        if is_ignored(&name, is_dir, entry.depth()) {
            return false;
        }
        match entry.path().strip_prefix(&filter_root) {
            Ok(rel) => !is_excluded_path(rel, is_dir, &filter_exclude),
            Err(_) => true,
        }
    });
    for entry in walker.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "directory walk error");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
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

        // `[index] exclude` is enforced in the walk's `filter_entry` above
        // (`is_excluded_path`), which drops matching files and prunes matching
        // dirs before they are yielded — so no second check is needed here.

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
    let parsed: Vec<ParseOutput> = match parse_pool(filter.jobs) {
        Some(pool) => pool.install(run_parse),
        None => run_parse(),
    };

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
        done: 0,
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
        if result.files_indexed % PROGRESS_STRIDE == 0 || result.files_indexed == storing_total {
            emit(ProgressUpdate::Storing {
                done: result.files_indexed,
                total: storing_total,
            });
        }
    }

    // Remove files that no longer exist. Treat deletions as "dirty" so the
    // scoped incremental-repair branch below still runs when the *only* change
    // is a file deletion — otherwise unchanged files keep dangling target_ids
    // and stale in-degrees until the next edit.
    let all_indexed = db.all_files()?;
    // A walk that found nothing while the DB holds an index almost always
    // means the wrong root (e.g. `cartog rag index --db <db>` run from another
    // directory). Sweeping would silently delete the whole index — refuse.
    if current_files.is_empty() && !all_indexed.is_empty() && !force {
        let exclude_hint = if filter.exclude.is_empty() {
            ""
        } else {
            " A `[index] exclude` glob is set — check it isn't matching every file."
        };
        anyhow::bail!(
            "refusing to empty the index: no supported source files found under {} \
             but the database holds {} indexed files. This usually means the wrong \
             root for this database (e.g. `cartog rag index --db <db>` run from \
             another directory).{} Re-run from the project root, or pass --force to \
             really empty the index.",
            root.display(),
            all_indexed.len(),
            exclude_hint
        );
    }
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
                &root,
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
        let _ = (lsp, lsp_overrides); // suppress unused warnings when feature is off
    }

    // No LSP pass ran, so the heuristic was the only resolver: seal remaining
    // state=0 edges at state=4 to keep the next re-index's resolution scan from
    // re-walking the permanent-failure backlog (#109 watch amplification).
    if !lsp_ran && !dirty_files.is_empty() {
        db.mark_heuristic_exhausted_in_tx()?;
    }

    // Store the current git commit as last indexed
    if let Some(commit) = git_head_commit(&root) {
        db.set_metadata("last_commit", &commit)?;
    }

    // Record the redaction policy this index was built under so the next run
    // can detect a toggle and force a re-index.
    db.set_metadata("redact_secrets", &redact.enabled.to_string())?;

    result.dirty_files = dirty_files.len() as u32;
    let did_work = !dirty_files.is_empty();
    tx.commit()?;

    // Refresh planner stats after the write commits (not inside the tx). Skipped
    // on no-op runs — nothing changed, so the stats can't have drifted. Guards
    // the planner against misplans like the tier-2 quadratic in #110.
    if did_work {
        db.optimize()?;
    }

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

/// The hardcoded floor: whether a walked entry should be skipped regardless of
/// `.gitignore`. Walker-agnostic (takes primitives, not a `DirEntry`).
///
/// Only directories can be ignored — files are always accepted. Common non-code
/// directories (hidden dirs, `node_modules`, `vendor`, …) are excluded via
/// [`is_ignored_dirname`]. `var` and `builds` are only excluded at depth 1
/// (project root); a nested `src/var` is valid application code.
fn is_ignored(name: &str, is_dir: bool, depth: usize) -> bool {
    if is_dir {
        // "var" and "builds" are only ignored at the project root (depth 1).
        if matches!(name, "var" | "builds") && depth != 1 {
            return false;
        }
        return is_ignored_dirname(name);
    }
    false
}

/// Whether a repo-root-relative path matches a user `[index] exclude` glob
/// (dirs use the dir-probe form so `dir/**` prunes the directory).
fn is_excluded_path(rel: &Path, is_dir: bool, exclude: &ExcludeGlobs) -> bool {
    !exclude.is_empty() && exclude.is_excluded_with_dir(rel, is_dir)
}

/// Check if a directory name should be ignored during indexing.
///
/// The hardcoded floor, shared between the indexer's walk and the file watcher.
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
mod exclude;
mod git;
mod merkle;
mod redact;
mod walk;

pub(crate) use content::extract_symbol_content_redacted;
pub use exclude::ExcludeGlobs;
pub use git::git_recently_changed_files;
pub(crate) use git::{git_changed_files, git_head_commit};
pub(crate) use merkle::{compute_merkle_hashes, merkle_diff};
pub use redact::RedactionConfig;
pub use walk::WalkFilter;

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
    pub const FIXTURE_LANGS: [&str; 13] = [
        "py", "ts", "go", "rs", "rb", "java", "php", "dart", "swift", "kt", "vue", "svelte",
        "astro",
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
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
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
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
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
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
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
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
        )
        .expect("incremental re-index")
    }
}

#[cfg(test)]
mod tests;
