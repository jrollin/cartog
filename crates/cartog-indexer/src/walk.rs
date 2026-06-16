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
}

impl Default for WalkFilter {
    fn default() -> Self {
        Self {
            exclude: ExcludeGlobs::empty(),
            respect_gitignore: true,
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
