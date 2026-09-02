//! Recording a project in the registry.
//!
//! The whole module obeys one contract: **a registry write never fails its
//! caller**. Registration is bookkeeping that rides on an index pass; a
//! missing state dir, a read-only filesystem, a lock-contended file or a
//! corrupt registry must all cost the caller nothing but a log line. That is
//! why nothing here returns a `Result` to the outside.

use std::path::Path;

use rusqlite::Connection;

use crate::corrupt::quarantine_if_corrupt;
use crate::fingerprint::source_fingerprint;
use crate::model::ProjectFacts;
use crate::open::open_read_write;
use crate::schema::{ensure_schema, read_registry_version, REGISTRY_VERSION};
use crate::slot::slot_for_db;

/// Record `facts` in the machine-local project registry.
///
/// Call this **after** the caller's own transaction has committed, never
/// inside it: the registry is a separate file, so a write here that raced an
/// open graph transaction would be recording an index state that might still
/// roll back.
///
/// Skips the write when the project database is unchanged since the stored
/// fingerprint *and* `facts` carries no `last_indexed` — a caller that just
/// finished indexing has new information by construction and is never skipped.
///
/// Every failure path logs at `warn` and returns. There is no error to ignore.
pub fn record_project(facts: &ProjectFacts) {
    let Some(registry) = crate::path::registry_path() else {
        // No state dir, or the kill switch is set. Not an error.
        return;
    };
    if let Err(e) = try_record(&registry, facts) {
        tracing::warn!(
            registry = %registry.display(),
            project = %facts.db_path.display(),
            error = %e,
            "could not record project in the registry; the index itself is unaffected"
        );
    }
}

/// The fallible core of [`record_project`], so the caller-facing function has
/// exactly one place that swallows and logs.
fn try_record(registry: &Path, facts: &ProjectFacts) -> rusqlite::Result<()> {
    quarantine_if_corrupt(registry);

    let conn = open_read_write(registry)?;
    ensure_schema(&conn)?;

    // An older binary must not write a registry a newer one owns: its columns
    // may mean something else. Reading it is still fine.
    if let Some(found) = read_registry_version(&conn) {
        if found > REGISTRY_VERSION {
            tracing::warn!(
                found,
                supported = REGISTRY_VERSION,
                "project registry was written by a newer cartog; skipping the write"
            );
            return Ok(());
        }
    }

    let id = slot_for_db("serve", &facts.db_path);
    let fingerprint = source_fingerprint(&facts.db_path);

    // `slot_for_db` canonicalizes fully only when the path exists, so a row
    // written while the database was absent carries an id this call will not
    // reproduce (the same drift `read::is_live` tolerates). Left alone, the
    // upsert would insert a *second* row for one physical database and
    // `projects list` would show it twice. Retire the stale row first so the
    // drift converges on one row instead of accumulating.
    retire_drifted_rows(&conn, &id, &facts.db_path);

    if facts.last_indexed.is_none() && is_unchanged(&conn, &id, fingerprint.as_deref()) {
        // The heavy write is redundant, but `last_seen` is not: a "last seen"
        // that does not update when the project is seen is useless, and three
        // doc comments promised a `serve` startup refreshes it. One cheap
        // single-column UPDATE, no COALESCE dance needed.
        touch_last_seen(&conn, &id);
        return Ok(());
    }

    upsert(&conn, &id, facts, fingerprint.as_deref())
}

/// Re-key any row pointing at `db_path` from a drifted id onto `keep_id`.
///
/// Keyed on `db_path`, not on the id: the id is the thing that drifted, so it
/// cannot be what identifies the duplicate. Re-keying rather than deleting
/// preserves the counts and `last_indexed` the drifted row accumulated — it
/// describes the same physical project, so throwing its history away would
/// make a path that merely became canonicalizable look like a fresh index.
///
/// An `UPDATE` can collide if a row already sits at `keep_id`; in that case
/// the drifted row is dropped, since the row under the correct id is the one
/// the upsert is about to refresh. Failure is logged and ignored — a leftover
/// duplicate is untidy, not incorrect, and must never fail the caller's index.
fn retire_drifted_rows(conn: &Connection, keep_id: &str, db_path: &Path) {
    for id in drifted_ids(conn, keep_id, db_path) {
        // Re-key; `OR IGNORE` so a collision with an existing `keep_id` row
        // leaves this one to be deleted below rather than failing.
        if let Err(e) = conn.execute(
            "UPDATE OR IGNORE projects SET id = ?2 WHERE id = ?1",
            rusqlite::params![id, keep_id],
        ) {
            tracing::warn!(
                project = %db_path.display(),
                error = %e,
                "could not re-key a drifted registry row; the project may list twice"
            );
            continue;
        }
        // Still on the old id? It lost the UPDATE to an existing `keep_id` row,
        // which is authoritative — so this one is a true duplicate.
        if let Err(e) = conn.execute(
            "DELETE FROM projects WHERE id = ?1 AND ?1 != ?2",
            rusqlite::params![id, keep_id],
        ) {
            tracing::warn!(
                project = %db_path.display(),
                error = %e,
                "could not drop a duplicate registry row; the project may list twice"
            );
        }
    }
}

/// Ids of rows that describe the same physical database as `db_path` but sit
/// under a different key.
///
/// Compares by **recomputed slot**, not by `db_path` string. A stored path can
/// legitimately differ in text while naming the same file: `absolutize`
/// canonicalizes only when the path exists, so a row written while the database
/// was absent keeps the non-canonical form (`/var/…` where the canonical form
/// is `/private/var/…` on macOS). A string match missed exactly that case, and
/// the upsert then created a second row for one project — the duplicate this
/// function exists to prevent. `slot_for_db` canonicalizes both sides, so it
/// sees through the difference.
fn drifted_ids(conn: &Connection, keep_id: &str, db_path: &Path) -> Vec<String> {
    let Ok(mut stmt) = conn.prepare("SELECT id, db_path FROM projects WHERE id != ?1") else {
        return Vec::new();
    };
    let target = slot_for_db("serve", db_path);
    let rows = stmt.query_map(rusqlite::params![keep_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.filter_map(Result::ok)
        .filter(|(_, stored)| slot_for_db("serve", Path::new(stored)) == target)
        .map(|(id, _)| id)
        .collect()
}

/// Refresh only `last_seen` on an otherwise-unchanged row.
///
/// Failure is logged and ignored: a stale `last_seen` is cosmetic, and this
/// runs on a path whose whole point is that nothing important changed.
fn touch_last_seen(conn: &Connection, id: &str) {
    if let Err(e) = conn.execute(
        "UPDATE projects SET last_seen = ?2 WHERE id = ?1",
        rusqlite::params![id, now_unix()],
    ) {
        tracing::warn!(id = %id, error = %e, "could not refresh registry last_seen");
    }
}

/// True when the stored fingerprint equals the current one, so the row already
/// describes this database's state.
fn is_unchanged(conn: &Connection, id: &str, fingerprint: Option<&str>) -> bool {
    // No current fingerprint (the DB cannot be stat'd) means we cannot claim
    // it is unchanged.
    let Some(current) = fingerprint else {
        return false;
    };
    let stored: Option<String> = conn
        .query_row(
            "SELECT source_fingerprint FROM projects WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    stored.as_deref() == Some(current)
}

/// One statement, no transaction: a single row's upsert is already atomic, and
/// a transaction spanning more would let one project's write block another's.
///
/// `COALESCE(excluded.x, projects.x)` is what makes "write only what you know"
/// safe — a `serve` startup refreshing `last_seen` cannot null out counts an
/// `index` recorded.
fn upsert(
    conn: &Connection,
    id: &str,
    facts: &ProjectFacts,
    fingerprint: Option<&str>,
) -> rusqlite::Result<()> {
    let languages = facts
        .languages
        .as_ref()
        .map(|langs| encode_languages(langs));
    conn.execute(
        "INSERT INTO projects (
             id, db_path, root, name, languages, schema_version,
             file_count, symbol_count, edge_count, resolved_count, embedding_count,
             embed_provider, embed_model, embed_dim,
             source_fingerprint, last_indexed, last_seen
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
         ON CONFLICT(id) DO UPDATE SET
             db_path            = excluded.db_path,
             root               = excluded.root,
             name               = excluded.name,
             languages          = COALESCE(excluded.languages,          projects.languages),
             schema_version     = COALESCE(excluded.schema_version,     projects.schema_version),
             file_count         = COALESCE(excluded.file_count,         projects.file_count),
             symbol_count       = COALESCE(excluded.symbol_count,       projects.symbol_count),
             edge_count         = COALESCE(excluded.edge_count,         projects.edge_count),
             resolved_count     = COALESCE(excluded.resolved_count,     projects.resolved_count),
             embedding_count    = COALESCE(excluded.embedding_count,    projects.embedding_count),
             embed_provider     = COALESCE(excluded.embed_provider,     projects.embed_provider),
             embed_model        = COALESCE(excluded.embed_model,        projects.embed_model),
             embed_dim          = COALESCE(excluded.embed_dim,          projects.embed_dim),
             source_fingerprint = COALESCE(excluded.source_fingerprint, projects.source_fingerprint),
             last_indexed       = COALESCE(excluded.last_indexed,       projects.last_indexed),
             last_seen          = excluded.last_seen",
        rusqlite::params![
            id,
            facts.db_path.to_string_lossy(),
            facts.root.to_string_lossy(),
            facts.name(),
            languages,
            facts.schema_version,
            facts.file_count,
            facts.symbol_count,
            facts.edge_count,
            facts.resolved_count,
            facts.embedding_count,
            facts.embed_provider,
            facts.embed_model,
            facts.embed_dim,
            fingerprint,
            facts.last_indexed,
            now_unix(),
        ],
    )?;
    Ok(())
}

/// Encode languages as `name:count` pairs joined by newlines.
///
/// Not JSON: it avoids a serde dependency in this crate for a value that is
/// only ever written and read here, and a newline cannot appear in a language
/// tag. Counts that fail to parse on read are dropped, not errors.
fn encode_languages(langs: &[(String, u32)]) -> String {
    langs
        .iter()
        .map(|(name, count)| format!("{name}:{count}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Decode what [`encode_languages`] wrote, skipping anything malformed.
pub(crate) fn decode_languages(encoded: &str) -> Vec<(String, u32)> {
    encoded
        .lines()
        .filter_map(|line| {
            // rsplit_once: a language tag could in principle contain a colon,
            // the count never does.
            let (name, count) = line.rsplit_once(':')?;
            Some((name.to_string(), count.parse().ok()?))
        })
        .collect()
}

/// Seconds since the Unix epoch, or 0 if the clock is before it.
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The upsert, exposed for in-crate tests so the read-side fixtures seed rows
/// through the real write statement rather than a hand-copied one that could
/// drift from it.
#[cfg(test)]
pub(crate) fn upsert_for_test(
    conn: &Connection,
    id: &str,
    facts: &ProjectFacts,
    fingerprint: Option<&str>,
) -> rusqlite::Result<()> {
    upsert(conn, id, facts, fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn languages_round_trip() {
        let langs = vec![("rust".to_string(), 412), ("markdown".to_string(), 30)];
        assert_eq!(decode_languages(&encode_languages(&langs)), langs);
    }

    #[test]
    fn an_empty_language_list_round_trips_as_empty() {
        assert_eq!(decode_languages(&encode_languages(&[])), vec![]);
    }

    #[test]
    fn a_malformed_language_line_is_skipped_not_fatal() {
        // A hand-edited or truncated value must degrade to the rows it can
        // parse rather than poisoning the whole listing.
        assert_eq!(
            decode_languages("rust:412\ngarbage\nmarkdown:not-a-number\ngo:7"),
            vec![("rust".to_string(), 412), ("go".to_string(), 7)]
        );
    }

    /// Registry seeded through the real upsert, with a real DB file so the
    /// fingerprint is a live value rather than a fixture constant.
    struct WriteFixture {
        dir: tempfile::TempDir,
        registry: std::path::PathBuf,
    }

    impl WriteFixture {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let registry = dir.path().join("projects.sqlite");
            let conn = crate::open::open_read_write(&registry).unwrap();
            ensure_schema(&conn).unwrap();
            Self { dir, registry }
        }

        fn project(&self, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
            // Canonicalize the base: `identity_only` absolutizes what it
            // stores, and on macOS /var canonicalizes to /private/var. A
            // fixture comparing the raw temp path would chase that difference
            // rather than the behaviour under test.
            let base = self.dir.path().canonicalize().unwrap();
            let root = base.join(name);
            std::fs::create_dir_all(&root).unwrap();
            let db = root.join("db.sqlite");
            std::fs::write(&db, b"seed").unwrap();
            (root, db)
        }

        /// Drive the real fallible core, so the env-dependent wrapper is the
        /// only thing these tests do not exercise.
        fn record(&self, facts: &ProjectFacts) {
            try_record(&self.registry, facts).unwrap();
        }

        fn row(&self, db: &Path) -> Option<crate::model::ProjectRow> {
            crate::read::list_projects_at(&self.registry, None, 8)
                .projects
                .into_iter()
                .find(|r| r.db_path == db)
        }

        fn stored_fingerprint(&self, db: &Path) -> Option<String> {
            let conn = crate::open::open_read_only(&self.registry).unwrap();
            conn.query_row(
                "SELECT source_fingerprint FROM projects WHERE id = ?1",
                rusqlite::params![slot_for_db("serve", db)],
                |r| r.get(0),
            )
            .ok()
            .flatten()
        }
    }

    fn counted(db: &Path, root: &Path, symbols: u32) -> ProjectFacts {
        ProjectFacts {
            symbol_count: Some(symbols),
            file_count: Some(10),
            languages: Some(vec![("rust".to_string(), 10)]),
            schema_version: Some(8),
            last_indexed: Some(1_700_000_000),
            ..ProjectFacts::identity_only(db, root)
        }
    }

    #[test]
    fn a_first_write_creates_the_row_with_its_counts() {
        let f = WriteFixture::new();
        let (root, db) = f.project("a");

        f.record(&counted(&db, &root, 100));

        let row = f.row(&db).expect("the row must exist");
        assert_eq!(row.symbol_count, Some(100));
        assert_eq!(row.name, "a");
    }

    #[test]
    fn an_identity_only_write_does_not_clobber_counts_an_index_recorded() {
        // The COALESCE invariant, and the most important test in this file: a
        // `serve` startup refreshing last_seen must not erase what an `index`
        // measured. Without it, opening a project would blank its own row.
        //
        // The DB must be *changed* between the two writes, otherwise the
        // fingerprint skip suppresses the second one and this test passes even
        // with COALESCE removed — it would assert the skip, not the merge.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&counted(&db, &root, 8134));
        std::fs::write(&db, b"seed plus a change so the write is not skipped").unwrap();

        f.record(&ProjectFacts::identity_only(&db, &root));

        let row = f.row(&db).unwrap();
        assert_eq!(
            row.symbol_count,
            Some(8134),
            "an identity-only write must leave counts alone"
        );
        assert_eq!(row.file_count, Some(10));
        assert_eq!(row.languages, vec![("rust".to_string(), 10)]);
        assert_eq!(row.last_indexed, Some(1_700_000_000));
    }

    #[test]
    fn a_later_write_overwrites_counts_it_does_supply() {
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&counted(&db, &root, 100));

        f.record(&counted(&db, &root, 200));

        assert_eq!(f.row(&db).unwrap().symbol_count, Some(200));
    }

    #[test]
    fn an_unchanged_database_skips_a_second_identity_only_write() {
        // The fingerprint guard: a repeated serve startup on an untouched DB
        // must not rewrite the row.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&counted(&db, &root, 100));
        let fingerprint = f.stored_fingerprint(&db).expect("a fingerprint is stored");

        // Mutate a column directly, then re-record identity-only. If the skip
        // works the mutation survives, because no write happened.
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute("UPDATE projects SET name = 'SENTINEL'", [])
                .unwrap();
        }
        f.record(&ProjectFacts::identity_only(&db, &root));

        assert_eq!(
            f.row(&db).unwrap().name,
            "SENTINEL",
            "an unchanged DB must skip the write entirely"
        );
        assert_eq!(
            f.stored_fingerprint(&db).as_deref(),
            Some(fingerprint.as_str())
        );
    }

    #[test]
    fn a_skipped_write_still_refreshes_last_seen() {
        // Three doc comments promise a `serve` startup refreshes `last_seen`.
        // The fingerprint skip used to short-circuit the whole upsert, so on an
        // idle database it refreshed nothing — the opposite of the contract.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&counted(&db, &root, 100));
        // Backdate so a refresh is observable regardless of clock resolution.
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute("UPDATE projects SET last_seen = 1", [])
                .unwrap();
        }

        // Identity-only on an unchanged DB: the skip path.
        f.record(&ProjectFacts::identity_only(&db, &root));

        let row = f.row(&db).unwrap();
        assert!(
            row.last_seen > 1,
            "a skipped write must still refresh last_seen, got {}",
            row.last_seen
        );
        assert_eq!(
            row.symbol_count,
            Some(100),
            "and must still not disturb the counts"
        );
    }

    #[test]
    fn a_changed_database_is_re_recorded() {
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&counted(&db, &root, 100));
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute("UPDATE projects SET name = 'SENTINEL'", [])
                .unwrap();
        }

        // Grow the DB file: the fingerprint must notice.
        std::fs::write(&db, b"seed plus rather more content than before").unwrap();
        f.record(&ProjectFacts::identity_only(&db, &root));

        assert_eq!(
            f.row(&db).unwrap().name,
            "a",
            "a changed DB must be re-recorded, restoring the real name"
        );
    }

    #[test]
    fn a_write_carrying_last_indexed_is_never_skipped_by_the_fingerprint() {
        // A caller that just finished an indexing pass has new information by
        // construction. Letting a fingerprint coincidence suppress it would
        // lose last_indexed forever.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&counted(&db, &root, 100));
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute("UPDATE projects SET name = 'SENTINEL'", [])
                .unwrap();
        }

        // Same DB bytes, so the fingerprint is unchanged — but last_indexed is set.
        f.record(&counted(&db, &root, 555));

        let row = f.row(&db).unwrap();
        assert_eq!(row.name, "a", "an indexing write must not be skipped");
        assert_eq!(row.symbol_count, Some(555));
    }

    #[test]
    fn a_drifted_id_converges_on_one_row_instead_of_duplicating() {
        // Regression: `slot_for_db` canonicalizes fully only when the path
        // exists, so a row written while the DB was absent carries an id a
        // later call will not reproduce. Left alone, the upsert inserted a
        // SECOND row for one physical database and `projects list` showed the
        // project twice.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            let facts = counted(&db, &root, 8134);
            upsert(&conn, "serve-deadbeefdeadbeef", &facts, Some("stale-fp")).unwrap();
        }

        f.record(&ProjectFacts::identity_only(&db, &root));

        let listing = crate::read::list_projects_at(&f.registry, None, 8);
        assert_eq!(
            listing.projects.len(),
            1,
            "one physical database must never occupy two registry rows"
        );
        assert_eq!(
            listing.projects[0].id,
            slot_for_db("serve", &db),
            "the surviving row must carry the reproducible id"
        );
    }

    #[test]
    fn a_row_stored_under_a_non_canonical_path_converges_instead_of_duplicating() {
        // The drift that a `db_path` string match missed: `absolutize`
        // canonicalizes only when the path exists, so a row written while the
        // database was absent keeps the non-canonical form (`/var/…` where the
        // canonical form is `/private/var/…` on macOS). Different string, same
        // file — the old dedup matched neither row and the upsert added a
        // second one.
        //
        // The sibling test seeds drift by editing only the `id`, keeping
        // `db_path` byte-identical, so it cannot catch this.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        let canonical = db.to_string_lossy().into_owned();
        let non_canonical = canonical.replace("/private/var/", "/var/");
        if non_canonical == canonical {
            // Not macOS, or no such prefix: the case under test cannot arise.
            return;
        }
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            let stale = ProjectFacts {
                // Bypass `absolutize` to reproduce what a DB-absent write stored.
                db_path: std::path::PathBuf::from(&non_canonical),
                ..counted(&db, &root, 8134)
            };
            upsert(&conn, "serve-stale0000000000", &stale, Some("stale-fp")).unwrap();
        }

        f.record(&ProjectFacts::identity_only(&db, &root));

        let listing = crate::read::list_projects_at(&f.registry, None, 8);
        assert_eq!(
            listing.projects.len(),
            1,
            "one physical database must never occupy two rows, even when the \
             stored paths differ textually"
        );
        assert_eq!(listing.projects[0].id, slot_for_db("serve", &db));
        assert_eq!(
            listing.projects[0].symbol_count,
            Some(8134),
            "the re-keyed row must keep the counts it accumulated"
        );
    }

    #[test]
    fn re_keying_a_drifted_row_preserves_the_counts_it_accumulated() {
        // It is the same project, so its history must survive: a path that
        // merely became canonicalizable must not look like a fresh index.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            let facts = counted(&db, &root, 8134);
            upsert(&conn, "serve-deadbeefdeadbeef", &facts, Some("stale-fp")).unwrap();
        }

        f.record(&ProjectFacts::identity_only(&db, &root));

        let row = f.row(&db).expect("the re-keyed row must be findable");
        assert_eq!(row.symbol_count, Some(8134), "counts must carry forward");
        assert_eq!(row.last_indexed, Some(1_700_000_000));
    }

    #[test]
    fn a_write_is_declined_when_the_registry_is_newer_than_this_binary() {
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute(
                "UPDATE metadata SET value = '99' WHERE key = 'registry_version'",
                [],
            )
            .unwrap();
        }

        f.record(&counted(&db, &root, 100));

        assert!(
            f.row(&db).is_none(),
            "an older binary must not write a registry a newer one owns"
        );
    }

    #[test]
    #[serial]
    fn recording_never_fails_its_caller_when_the_registry_is_unwritable() {
        // The whole failure contract: registration is bookkeeping riding on an
        // index pass, so it must cost the caller nothing but a log line.
        //
        // `record_project` resolves the registry from the environment, so this
        // test MUST redirect it. Without the guard it inserted a row into the
        // developer's own user-global registry on every `cargo test` — pointing
        // at a `TempDir` that is deleted moments later. `#[serial]` because the
        // override is process-global.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        let _env = RegistryEnvGuard::redirect(&f.dir.path().join("redirected.sqlite"));
        // A directory where the registry file should be: every open fails.
        let blocked = f.dir.path().join("blocked");
        std::fs::create_dir_all(blocked.join("projects.sqlite")).unwrap();

        // try_record surfaces the error; record_project is what swallows it.
        assert!(try_record(&blocked.join("projects.sqlite"), &counted(&db, &root, 1)).is_err());
        // The public entry point must not panic or propagate.
        record_project(&counted(&db, &root, 1));
    }

    #[test]
    #[serial]
    fn recording_is_a_no_op_when_the_registry_is_disabled() {
        // The kill switch must reach the public entry point, not just
        // `registry_path()`: `record_project` is what every hook calls.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        let _env = RegistryEnvGuard::disable();

        record_project(&counted(&db, &root, 1));

        // Nothing was written anywhere — including the fixture's own registry.
        assert!(
            f.row(&db).is_none(),
            "a disabled registry must swallow the write entirely"
        );
    }

    /// RAII redirect of `CARTOG_REGISTRY`, restoring the previous value on drop.
    ///
    /// Any test that calls the public `record_project` needs this: it resolves
    /// the registry from the live environment, so an unguarded test writes into
    /// the developer's real user-global registry.
    struct RegistryEnvGuard(Option<std::ffi::OsString>);

    impl RegistryEnvGuard {
        fn set(value: &std::ffi::OsStr) -> Self {
            let prev = std::env::var_os(crate::path::REGISTRY_ENV);
            std::env::set_var(crate::path::REGISTRY_ENV, value);
            Self(prev)
        }

        fn redirect(path: &Path) -> Self {
            Self::set(path.as_os_str())
        }

        fn disable() -> Self {
            Self::set(std::ffi::OsStr::new(""))
        }
    }

    impl Drop for RegistryEnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var(crate::path::REGISTRY_ENV, v),
                None => std::env::remove_var(crate::path::REGISTRY_ENV),
            }
        }
    }

    #[test]
    fn a_language_tag_containing_a_colon_keeps_its_count() {
        // rsplit_once, not split_once: the count is the last field.
        assert_eq!(decode_languages("c:pp:5"), vec![("c:pp".to_string(), 5)]);
    }
}
