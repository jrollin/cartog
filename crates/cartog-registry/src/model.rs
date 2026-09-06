//! The registry's data types: what a writer supplies, and what a reader gets.

use std::path::{Path, PathBuf};

/// Where a project's description came from.
///
/// Stored so `cartog projects list` can show that a description was *inferred*
/// from a README rather than declared by the project's author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptionSource {
    /// `[project] description` in `.cartog.toml`.
    Config,
    /// The first prose paragraph of the project's README.
    Readme,
}

impl DescriptionSource {
    /// The stored/serialized form: `"config"` or `"readme"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Readme => "readme",
        }
    }

    /// Parse the stored form, or `None` for anything else.
    ///
    /// A hand-edited or future value reads as "unknown source", never as an
    /// error: the description itself is still usable.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "config" => Some(Self::Config),
            "readme" => Some(Self::Readme),
            _ => None,
        }
    }
}

/// A project description plus the source that won.
///
/// The text is **repository-authored, untrusted input**: it exists to be read
/// by an agent, which makes it the most injection-prone value the registry
/// stores. Every consumer treats it as data — escaped on any rendering
/// surface, parameterized in SQL, never interpreted as instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    /// Plain text, at most `describe::DESCRIPTION_MAX_CHARS` characters.
    pub text: String,
    /// Which of the two sources produced `text`.
    pub source: DescriptionSource,
}

/// What a config-aware writer resolved for this project's declared identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declared {
    /// `[project] name`, or `None` to fall back to the root basename.
    pub name: Option<String>,
    /// The resolved description, or `None` when neither source produced one.
    pub description: Option<Description>,
}

/// Whether a writer looked at the project's declared identity at all.
///
/// The distinction is load-bearing: `cartog serve` startup and the watcher have
/// no config in scope, so they must leave a declared name and description
/// alone. Only a config-aware writer (`index`, `rag index`, `pull`) can
/// legitimately clear them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeclaredUpdate {
    /// Writer had no config in scope: stored values are kept as-is.
    #[default]
    Keep,
    /// Writer resolved name + description; both stored columns are
    /// overwritten, `NULL` included — so removing `[project] description` and
    /// the README paragraph clears the row.
    Set(Declared),
}

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
    /// Whether this writer resolved the project's declared name/description.
    /// [`DeclaredUpdate::Keep`] for any writer without config in scope.
    pub declared: DeclaredUpdate,
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
        let db_path = absolutize(db_path.into());
        let root = reconcile_root(absolutize(root.into()), &db_path);
        Self {
            db_path,
            root,
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
            declared: DeclaredUpdate::Keep,
        }
    }

    /// The project's root name: the root directory's basename.
    ///
    /// This feeds the `NOT NULL` `name` column and always means "basename" —
    /// a declared `[project] name` is stored separately (see
    /// [`ProjectRow::display_name`]). Falls back to the `db_path` basename,
    /// then to `"unknown"`, so the column always has a value.
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

/// Format a registry timestamp (Unix seconds) as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Lives here because the registry is what stores these timestamps, and both
/// consumers — the `cartog projects` CLI and the `cartog_list_projects` MCP
/// tool — need the same rendering. `cartog-mcp` cannot reach the binary's
/// `time_fmt` (the binary depends on it, not the reverse), so without a shared
/// home the two grew independent date implementations.
///
/// A pre-epoch value (a corrupted stored timestamp) clamps to the epoch rather
/// than panicking: a bad row must not crash a listing.
#[must_use]
pub fn format_timestamp(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = {
        let rem = secs % 86_400;
        (rem / 3600, (rem % 3600) / 60, rem % 60)
    };
    let (y, mo, d) = civil_from_days(secs / 86_400);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days since the Unix epoch → `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`, the standard branch-free algorithm —
/// correct across leap years and century non-leap boundaries.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Reduce an indexed path to the *project* root.
///
/// Every index command takes a path, and it need not be the project root:
/// `cartog index src` inside `myproj` indexes a subdirectory while writing to
/// `myproj/.cartog/db.sqlite`. Storing `src` as the root would name the project
/// `src` — and since phase-1 routing keys on the name, an agent would be told
/// the project is called `src`.
///
/// The database's location is the authority on where the project is, so when
/// the given root is not an ancestor of `db_path`, prefer the root inferred
/// from `db_path`. An `--db` pointing outside the indexed tree entirely (an
/// unusual but legal setup) keeps the caller's root, since then neither path
/// can vouch for the other.
fn reconcile_root(root: PathBuf, db_path: &Path) -> PathBuf {
    let inferred = infer_root_from_db_path(db_path);
    // The common case: the caller's root IS the project root, or an ancestor
    // of the DB — keep it, it is the more authoritative of the two.
    if db_path.starts_with(&root) {
        return root;
    }
    // The caller indexed a subdirectory of the project the DB belongs to.
    if root.starts_with(&inferred) {
        return inferred;
    }
    root
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
    /// The project root's basename. Always present; never the declared name.
    pub name: String,
    /// `[project] name`, when the project declared one.
    pub declared_name: Option<String>,
    /// The stored description and its source, when one is known.
    ///
    /// Repository-authored, untrusted text — see [`Description`].
    pub description: Option<Description>,
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

impl ProjectRow {
    /// The name to show a user: `declared_name` when present, else `name`.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.declared_name.as_deref().unwrap_or(&self.name)
    }
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
    fn timestamps_format_correctly_across_leap_and_century_boundaries() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
        // A leap day, the case naive date math gets wrong.
        assert_eq!(format_timestamp(1_709_164_800), "2024-02-29T00:00:00Z");
        // 2100 is NOT a leap year (century rule).
        assert_eq!(format_timestamp(4_107_542_400), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn a_pre_epoch_timestamp_clamps_rather_than_panicking() {
        // A corrupted stored value must not crash a listing.
        assert_eq!(format_timestamp(-1), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn indexing_a_subdirectory_still_names_the_project_after_its_root() {
        // Regression: `cartog index src` inside `myproj` stored root=".../src"
        // and therefore name="src". Phase-1 routing keys on the name, so an
        // agent was told the project is called `src`.
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("myproj");
        std::fs::create_dir_all(proj.join(".cartog")).unwrap();
        std::fs::create_dir_all(proj.join("src")).unwrap();
        let db = proj.join(".cartog").join("db.sqlite");
        std::fs::write(&db, b"").unwrap();

        let f = ProjectFacts::identity_only(&db, proj.join("src"));

        assert_eq!(f.name(), "myproj", "the project is myproj, not src");
        assert_eq!(f.root, proj.canonicalize().unwrap());
    }

    #[test]
    fn indexing_the_project_root_keeps_the_callers_root() {
        // The common case must be untouched: the caller's root is the more
        // authoritative of the two when it is an ancestor of the DB.
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("myproj");
        std::fs::create_dir_all(proj.join(".cartog")).unwrap();
        let db = proj.join(".cartog").join("db.sqlite");
        std::fs::write(&db, b"").unwrap();

        let f = ProjectFacts::identity_only(&db, &proj);

        assert_eq!(f.root, proj.canonicalize().unwrap());
        assert_eq!(f.name(), "myproj");
    }

    #[test]
    fn a_db_outside_the_indexed_tree_keeps_the_callers_root() {
        // `--db /elsewhere/x.sqlite` while indexing /work/proj: neither path
        // can vouch for the other, so trust the caller.
        let f = ProjectFacts::identity_only("/elsewhere/x.sqlite", "/work/proj");
        assert_eq!(f.root, Path::new("/work/proj"));
        assert_eq!(f.name(), "proj");
    }

    /// A row with only the fields these tests care about.
    fn row(name: &str, declared_name: Option<&str>) -> ProjectRow {
        ProjectRow {
            id: "serve-0".to_string(),
            db_path: PathBuf::from("/r/.cartog/db.sqlite"),
            root: PathBuf::from("/r"),
            name: name.to_string(),
            declared_name: declared_name.map(str::to_string),
            description: None,
            languages: Vec::new(),
            schema_version: None,
            file_count: None,
            symbol_count: None,
            edge_count: None,
            resolved_count: None,
            embedding_count: None,
            embed_provider: None,
            embed_model: None,
            embed_dim: None,
            last_indexed: None,
            last_seen: 1,
            markers: Markers::default(),
        }
    }

    #[test]
    fn the_display_name_prefers_the_declared_name() {
        assert_eq!(
            row("api", Some("svc-billing")).display_name(),
            "svc-billing"
        );
    }

    #[test]
    fn the_display_name_falls_back_to_the_root_basename() {
        assert_eq!(row("api", None).display_name(), "api");
    }

    #[test]
    fn identity_only_facts_keep_the_declared_identity() {
        // A writer with no config in scope must never clear a declared name or
        // description: `serve` startup and the watcher both go through here.
        let f = ProjectFacts::identity_only("/d/db.sqlite", "/r");
        assert_eq!(f.declared, DeclaredUpdate::Keep);
    }

    #[test]
    fn description_sources_round_trip_through_their_stored_form() {
        for source in [DescriptionSource::Config, DescriptionSource::Readme] {
            assert_eq!(DescriptionSource::parse(source.as_str()), Some(source));
        }
    }

    #[test]
    fn an_unknown_description_source_parses_as_none() {
        assert_eq!(DescriptionSource::parse("wikipedia"), None);
        assert_eq!(DescriptionSource::parse(""), None);
    }

    #[test]
    fn an_unavailable_listing_is_empty_and_flagged() {
        let l = Listing::unavailable();
        assert!(l.projects.is_empty());
        assert!(!l.available);
    }
}
