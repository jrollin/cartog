//! The registry's data types: what a writer supplies, and what a reader gets.

use std::path::{Path, PathBuf};

/// Facts a trigger supplies about a project it just wrote to.
///
/// Every field beyond `db_path`/`root` is optional, and `None` means "I don't
/// know", not "zero". The upsert leaves a `None` column at its stored value
/// (`COALESCE`), which is what lets a `serve` startup refresh `last_seen`
/// without erasing counts an `index` recorded.
#[derive(Debug, Clone)]
pub struct ProjectFacts {
    /// Path to the project's graph database. The registry's identity is
    /// derived from this, and it is the payload a consumer passes to
    /// `cartog <cmd> --db <path>`.
    pub db_path: PathBuf,
    /// The indexed project root.
    pub root: PathBuf,
    /// Graph-schema version of `db_path`, read by the caller (which already
    /// depends on `cartog-db`) and passed in as a primitive.
    pub schema_version: Option<u32>,
    pub file_count: Option<u32>,
    pub symbol_count: Option<u32>,
    pub edge_count: Option<u32>,
    pub resolved_count: Option<u32>,
    pub embedding_count: Option<u32>,
    /// Languages by symbol count, most-populous first.
    pub languages: Option<Vec<(String, u32)>>,
    pub embed_provider: Option<String>,
    pub embed_model: Option<String>,
    pub embed_dim: Option<u32>,
    /// Set only by a trigger that just completed an indexing pass. Its
    /// presence also defeats the fingerprint skip: a caller that indexed has
    /// new information by construction.
    pub last_indexed: Option<i64>,
}

impl ProjectFacts {
    /// Facts carrying only the project's identity — no counts.
    ///
    /// Used by triggers that opened a database without measuring it
    /// (`cartog serve` startup), so registration costs no extra query.
    ///
    /// Both paths are absolutized here rather than at each call site. Every
    /// index command defaults its root to `.`, and a registry row holding `.`
    /// or a relative `db_path` is worthless to the consumer it exists for —
    /// they read it from a different working directory than the writer had.
    #[must_use]
    pub fn identity_only(db_path: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        Self {
            db_path: absolutize(db_path.into()),
            root: absolutize(root.into()),
            schema_version: None,
            file_count: None,
            symbol_count: None,
            edge_count: None,
            resolved_count: None,
            embedding_count: None,
            languages: None,
            embed_provider: None,
            embed_model: None,
            embed_dim: None,
            last_indexed: None,
        }
    }

    /// The project's display name: the root directory's basename.
    ///
    /// Phase 1 has no `[project] name` config, so the basename is the only
    /// source. Falls back to the `db_path` basename, then to `"unknown"`, so
    /// the `NOT NULL` column always has a value.
    #[must_use]
    pub fn name(&self) -> String {
        basename(&self.root)
            // The root can be a relative path with no usable basename — every
            // index command defaults to `.`, and `cartog index .` is the common
            // case. Fall back to the root *inferred from the database path*,
            // which is absolute, before falling back to the database's own
            // filename (which would name every project "db.sqlite").
            .or_else(|| basename(&infer_root_from_db_path(&self.db_path)))
            .or_else(|| basename(&self.db_path))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Infer a project root from its database path.
///
/// cartog's layout is `<root>/.cartog/db.sqlite`, so the root is the
/// grandparent of the database file. Falls back to the parent (a `--db` pointed
/// straight at a file in the project), then to the path itself.
///
/// Inferred, not authoritative: a caller that *knows* the root — every index
/// command does, it is what the user pointed at — must pass that instead. This
/// exists for `cartog serve`, which is handed a database path and no root.
#[must_use]
pub fn infer_root_from_db_path(db_path: &Path) -> PathBuf {
    let parent = db_path.parent();
    match parent {
        // The conventional layout: strip the `.cartog` directory.
        Some(p) if p.file_name().is_some_and(|n| n == ".cartog") => {
            p.parent().unwrap_or(p).to_path_buf()
        }
        Some(p) => p.to_path_buf(),
        None => db_path.to_path_buf(),
    }
}

/// Make `p` absolute, resolving symlinks where the path exists.
///
/// A registry row is read by a *different* process from a *different* working
/// directory, so a relative path in it cannot be resolved back. Canonicalize
/// when possible (it also collapses `.`/`..` and macOS `/tmp` →
/// `/private/tmp`), else join onto the current directory, else return as-is —
/// never fail, since a registry write must not fail its caller.
fn absolutize(p: PathBuf) -> PathBuf {
    if let Ok(canon) = p.canonicalize() {
        return canon;
    }
    if p.is_absolute() {
        return p;
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let joined = cwd.join(&p);
            joined.canonicalize().unwrap_or(joined)
        }
        Err(_) => p,
    }
}

fn basename(p: &Path) -> Option<String> {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// One project as the registry knows it, plus the markers a reader computes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    /// The stored primary key: the serve slot at the time of writing.
    pub id: String,
    pub db_path: PathBuf,
    pub root: PathBuf,
    pub name: String,
    pub languages: Vec<(String, u32)>,
    pub schema_version: Option<u32>,
    pub file_count: Option<u32>,
    pub symbol_count: Option<u32>,
    pub edge_count: Option<u32>,
    pub resolved_count: Option<u32>,
    pub embedding_count: Option<u32>,
    pub embed_provider: Option<String>,
    pub embed_model: Option<String>,
    pub embed_dim: Option<u32>,
    pub last_indexed: Option<i64>,
    pub last_seen: i64,
    pub markers: Markers,
}

/// Diagnostic flags computed at read time, never stored — so a stale row can
/// never carry a stale marker.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Markers {
    /// A live `cartog serve`/`watch` peer holds this project's lock.
    ///
    /// Best-effort and advisory: never treat it as holding a lock yourself.
    pub live: bool,
    /// The stored `schema_version` differs from this binary's. The row still
    /// lists — its cached counts are the last thing known to be true.
    pub stale_schema: bool,
    /// `db_path` no longer exists: a `prune` candidate.
    pub missing: bool,
    /// This project's embedding fingerprint differs from the majority
    /// fingerprint among projects that have one. A project with no embeddings
    /// is never flagged.
    pub embed_mismatch: bool,
}

/// The result of reading the registry.
///
/// `available == false` with an empty list is an honest "there is no
/// registry", which a consumer must be able to tell apart from a registry
/// that exists and holds no projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub projects: Vec<ProjectRow>,
    pub available: bool,
}

impl Listing {
    /// The empty, unavailable listing: no registry, or the kill switch is set.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            projects: Vec::new(),
            available: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_the_root_basename() {
        let f = ProjectFacts::identity_only("/w/svc/.cartog/db.sqlite", "/w/svc-billing");
        assert_eq!(f.name(), "svc-billing");
    }

    #[test]
    fn a_rootless_path_falls_back_to_the_root_inferred_from_the_db_path() {
        // Root "/" has no basename, so the name comes from the db path's own
        // parent — never from the db *filename*, which would name every
        // project "db.sqlite".
        let f = ProjectFacts::identity_only("/w/proj.sqlite", "/");
        assert_eq!(f.name(), "w");
    }

    #[test]
    fn a_root_with_no_usable_basename_still_yields_a_real_project_name() {
        // Regression: `cartog index .` passes root="." (absolutized to the
        // *caller's* cwd, which is not the project). Whenever the root's
        // basename is unusable the name must come from the root inferred from
        // the db path — never from the db filename, which would name every
        // project "db.sqlite".
        let f = ProjectFacts {
            // Bypass identity_only's absolutize so the rootless case is exact.
            root: PathBuf::from("/"),
            ..ProjectFacts::identity_only("/w/svc-billing/.cartog/db.sqlite", "/")
        };
        assert_eq!(f.name(), "svc-billing");
    }

    #[test]
    fn a_relative_path_is_absolutized_so_another_process_can_resolve_it() {
        // A registry row is read from a different working directory than the
        // writer had, so a stored relative path is unresolvable.
        let f = ProjectFacts::identity_only("db.sqlite", ".");
        assert!(
            f.db_path.is_absolute(),
            "a stored db_path must be absolute: {}",
            f.db_path.display()
        );
        assert!(f.root.is_absolute(), "a stored root must be absolute");
    }

    #[test]
    fn the_name_never_yields_an_empty_string() {
        let f = ProjectFacts::identity_only("/", "/");
        assert_eq!(f.name(), "unknown");
    }

    #[test]
    fn identity_only_facts_carry_no_counts() {
        let f = ProjectFacts::identity_only("/d/db.sqlite", "/r");
        // A serve startup must not be able to accidentally write a zero count
        // over an index's real one.
        assert_eq!(f.symbol_count, None);
        assert_eq!(f.file_count, None);
        assert_eq!(f.last_indexed, None);
        assert!(f.languages.is_none());
    }

    #[test]
    fn the_root_is_inferred_past_the_dot_cartog_directory() {
        assert_eq!(
            infer_root_from_db_path(Path::new("/w/svc/.cartog/db.sqlite")),
            Path::new("/w/svc")
        );
    }

    #[test]
    fn a_db_beside_the_project_infers_its_parent_as_the_root() {
        // `--db /w/svc/custom.db` has no `.cartog` to strip.
        assert_eq!(
            infer_root_from_db_path(Path::new("/w/svc/custom.db")),
            Path::new("/w/svc")
        );
    }

    #[test]
    fn a_bare_db_filename_infers_a_root_without_panicking() {
        // No parent at all: must degrade, never panic.
        let inferred = infer_root_from_db_path(Path::new("db.sqlite"));
        assert!(inferred.as_os_str().is_empty() || inferred == Path::new("db.sqlite"));
    }

    #[test]
    fn a_dot_cartog_at_the_filesystem_root_does_not_panic() {
        assert_eq!(
            infer_root_from_db_path(Path::new("/.cartog/db.sqlite")),
            Path::new("/")
        );
    }

    #[test]
    fn an_unavailable_listing_is_empty_and_flagged() {
        let l = Listing::unavailable();
        assert!(l.projects.is_empty());
        assert!(!l.available);
    }
}
