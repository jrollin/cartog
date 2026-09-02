//! Reading the registry: rows plus the markers computed from them.
//!
//! Nothing here opens a project database. Every marker comes from a cached
//! column, a `db_path.exists()` check, or the state directory's PID files, so
//! the cost of a listing is independent of how many projects are registered
//! and can never contend with another project's writer.

use std::path::Path;

use rusqlite::Connection;

use crate::corrupt::quarantine_if_corrupt;
use crate::model::{Description, DescriptionSource, Listing, Markers, ProjectRow};
use crate::open::open_read_only;
use crate::slot::slot_for_db;
use crate::write::decode_languages;

/// List every registered project, using the default registry and state dir.
///
/// Never fails: an absent, disabled, or unreadable registry yields
/// [`Listing::unavailable`]. A corrupt one is renamed aside first (see
/// `corrupt`), then reported the same way.
#[must_use]
pub fn list_projects(current_schema_version: u32) -> Listing {
    let Some(registry) = crate::path::registry_path() else {
        return Listing::unavailable();
    };
    list_projects_at(
        &registry,
        crate::state_dir::default_state_dir().as_deref(),
        current_schema_version,
    )
}

/// [`list_projects`] against explicit paths, for tests and for a server that
/// was given a state directory other than the default.
#[must_use]
pub fn list_projects_at(
    registry: &Path,
    state_dir: Option<&Path>,
    current_schema_version: u32,
) -> Listing {
    if !registry.exists() {
        return Listing::unavailable();
    }
    quarantine_if_corrupt(registry);

    let conn = match open_read_only(registry) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                registry = %registry.display(),
                error = %e,
                "could not read the project registry; reporting no projects"
            );
            return Listing::unavailable();
        }
    };

    let mut rows = match select_rows(&conn) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                registry = %registry.display(),
                error = %e,
                "could not query the project registry; reporting no projects"
            );
            return Listing::unavailable();
        }
    };

    let live_slots = live_slots(state_dir);
    let majority = majority_fingerprint(&rows);
    for row in &mut rows {
        row.markers = markers_for(row, &live_slots, majority.as_ref(), current_schema_version);
    }
    // Most-recently-indexed first; never-indexed rows last. That ordering
    // makes the listing answer "what have I been working on" without a flag.
    rows.sort_by(|a, b| {
        b.last_indexed
            .cmp(&a.last_indexed)
            .then_with(|| a.name.cmp(&b.name))
    });

    Listing {
        projects: rows,
        available: true,
    }
}

/// Read every row, degrading **per row** rather than per registry.
///
/// A row that cannot be deserialized is logged and skipped, never fatal.
/// Collecting into a single `Result` instead let one out-of-range count (a
/// hand-edited file, a negative value, a `u32` overflow) abort the whole
/// listing: every healthy project vanished, the caller could not tell that
/// from "no registry exists", and `prune` became a permanent no-op on exactly
/// the row that most needed pruning.
fn select_rows(conn: &Connection) -> rusqlite::Result<Vec<ProjectRow>> {
    // A not-yet-migrated v1 file has no declared columns; read them as NULL
    // rather than failing the whole listing until some writer migrates it.
    let declared_cols = if crate::schema::has_declared_columns(conn) {
        "declared_name, description, description_src"
    } else {
        "NULL, NULL, NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, db_path, root, name, languages, schema_version,
                file_count, symbol_count, edge_count, resolved_count, embedding_count,
                embed_provider, embed_model, embed_dim, last_indexed, last_seen,
                {declared_cols}
         FROM projects"
    ))?;
    let rows = stmt.query_map([], |row| {
        let languages: Option<String> = row.get(4)?;
        Ok(ProjectRow {
            id: row.get(0)?,
            db_path: row.get::<_, String>(1)?.into(),
            root: row.get::<_, String>(2)?.into(),
            name: row.get(3)?,
            declared_name: row.get(16)?,
            description: description(row.get(17)?, row.get(18)?),
            languages: languages
                .as_deref()
                .map(decode_languages)
                .unwrap_or_default(),
            schema_version: count(row, 5)?,
            file_count: count(row, 6)?,
            symbol_count: count(row, 7)?,
            edge_count: count(row, 8)?,
            resolved_count: count(row, 9)?,
            embedding_count: count(row, 10)?,
            embed_provider: row.get(11)?,
            embed_model: row.get(12)?,
            embed_dim: count(row, 13)?,
            last_indexed: row.get(14)?,
            last_seen: row.get(15)?,
            markers: Markers::default(),
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        match row {
            Ok(r) => out.push(r),
            Err(e) => tracing::warn!(
                error = %e,
                "skipping an unreadable project registry row; the other projects still list"
            ),
        }
    }
    Ok(out)
}

/// Pair a stored description with its source.
///
/// An unrecognized `description_src` falls back to `Readme`, the weaker claim:
/// mislabeling an inferred description as declared would tell a user their
/// repo asserts something it does not.
fn description(text: Option<String>, src: Option<String>) -> Option<Description> {
    let text = text.filter(|t| !t.is_empty())?;
    let source = src
        .as_deref()
        .and_then(DescriptionSource::parse)
        .unwrap_or(DescriptionSource::Readme);
    Some(Description { text, source })
}

/// Read a count column as `i64`, then narrow it, treating an out-of-range or
/// negative value as "not known" rather than as an error.
///
/// A count is a cache of something measured elsewhere, so a nonsensical stored
/// value means the cache is wrong about that one field — not that the row is
/// unusable.
fn count(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<u32>> {
    Ok(row
        .get::<_, Option<i64>>(idx)?
        .and_then(|v| u32::try_from(v).ok()))
}

/// Slots of every live PID lock in `state_dir`.
fn live_slots(state_dir: Option<&Path>) -> Vec<String> {
    state_dir
        .map(|dir| {
            cartog_process_lock::find_active_locks(dir)
                .into_iter()
                .map(|lock| lock.slot)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a live serve or watch peer holds this project.
///
/// Checks both the stored `id` and the slot recomputed from `db_path`. They
/// can differ: `slot_for_db` canonicalizes fully only when the path exists, so
/// a row written while the database was absent carries an id a later
/// canonicalizable path will not reproduce. Checking both directions costs one
/// string compare and catches the peer either way.
///
/// A mismatch means **"no live peer"** — never "missing project", never a
/// reason to rewrite or drop the row.
fn is_live(row: &ProjectRow, live_slots: &[String]) -> bool {
    let recomputed = slot_for_db("serve", &row.db_path);
    let watch_stored = row.id.strip_prefix("serve-").map(|h| format!("watch-{h}"));
    let watch_recomputed = recomputed
        .strip_prefix("serve-")
        .map(|h| format!("watch-{h}"));

    live_slots.iter().any(|slot| {
        slot == &row.id
            || slot == &recomputed
            || watch_stored.as_deref() == Some(slot)
            || watch_recomputed.as_deref() == Some(slot)
    })
}

/// The embedding fingerprint shared by the most projects that have one.
///
/// `None` — nothing is flagged — in three cases:
///
/// - Fewer than two *distinct* fingerprints exist. That covers both "nobody
///   has embeddings" and "everybody agrees", which are the same answer.
/// - The most common fingerprint is tied. Flagging both halves of a 1-1 split
///   would be noise, not a diagnostic.
///
/// Otherwise the modal fingerprint wins and every other one is a mismatch, so
/// a 2-vs-1 split does flag the single odd project — that is the intent: the
/// marker answers "which project is configured differently from my others".
fn majority_fingerprint(rows: &[ProjectRow]) -> Option<(String, String, u32)> {
    let mut counts: std::collections::HashMap<(String, String, u32), usize> =
        std::collections::HashMap::new();
    for row in rows {
        if let (Some(p), Some(m), Some(d)) = (
            row.embed_provider.as_ref(),
            row.embed_model.as_ref(),
            row.embed_dim,
        ) {
            *counts.entry((p.clone(), m.clone(), d)).or_default() += 1;
        }
    }
    if counts.len() < 2 {
        // Zero fingerprints, or one that everybody shares: nothing to mismatch.
        return None;
    }
    let max = counts.values().copied().max()?;
    let mut leaders = counts.iter().filter(|(_, &c)| c == max);
    let (fingerprint, _) = leaders.next()?;
    if leaders.next().is_some() {
        // Tied: no majority.
        return None;
    }
    Some(fingerprint.clone())
}

fn markers_for(
    row: &ProjectRow,
    live_slots: &[String],
    majority: Option<&(String, String, u32)>,
    current_schema_version: u32,
) -> Markers {
    let embed_mismatch = match (
        majority,
        row.embed_provider.as_ref(),
        row.embed_model.as_ref(),
        row.embed_dim,
    ) {
        (Some(maj), Some(p), Some(m), Some(d)) => maj != &(p.clone(), m.clone(), d),
        // A project with no embeddings is never flagged.
        _ => false,
    };
    Markers {
        live: is_live(row, live_slots),
        stale_schema: row
            .schema_version
            .is_some_and(|v| v != current_schema_version),
        missing: !row.db_path.exists(),
        embed_mismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectFacts;

    /// Registry seeded through the real write path, so reads are tested
    /// against rows a real trigger would have produced.
    struct Fixture {
        _dir: tempfile::TempDir,
        registry: std::path::PathBuf,
        state_dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let registry = dir.path().join("projects.sqlite");
            let state_dir = dir.path().join("state");
            std::fs::create_dir_all(&state_dir).unwrap();
            Self {
                _dir: dir,
                registry,
                state_dir,
            }
        }

        /// Insert a row directly, bypassing `record_project`'s env lookup.
        fn seed(&self, facts: &ProjectFacts) {
            let conn = crate::open::open_read_write(&self.registry).unwrap();
            crate::schema::ensure_schema(&conn).unwrap();
            let id = slot_for_db("serve", &facts.db_path);
            crate::write::upsert_for_test(&conn, &id, facts, None).unwrap();
        }

        fn list(&self) -> Listing {
            list_projects_at(&self.registry, Some(&self.state_dir), 8)
        }
    }

    /// A real project DB file, so `missing` is false and the fingerprint works.
    fn touch_db(dir: &Path, name: &str) -> std::path::PathBuf {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join(".cartog")).unwrap();
        let db = root.join(".cartog").join("db.sqlite");
        std::fs::write(&db, b"").unwrap();
        db
    }

    fn facts(db: &Path, root: &Path) -> ProjectFacts {
        ProjectFacts {
            schema_version: Some(8),
            file_count: Some(412),
            symbol_count: Some(8134),
            edge_count: Some(19022),
            resolved_count: Some(13505),
            embedding_count: Some(8134),
            languages: Some(vec![("rust".to_string(), 412)]),
            last_indexed: Some(1_700_000_000),
            ..ProjectFacts::identity_only(db, root)
        }
    }

    #[test]
    fn an_absent_registry_lists_as_unavailable() {
        let f = Fixture::new();
        let listing = f.list();
        assert!(
            !listing.available,
            "absent must be distinguishable from empty"
        );
        assert!(listing.projects.is_empty());
    }

    #[test]
    fn an_empty_registry_is_available_with_no_projects() {
        let f = Fixture::new();
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            crate::schema::ensure_schema(&conn).unwrap();
        }
        let listing = f.list();
        assert!(listing.available, "an existing empty registry is available");
        assert!(listing.projects.is_empty());
    }

    #[test]
    fn a_seeded_row_round_trips_every_field() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "svc-billing");
        let root = dir.join("svc-billing");
        f.seed(&facts(&db, &root));

        let listing = f.list();
        assert_eq!(listing.projects.len(), 1);
        let row = &listing.projects[0];
        assert_eq!(row.name, "svc-billing");
        // Compare canonicalized: the writer absolutizes, and on macOS
        // /var canonicalizes to /private/var. That normalization is the point —
        // it is what stops one project occupying two rows.
        assert_eq!(row.db_path, db.canonicalize().unwrap());
        assert_eq!(row.root, root.canonicalize().unwrap());
        assert_eq!(row.symbol_count, Some(8134));
        assert_eq!(row.file_count, Some(412));
        assert_eq!(row.languages, vec![("rust".to_string(), 412)]);
        assert_eq!(row.last_indexed, Some(1_700_000_000));
    }

    #[test]
    fn a_declared_name_and_description_round_trip_with_their_source() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "api");
        f.seed(&ProjectFacts {
            declared: crate::model::DeclaredUpdate::Set(crate::model::Declared {
                name: Some("svc-billing".to_string()),
                description: Some(Description {
                    text: "Invoice generation.".to_string(),
                    source: DescriptionSource::Readme,
                }),
            }),
            ..facts(&db, &dir.join("api"))
        });

        let row = &f.list().projects[0];
        assert_eq!(row.name, "api", "name stays the root basename");
        assert_eq!(row.declared_name.as_deref(), Some("svc-billing"));
        let d = row.description.as_ref().expect("a description");
        assert_eq!(d.text, "Invoice generation.");
        assert_eq!(d.source, DescriptionSource::Readme);
    }

    #[test]
    fn the_display_name_prefers_the_declared_name_on_a_read_row() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "api");
        f.seed(&ProjectFacts {
            declared: crate::model::DeclaredUpdate::Set(crate::model::Declared {
                name: Some("svc-billing".to_string()),
                description: None,
            }),
            ..facts(&db, &dir.join("api"))
        });

        assert_eq!(f.list().projects[0].display_name(), "svc-billing");
    }

    #[test]
    fn an_unrecognized_description_source_reads_back_as_inferred() {
        // The weaker claim: calling an unknown source "config" would tell a
        // user their repo asserts something it does not.
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "api");
        f.seed(&facts(&db, &dir.join("api")));
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute(
                "UPDATE projects SET description = 'x', description_src = 'wikipedia'",
                [],
            )
            .unwrap();
        }

        let d = f.list().projects[0].description.clone().unwrap();
        assert_eq!(d.source, DescriptionSource::Readme);
    }

    #[test]
    fn a_not_yet_migrated_v1_registry_still_lists_its_projects() {
        let fx = Fixture::new();
        let conn = Connection::open(&fx.registry).unwrap();
        conn.execute_batch(crate::schema::V1_SCHEMA).unwrap();
        drop(conn);

        let listing = fx.list();

        assert!(listing.available);
        assert_eq!(listing.projects.len(), 1);
        let row = &listing.projects[0];
        assert_eq!(row.name, "svc");
        assert_eq!(row.declared_name, None);
        assert_eq!(row.description, None);
        assert_eq!(row.symbol_count, Some(8134));
    }

    #[test]
    fn reading_a_v1_registry_does_not_migrate_it() {
        let fx = Fixture::new();
        let conn = Connection::open(&fx.registry).unwrap();
        conn.execute_batch(crate::schema::V1_SCHEMA).unwrap();

        fx.list();

        assert!(!crate::schema::has_declared_columns(&conn));
    }

    #[test]
    fn a_deleted_database_is_marked_missing() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "gone");
        f.seed(&facts(&db, &dir.join("gone")));
        std::fs::remove_file(&db).unwrap();

        assert!(f.list().projects[0].markers.missing);
    }

    #[test]
    fn a_differing_schema_version_is_marked_stale() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "old");
        let mut fx = facts(&db, &dir.join("old"));
        fx.schema_version = Some(6);
        f.seed(&fx);

        let row = &f.list().projects[0];
        assert!(row.markers.stale_schema);
        assert_eq!(
            row.symbol_count,
            Some(8134),
            "a stale row still lists its cached counts"
        );
    }

    #[test]
    fn a_live_serve_lock_marks_the_project_live() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "serving");
        f.seed(&facts(&db, &dir.join("serving")));

        let _held =
            cartog_process_lock::ProcessLock::acquire(&f.state_dir, &slot_for_db("serve", &db))
                .unwrap();

        assert!(f.list().projects[0].markers.live);
    }

    #[test]
    fn a_live_watch_lock_also_marks_the_project_live() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "watching");
        f.seed(&facts(&db, &dir.join("watching")));

        let _held =
            cartog_process_lock::ProcessLock::acquire(&f.state_dir, &slot_for_db("watch", &db))
                .unwrap();

        assert!(f.list().projects[0].markers.live);
    }

    #[test]
    fn no_lock_means_not_live() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "idle");
        f.seed(&facts(&db, &dir.join("idle")));

        assert!(!f.list().projects[0].markers.live);
    }

    #[test]
    fn an_id_that_no_longer_recomputes_reports_no_live_peer_not_a_missing_project() {
        // A row written while the DB was absent can carry an id that a later
        // canonicalizable path will not reproduce. That must degrade to "no
        // live peer" and never to "missing" — the DB is right there.
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "drifted");
        f.seed(&facts(&db, &dir.join("drifted")));
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute("UPDATE projects SET id = 'serve-deadbeefdeadbeef'", [])
                .unwrap();
        }

        let row = &f.list().projects[0];
        assert!(!row.markers.live, "a drifted id means no live peer");
        assert!(
            !row.markers.missing,
            "a drifted id must never be reported as a missing project"
        );
    }

    #[test]
    fn a_drifted_id_still_matches_a_peer_on_the_recomputed_slot() {
        // The both-directions check: the lock is held on the slot recomputed
        // from db_path, which differs from the stored id.
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "drifted-live");
        f.seed(&facts(&db, &dir.join("drifted-live")));
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute("UPDATE projects SET id = 'serve-deadbeefdeadbeef'", [])
                .unwrap();
        }

        let _held =
            cartog_process_lock::ProcessLock::acquire(&f.state_dir, &slot_for_db("serve", &db))
                .unwrap();

        assert!(f.list().projects[0].markers.live);
    }

    #[test]
    fn a_project_with_no_embeddings_is_never_flagged_mismatched() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        for (name, provider) in [("a", Some("local")), ("b", Some("local")), ("c", None)] {
            let db = touch_db(&dir, name);
            let mut fx = facts(&db, &dir.join(name));
            fx.embed_provider = provider.map(str::to_string);
            fx.embed_model = provider.map(|_| "bge-small".to_string());
            fx.embed_dim = provider.map(|_| 384);
            f.seed(&fx);
        }

        let listing = f.list();
        let unembedded = listing.projects.iter().find(|r| r.name == "c").unwrap();
        assert!(!unembedded.markers.embed_mismatch);
    }

    #[test]
    fn a_minority_embedding_fingerprint_is_flagged() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        for (name, model, dim) in [
            ("a", "bge-small", 384),
            ("b", "bge-small", 384),
            ("odd", "text-embedding-3", 1536),
        ] {
            let db = touch_db(&dir, name);
            let mut fx = facts(&db, &dir.join(name));
            fx.embed_provider = Some("local".to_string());
            fx.embed_model = Some(model.to_string());
            fx.embed_dim = Some(dim);
            f.seed(&fx);
        }

        let listing = f.list();
        let odd = listing.projects.iter().find(|r| r.name == "odd").unwrap();
        let normal = listing.projects.iter().find(|r| r.name == "a").unwrap();
        assert!(odd.markers.embed_mismatch, "the minority must be flagged");
        assert!(!normal.markers.embed_mismatch, "the majority must not be");
    }

    #[test]
    fn a_tied_embedding_split_flags_nobody() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        for (name, dim) in [("a", 384), ("b", 1536)] {
            let db = touch_db(&dir, name);
            let mut fx = facts(&db, &dir.join(name));
            fx.embed_provider = Some("local".to_string());
            fx.embed_model = Some(format!("m{dim}"));
            fx.embed_dim = Some(dim);
            f.seed(&fx);
        }

        assert!(
            f.list().projects.iter().all(|r| !r.markers.embed_mismatch),
            "a 1-1 split has no majority, so flagging either half is noise"
        );
    }

    #[test]
    fn a_uniform_fingerprint_flags_nobody() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        for name in ["a", "b", "c"] {
            let db = touch_db(&dir, name);
            let mut fx = facts(&db, &dir.join(name));
            fx.embed_provider = Some("local".to_string());
            fx.embed_model = Some("bge-small".to_string());
            fx.embed_dim = Some(384);
            f.seed(&fx);
        }
        assert!(f.list().projects.iter().all(|r| !r.markers.embed_mismatch));
    }

    #[test]
    fn rows_are_ordered_most_recently_indexed_first() {
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        for (name, when) in [("old", Some(1000)), ("new", Some(2000)), ("never", None)] {
            let db = touch_db(&dir, name);
            let mut fx = facts(&db, &dir.join(name));
            fx.last_indexed = when;
            f.seed(&fx);
        }

        let names: Vec<_> = f.list().projects.iter().map(|r| r.name.clone()).collect();
        assert_eq!(names, vec!["new", "old", "never"]);
    }

    #[test]
    fn one_unreadable_row_does_not_hide_the_healthy_projects() {
        // Regression: collecting rows into a single Result let one
        // out-of-range count abort the whole listing, so every healthy project
        // vanished AND the caller could not tell that from "no registry".
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "healthy");
        f.seed(&facts(&db, &dir.join("healthy")));
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute(
                "INSERT INTO projects (id, db_path, root, name, symbol_count, last_seen)
                 VALUES ('broken', '/x/db', '/x', 'broken', -1, 1)",
                [],
            )
            .unwrap();
        }

        let listing = f.list();

        assert!(
            listing.available,
            "one bad row must not read as 'no registry'"
        );
        assert!(
            listing.projects.iter().any(|r| r.name == "healthy"),
            "the healthy project must still list"
        );
    }

    #[test]
    fn an_out_of_range_count_reads_as_unknown_not_as_a_failed_row() {
        // A count is a cache of a measurement; a nonsensical stored value means
        // the cache is wrong about that field, not that the row is unusable.
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "odd");
        f.seed(&facts(&db, &dir.join("odd")));
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute(
                "UPDATE projects SET symbol_count = -1, edge_count = 5000000000",
                [],
            )
            .unwrap();
        }

        let row = &f.list().projects[0];
        assert_eq!(row.symbol_count, None, "a negative count is 'not known'");
        assert_eq!(row.edge_count, None, "an overflowing count is 'not known'");
        assert_eq!(row.file_count, Some(412), "the sane columns survive");
    }

    #[test]
    fn a_corrupt_registry_lists_as_unavailable_and_is_quarantined() {
        let f = Fixture::new();
        std::fs::write(&f.registry, b"not a database").unwrap();

        let listing = f.list();

        assert!(!listing.available);
        assert!(listing.projects.is_empty());
        let quarantined = std::fs::read_dir(f._dir.path())
            .unwrap()
            .any(|e| e.unwrap().path().to_string_lossy().contains(".corrupt."));
        assert!(quarantined, "the corrupt file must be preserved aside");
    }

    #[test]
    fn a_missing_state_dir_lists_rows_with_no_live_marker() {
        // Liveness is unknowable without the lock directory; the rows must
        // still list rather than the whole call failing.
        let f = Fixture::new();
        let dir = f._dir.path().to_path_buf();
        let db = touch_db(&dir, "x");
        f.seed(&facts(&db, &dir.join("x")));

        let listing = list_projects_at(&f.registry, None, 8);
        assert!(listing.available);
        assert_eq!(listing.projects.len(), 1);
        assert!(!listing.projects[0].markers.live);
    }
}
