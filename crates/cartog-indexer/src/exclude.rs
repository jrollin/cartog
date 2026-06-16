//! `[index] exclude` path globs, compiled once into a [`globset::GlobSet`].
//! Consulted by the indexer walk (prunes dirs, drops files) and the watcher's
//! relevance filter, which must agree on scope. Absent config = no-op matcher.
//!
//! `dir/**` matches children but not the bare directory entry, so pruning a
//! directory needs the dir-probe in [`ExcludeGlobs::is_excluded_with_dir`].

use std::path::Path;

use anyhow::{bail, Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Synthetic child appended to a dir so a `dir/**` glob prunes it. NUL byte
/// cannot appear in a real path component, so it never collides.
const DIR_PROBE: &str = "\u{0}cartog-dir-probe";

/// Compiled repo-root-relative exclude globs from `[index] exclude`.
///
/// Cheap to clone is *not* guaranteed (the inner `GlobSet` holds compiled
/// automata), so callers pass `&ExcludeGlobs` rather than by value — mirroring
/// `lsp_overrides: &HashMap<..>`. Use [`ExcludeGlobs::empty`] (or `default`) for
/// the no-op case threaded through tests and unconfigured runs.
#[derive(Debug, Clone, Default)]
pub struct ExcludeGlobs {
    set: GlobSet,
}

impl ExcludeGlobs {
    /// The empty matcher — matches no path. Equivalent to [`Default::default`].
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Compile repo-root-relative globs (e.g. `mobile/ios/Pods/**`, `**/*.md`).
    ///
    /// `*`/`?` are segment-local (`literal_separator`): `src/*` matches direct
    /// children of `src`, not the whole subtree — `src/**` does that. This
    /// matches the gitignore mental model the feature evokes.
    ///
    /// # Errors
    /// Returns an error if any glob is empty (it would match the repo root and
    /// silently empty the index) or malformed, so a bad `[index] exclude` entry
    /// fails at config load rather than at first index.
    pub fn from_globs(globs: &[String]) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for g in globs {
            if g.is_empty() {
                bail!("empty exclude glob: an empty pattern matches everything");
            }
            let glob = GlobBuilder::new(g)
                .literal_separator(true)
                .build()
                .with_context(|| format!("invalid exclude glob {g:?}"))?;
            builder.add(glob);
        }
        let set = builder
            .build()
            .context("failed to build exclude glob set")?;
        Ok(Self { set })
    }

    /// True when no globs are configured (every match returns `false`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// True if a repo-root-relative *file* path matches any glob.
    ///
    /// Use this for files and for the watcher's relevance filter (which only ever
    /// sees file paths). For directory pruning during a walk, use
    /// [`is_excluded_with_dir`](Self::is_excluded_with_dir).
    #[must_use]
    pub fn is_excluded(&self, rel_path: &Path) -> bool {
        self.set.is_match(rel_path)
    }

    /// True if a repo-root-relative path should be excluded, accounting for the
    /// `dir/**` pruning subtlety when `is_dir` is set.
    ///
    /// For files this is identical to [`is_excluded`](Self::is_excluded). For
    /// directories it additionally probes a synthetic child so a `dir/**`-style
    /// glob prunes the directory (the walker then never descends into it).
    #[must_use]
    pub fn is_excluded_with_dir(&self, rel_path: &Path, is_dir: bool) -> bool {
        if self.set.is_match(rel_path) {
            return true;
        }
        is_dir && self.set.is_match(rel_path.join(DIR_PROBE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globs(patterns: &[&str]) -> ExcludeGlobs {
        ExcludeGlobs::from_globs(
            &patterns
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>(),
        )
        .expect("valid globs")
    }

    #[test]
    fn excludes_file_by_recursive_glob() {
        let ex = globs(&["**/*.md"]);
        assert!(ex.is_excluded(Path::new("docs/readme.md")));
        assert!(ex.is_excluded(Path::new("a/b/c.md")));
        assert!(!ex.is_excluded(Path::new("src/main.rs")));
    }

    #[test]
    fn prunes_directory_via_double_star() {
        // The crux: `dir/**` does NOT match the bare directory in globset, so the
        // dir-probe must make `is_excluded_with_dir(dir, is_dir=true)` true.
        let ex = globs(&["mobile/ios/Pods/**"]);
        assert!(
            ex.is_excluded_with_dir(Path::new("mobile/ios/Pods"), true),
            "directory must be pruned so the walker never descends"
        );
        // And a child file still matches directly.
        assert!(ex.is_excluded(Path::new("mobile/ios/Pods/Firebase/Core.swift")));
        // A sibling directory is untouched.
        assert!(!ex.is_excluded_with_dir(Path::new("mobile/ios/Runner"), true));
    }

    #[test]
    fn prunes_directory_via_single_star_segment() {
        let ex = globs(&["mobile/*/Runner/**"]);
        assert!(ex.is_excluded_with_dir(Path::new("mobile/app/Runner"), true));
        assert!(ex.is_excluded(Path::new("mobile/app/Runner/main.kt")));
        // `*` spans exactly one path segment (literal_separator), so the literal
        // `/Runner/` cannot line up here.
        assert!(!ex.is_excluded_with_dir(Path::new("mobile/Runner"), true));
    }

    #[test]
    fn single_star_is_segment_local() {
        // With literal_separator, `*` spans one segment, so a *file* path two
        // levels deep is not matched directly (only `**` matches deep paths).
        let ex = globs(&["src/*"]);
        assert!(ex.is_excluded(Path::new("src/main.rs")));
        assert!(!ex.is_excluded(Path::new("src/sub/deep.rs")));
        let deep = globs(&["src/**"]);
        assert!(deep.is_excluded(Path::new("src/sub/deep.rs")));
    }

    #[test]
    fn single_star_prunes_direct_subdirs_like_gitignore() {
        // `src/*` matches the direct subdir `src/sub` itself, so the walk prunes
        // it (and everything under it) — matching gitignore, where `src/*`
        // excludes all direct entries of `src`. Use `src/*.rs` to keep subdirs.
        let ex = globs(&["src/*"]);
        assert!(ex.is_excluded_with_dir(Path::new("src/sub"), true));
        // A narrower glob spares subdirs:
        let only_rs = globs(&["src/*.rs"]);
        assert!(only_rs.is_excluded(Path::new("src/main.rs")));
        assert!(!only_rs.is_excluded_with_dir(Path::new("src/sub"), true));
    }

    #[test]
    fn file_form_does_not_prune_dir_without_probe() {
        // A bare `is_excluded` on the directory path must NOT match a `dir/**`
        // glob — proving the probe in is_excluded_with_dir is what does the work.
        let ex = globs(&["mobile/ios/Pods/**"]);
        assert!(!ex.is_excluded(Path::new("mobile/ios/Pods")));
    }

    #[test]
    fn empty_matcher_never_matches() {
        let ex = ExcludeGlobs::empty();
        assert!(ex.is_empty());
        assert!(!ex.is_excluded(Path::new("anything/at/all.md")));
        assert!(!ex.is_excluded_with_dir(Path::new("anything"), true));
        assert!(!ex.is_excluded_with_dir(Path::new("anything"), false));
    }

    #[test]
    fn from_globs_empty_slice_is_empty() {
        let ex = ExcludeGlobs::from_globs(&[]).unwrap();
        assert!(ex.is_empty());
    }

    #[test]
    fn invalid_glob_is_rejected() {
        let err = ExcludeGlobs::from_globs(&["[unclosed".to_string()]);
        assert!(err.is_err(), "malformed glob must error, not match nothing");
    }

    #[test]
    fn empty_glob_is_rejected() {
        // An empty pattern matches the repo root and would silently empty the
        // index — reject it at compile rather than letting the walk prune all.
        let err = ExcludeGlobs::from_globs(&["".to_string()]);
        assert!(err.is_err(), "empty glob must be rejected");
        // And it must not slip through alongside a valid one.
        let mixed = ExcludeGlobs::from_globs(&["vendor/**".to_string(), String::new()]);
        assert!(mixed.is_err());
    }
}
