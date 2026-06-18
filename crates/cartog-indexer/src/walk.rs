//! Walk-time path filtering: which files the indexer considers as candidates.
//!
//! Three layers compose, all AND-pruning:
//! 1. the hardcoded floor (`is_ignored_dirname`: `node_modules`, `target`, …),
//! 2. `.gitignore`/`.cartogignore` (honored by the `ignore` crate when
//!    [`WalkFilter::respect_gitignore`] is set — the default),
//! 3. user [`ExcludeGlobs`] from `[index] exclude`.
//!
//! The floor stays authoritative: it applies even in a repo that `!`-unignores
//! those dirs and in a non-git tree. `respect_gitignore = false` disables only
//! layer 2 (git's view), never the floor or the explicit excludes.

use super::*;
use crate::exclude::ExcludeGlobs;

/// Path-filtering policy threaded into `index_directory`.
///
/// Bundles the `[index] exclude` globs with the `respect_gitignore` toggle so a
/// single `&WalkFilter` carries all walk knobs (rather than a growing list of
/// positional args). Use [`WalkFilter::unrestricted`] (or `default`) for the
/// no-op case threaded through tests and unconfigured runs — note that even the
/// default honors `.gitignore`, matching production behavior.
#[derive(Debug, Clone)]
pub struct WalkFilter {
    /// User `[index] exclude` globs.
    pub exclude: ExcludeGlobs,
    /// Honor git's ignore files — `.gitignore` and `.git/info/exclude` (incl.
    /// nested). Default `true`; set `false` to index git-ignored files (e.g.
    /// committed generated code). The floor, `exclude`, and cartog's own
    /// `.cartogignore`/`.ignore` files still apply regardless (this toggle only
    /// governs git's view).
    pub respect_gitignore: bool,
    /// Parse-phase worker threads. `0` = auto (`available_parallelism`), clamped
    /// `1..=64`. Sizes a dedicated rayon pool (cached per size, reused across
    /// re-indexes) that the parse phase runs in, so the cap applies on every
    /// index, including under a long-lived `serve`/`watch`.
    pub jobs: usize,
    /// Max concurrent LSP server processes during the edge-resolution pass.
    /// `0` = auto (`min(languages_in_pass, 4)`). Each server is RAM-heavy
    /// (rust-analyzer ~1-2GB). Only the indexer's owned-manager pass fans out;
    /// the warm MCP pass stays serial.
    pub lsp_max_servers: usize,
}

impl Default for WalkFilter {
    fn default() -> Self {
        Self {
            exclude: ExcludeGlobs::empty(),
            respect_gitignore: true,
            jobs: 0,
            lsp_max_servers: 0,
        }
    }
}

impl WalkFilter {
    /// No user excludes; `.gitignore` still honored (the production default).
    /// The ergonomic no-op for tests and call sites without config.
    #[must_use]
    pub fn unrestricted() -> Self {
        Self::default()
    }
}

/// Phase 1 output: files to parse, plus the full current-file set used by the
/// post-store removal sweep.
pub(crate) struct Candidates {
    /// `(absolute_path, rel_path, language)` for each file that needs parsing.
    pub items: Vec<(PathBuf, String, &'static str)>,
    /// Every supported source file seen this walk (parsed or hash-skipped).
    /// The removal sweep deletes DB rows for indexed files absent from this set.
    pub current_files: std::collections::HashSet<String>,
}

/// Phase 1: walk the tree and collect parse candidates.
///
/// Single-threaded and DB-free apart from the pre-fetched `stored_hashes` /
/// `changed_files` reads done by the caller. Honors the floor + `.gitignore` +
/// `[index] exclude` (via [`WalkFilter`]) and the sensitive-file deny-list, then
/// applies git-diff / stored-hash skip so unchanged files never reach Phase 2.
/// Mutates `result`'s walk-side counters (skipped/unsupported/redacted-skipped).
#[must_use]
pub(crate) fn walk_candidates(
    root: &Path,
    force: bool,
    filter: &WalkFilter,
    changed_files: Option<&std::collections::HashSet<String>>,
    stored_hashes: &std::collections::HashMap<String, String>,
    result: &mut IndexResult,
) -> Candidates {
    let mut current_files = std::collections::HashSet::new();
    let mut candidates: Vec<(PathBuf, String, &'static str)> = Vec::new();
    let mut unsupported_ext: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    // `ignore` honors .gitignore (incl. nested) + .cartogignore. require_git
    // makes it apply even without a .git dir. parents/git_global are OFF so only
    // ignore files INSIDE the indexed tree count — never an ancestor or
    // $HOME/.gitignore (which would silently drop files when indexing a subdir).
    // The hardcoded floor + `[index] exclude` run as a `filter_entry` on top.
    let mut walker = WalkBuilder::new(root);
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
    let filter_root = root.to_path_buf();
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
        let rel_path = match path.strip_prefix(root) {
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
            if let Some(changed) = changed_files {
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

    Candidates {
        items: candidates,
        current_files,
    }
}
