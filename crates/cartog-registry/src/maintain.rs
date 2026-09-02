//! Removing rows from the registry.
//!
//! Both operations touch the registry only. Neither ever deletes, moves, or
//! opens a project's index — forgetting a project means forgetting where it
//! is, not destroying it.

use std::path::Path;

use crate::model::ProjectRow;
use crate::open::open_read_write;
use crate::schema::ensure_schema;

/// Outcome of a maintenance operation, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    /// Rows actually dropped (empty for a dry run).
    pub dropped: Vec<String>,
    /// True when the registry could not be reached at all.
    pub unavailable: bool,
    /// Candidates a `forget` refused to act on because the target matched
    /// more than one project. Empty on every other path.
    ///
    /// Names are non-unique by construction (`~/w1/api` and `~/w2/api` both
    /// have the name `api`), so an ambiguous target must be reported, never
    /// resolved by deleting all of them.
    pub ambiguous: Vec<String>,
}

impl Removed {
    fn unavailable() -> Self {
        Self {
            dropped: Vec::new(),
            unavailable: true,
            ambiguous: Vec::new(),
        }
    }

    fn dropped(ids: Vec<String>) -> Self {
        Self {
            dropped: ids,
            unavailable: false,
            ambiguous: Vec::new(),
        }
    }
}

/// Drop one project's row, identified by its registry id, root path, database
/// path, root name, or declared `[project] name`.
///
/// Accepting all of them is what makes the command usable: the id is what
/// `projects list --json` reports, the path or name is what a person
/// remembers — and the name they *see* in a listing is the declared one when
/// the project has one, so matching only the root basename would reject the
/// argument the listing just showed them. A name matches only when it
/// identifies exactly one project —
/// two workspaces each holding an `api` directory produce two rows named
/// `api`, and deleting both from one unambiguous-looking argument would
/// deregister a project the user never named. Such a target drops nothing and
/// comes back in [`Removed::ambiguous`] for the caller to report.
///
/// The project's index is left entirely alone: forgetting a project means
/// forgetting where it is, never destroying it.
#[must_use]
pub fn forget_project_at(registry: &Path, target: &str) -> Removed {
    // Check existence first: `open_read_write` creates the file. A *removal*
    // command must never materialize machine-global state — `forget` on a
    // machine with no registry left a fresh empty one behind. (`prune` was
    // already correct: it reads through `list_projects_at`, which guards on
    // existence.)
    if !registry.exists() {
        return Removed::unavailable();
    }
    let Ok(conn) = open_read_write(registry) else {
        return Removed::unavailable();
    };
    if ensure_schema(&conn).is_err() {
        return Removed::unavailable();
    }
    let matches: Vec<String> = match conn.prepare(
        "SELECT id FROM projects
          WHERE id = ?1 OR root = ?1 OR db_path = ?1 OR name = ?1 OR declared_name = ?1",
    ) {
        Ok(mut stmt) => stmt
            .query_map(rusqlite::params![target], |row| row.get::<_, String>(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default(),
        Err(_) => return Removed::unavailable(),
    };

    if matches.len() > 1 {
        return Removed {
            dropped: Vec::new(),
            unavailable: false,
            ambiguous: matches,
        };
    }

    let mut dropped = Vec::new();
    for id in matches {
        match conn.execute("DELETE FROM projects WHERE id = ?1", rusqlite::params![id]) {
            Ok(_) => dropped.push(id),
            Err(e) => tracing::warn!(id = %id, error = %e, "could not drop registry row"),
        }
    }
    Removed::dropped(dropped)
}

/// Drop every row whose database file no longer exists.
///
/// The predicate is `db_path` existence alone; it never consults the row's
/// `id`, so an id that no longer recomputes (see `read::is_live`) can never
/// cause a live project to be pruned.
#[must_use]
pub fn prune_projects_at(registry: &Path, dry_run: bool) -> Removed {
    let listing = crate::read::list_projects_at(registry, None, u32::MAX);
    if !listing.available {
        return Removed::unavailable();
    }
    let stale: Vec<&ProjectRow> = listing
        .projects
        .iter()
        .filter(|row| !row.db_path.exists())
        .collect();

    if dry_run {
        // A dry run reports what *would* go, so the caller renders the same
        // list either way, but drops nothing.
        return Removed::dropped(stale.iter().map(|r| r.id.clone()).collect());
    }

    let Ok(conn) = open_read_write(registry) else {
        return Removed::unavailable();
    };
    let mut dropped = Vec::new();
    for row in stale {
        match conn.execute(
            "DELETE FROM projects WHERE id = ?1",
            rusqlite::params![row.id],
        ) {
            Ok(_) => dropped.push(row.id.clone()),
            Err(e) => tracing::warn!(id = %row.id, error = %e, "could not prune registry row"),
        }
    }
    Removed::dropped(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectFacts;
    use crate::slot::slot_for_db;

    struct Fixture {
        dir: tempfile::TempDir,
        registry: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let registry = dir.path().join("projects.sqlite");
            Self { dir, registry }
        }

        /// Create a real project tree + DB file and register it.
        fn add(&self, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
            // See the note in write.rs's fixture: the writer absolutizes, so
            // the fixture must too.
            let base = self.dir.path().canonicalize().unwrap();
            let root = base.join(name);
            std::fs::create_dir_all(root.join(".cartog")).unwrap();
            let db = root.join(".cartog").join("db.sqlite");
            std::fs::write(&db, b"").unwrap();

            let conn = open_read_write(&self.registry).unwrap();
            ensure_schema(&conn).unwrap();
            let facts = ProjectFacts::identity_only(&db, &root);
            crate::write::upsert_for_test(&conn, &slot_for_db("serve", &db), &facts, None).unwrap();
            (root, db)
        }

        fn ids(&self) -> Vec<String> {
            crate::read::list_projects_at(&self.registry, None, 8)
                .projects
                .into_iter()
                .map(|r| r.id)
                .collect()
        }
    }

    #[test]
    fn forget_by_id_drops_exactly_one_row() {
        let f = Fixture::new();
        let (_, db_a) = f.add("a");
        f.add("b");
        let id_a = slot_for_db("serve", &db_a);

        let out = forget_project_at(&f.registry, &id_a);

        assert_eq!(out.dropped, vec![id_a.clone()]);
        assert_eq!(f.ids().len(), 1);
        assert!(!f.ids().contains(&id_a));
    }

    #[test]
    fn forget_by_root_path_drops_the_row() {
        let f = Fixture::new();
        let (root, _) = f.add("a");

        let out = forget_project_at(&f.registry, &root.to_string_lossy());

        assert_eq!(out.dropped.len(), 1);
        assert!(f.ids().is_empty());
    }

    #[test]
    fn forget_by_name_drops_the_row() {
        let f = Fixture::new();
        f.add("svc-billing");

        let out = forget_project_at(&f.registry, "svc-billing");

        assert_eq!(out.dropped.len(), 1);
    }

    #[test]
    fn forget_by_the_declared_name_drops_the_row() {
        // Users type the name they see in `projects list`, which is the
        // declared one when the project has one.
        let f = Fixture::new();
        let (_, db) = f.add("api");
        {
            let conn = open_read_write(&f.registry).unwrap();
            conn.execute(
                "UPDATE projects SET declared_name = 'svc-billing' WHERE id = ?1",
                rusqlite::params![slot_for_db("serve", &db)],
            )
            .unwrap();
        }

        let out = forget_project_at(&f.registry, "svc-billing");

        assert_eq!(out.dropped, vec![slot_for_db("serve", &db)]);
        assert!(f.ids().is_empty());
    }

    #[test]
    fn forget_by_an_ambiguous_name_drops_nothing_and_reports_the_candidates() {
        // Names are non-unique by construction: two workspaces each holding an
        // `api` directory produce two rows named `api`. Deleting both from one
        // unambiguous-looking argument would deregister a project the user
        // never named.
        let f = Fixture::new();
        for ws in ["w1", "w2"] {
            let root = f.dir.path().join(ws).join("api");
            std::fs::create_dir_all(&root).unwrap();
            let db = root.join("db.sqlite");
            std::fs::write(&db, b"").unwrap();
            let conn = open_read_write(&f.registry).unwrap();
            ensure_schema(&conn).unwrap();
            let facts = ProjectFacts::identity_only(&db, &root);
            crate::write::upsert_for_test(&conn, &slot_for_db("serve", &db), &facts, None).unwrap();
        }

        let out = forget_project_at(&f.registry, "api");

        assert!(
            out.dropped.is_empty(),
            "an ambiguous target must drop nothing"
        );
        assert_eq!(out.ambiguous.len(), 2, "both candidates must be reported");
        assert_eq!(f.ids().len(), 2, "both projects must survive");
    }

    #[test]
    fn forget_by_an_exact_id_still_works_when_the_name_is_ambiguous() {
        // The escape hatch the ambiguity check depends on: the id is unique
        // even when the name is not.
        let f = Fixture::new();
        let mut ids = Vec::new();
        for ws in ["w1", "w2"] {
            let root = f.dir.path().join(ws).join("api");
            std::fs::create_dir_all(&root).unwrap();
            let db = root.join("db.sqlite");
            std::fs::write(&db, b"").unwrap();
            let conn = open_read_write(&f.registry).unwrap();
            ensure_schema(&conn).unwrap();
            let facts = ProjectFacts::identity_only(&db, &root);
            let id = slot_for_db("serve", &db);
            crate::write::upsert_for_test(&conn, &id, &facts, None).unwrap();
            ids.push(id);
        }

        let out = forget_project_at(&f.registry, &ids[0]);

        assert_eq!(out.dropped, vec![ids[0].clone()]);
        assert_eq!(f.ids(), vec![ids[1].clone()]);
    }

    #[test]
    fn prune_still_reaps_a_stale_row_when_another_row_is_unreadable() {
        // Regression: an unreadable row used to abort the whole listing, which
        // made prune report "unavailable" and drop nothing — so the broken row
        // could never be cleaned up, which is the one job prune has.
        let f = Fixture::new();
        let (_, db_gone) = f.add("gone");
        std::fs::remove_file(&db_gone).unwrap();
        {
            let conn = open_read_write(&f.registry).unwrap();
            conn.execute(
                "INSERT INTO projects (id, db_path, root, name, symbol_count, last_seen)
                 VALUES ('broken', '/nonexistent/db', '/nonexistent', 'broken', -1, 1)",
                [],
            )
            .unwrap();
        }

        let out = prune_projects_at(&f.registry, false);

        assert!(!out.unavailable, "one bad row must not disable prune");
        assert!(
            out.dropped.contains(&slot_for_db("serve", &db_gone)),
            "the stale row must still be reaped"
        );
    }

    #[test]
    fn forget_leaves_the_index_on_disk_untouched() {
        // The invariant that makes `forget` safe to run: it forgets where a
        // project is, it does not destroy it.
        let f = Fixture::new();
        let (root, db) = f.add("keepme");

        let _ = forget_project_at(&f.registry, "keepme");

        assert!(db.exists(), "forget must never delete a project's index");
        assert!(root.exists(), "forget must never touch a project's tree");
    }

    #[test]
    fn forget_an_unknown_target_drops_nothing_and_is_not_an_error() {
        let f = Fixture::new();
        f.add("a");

        let out = forget_project_at(&f.registry, "no-such-project");

        assert!(out.dropped.is_empty());
        assert!(!out.unavailable);
        assert_eq!(f.ids().len(), 1);
    }

    #[test]
    fn prune_drops_exactly_the_rows_whose_database_is_gone() {
        let f = Fixture::new();
        let (_, db_gone) = f.add("gone");
        let (_, db_kept) = f.add("kept");
        std::fs::remove_file(&db_gone).unwrap();

        let out = prune_projects_at(&f.registry, false);

        assert_eq!(out.dropped, vec![slot_for_db("serve", &db_gone)]);
        assert_eq!(f.ids(), vec![slot_for_db("serve", &db_kept)]);
    }

    #[test]
    fn prune_keeps_every_row_whose_database_still_exists() {
        let f = Fixture::new();
        f.add("a");
        f.add("b");

        let out = prune_projects_at(&f.registry, false);

        assert!(out.dropped.is_empty());
        assert_eq!(f.ids().len(), 2);
    }

    #[test]
    fn a_prune_dry_run_reports_candidates_without_dropping_them() {
        let f = Fixture::new();
        let (_, db_gone) = f.add("gone");
        std::fs::remove_file(&db_gone).unwrap();

        let out = prune_projects_at(&f.registry, true);

        assert_eq!(out.dropped, vec![slot_for_db("serve", &db_gone)]);
        assert_eq!(f.ids().len(), 1, "a dry run must change nothing");
    }

    #[test]
    fn concurrent_writers_on_distinct_projects_all_land() {
        // The ON CONFLICT upsert plus the short busy timeout must let a burst
        // of index passes across different projects all register. No timing
        // assertions — only that every row is present afterwards.
        let f = Fixture::new();
        {
            let conn = open_read_write(&f.registry).unwrap();
            ensure_schema(&conn).unwrap();
        }
        let dir = f.dir.path().to_path_buf();
        let registry = f.registry.clone();

        std::thread::scope(|scope| {
            for i in 0..8 {
                let dir = dir.clone();
                let registry = registry.clone();
                scope.spawn(move || {
                    let root = dir.join(format!("p{i}"));
                    std::fs::create_dir_all(&root).unwrap();
                    let db = root.join("db.sqlite");
                    std::fs::write(&db, b"").unwrap();
                    let conn = open_read_write(&registry).unwrap();
                    let facts = ProjectFacts::identity_only(&db, &root);
                    crate::write::upsert_for_test(&conn, &slot_for_db("serve", &db), &facts, None)
                        .unwrap();
                });
            }
        });

        assert_eq!(f.ids().len(), 8, "every concurrent writer must land a row");
    }

    #[test]
    fn concurrent_writers_on_the_same_project_leave_exactly_one_row() {
        let f = Fixture::new();
        let (root, db) = f.add("shared");
        let registry = f.registry.clone();

        std::thread::scope(|scope| {
            for _ in 0..8 {
                let registry = registry.clone();
                let root = root.clone();
                let db = db.clone();
                scope.spawn(move || {
                    let conn = open_read_write(&registry).unwrap();
                    let facts = ProjectFacts::identity_only(&db, &root);
                    crate::write::upsert_for_test(&conn, &slot_for_db("serve", &db), &facts, None)
                        .unwrap();
                });
            }
        });

        assert_eq!(
            f.ids().len(),
            1,
            "ON CONFLICT(id) must update rather than duplicate"
        );
    }

    #[test]
    fn maintenance_on_an_absent_registry_reports_unavailable() {
        let f = Fixture::new();
        assert!(prune_projects_at(&f.registry, false).unavailable);
        assert!(forget_project_at(&f.registry, "x").unavailable);
    }

    #[test]
    fn neither_maintenance_command_creates_a_registry_that_did_not_exist() {
        // Regression: `forget` opened read-write before checking for a match,
        // and `open_read_write` creates the file — so forgetting a project on a
        // machine with no registry left an empty 20 KB one behind. A removal
        // command must not materialize machine-global state.
        let f = Fixture::new();

        let _ = forget_project_at(&f.registry, "nosuch");
        let _ = prune_projects_at(&f.registry, false);
        let _ = prune_projects_at(&f.registry, true);

        assert!(
            !f.registry.exists(),
            "no maintenance command may create the registry"
        );
    }
}
