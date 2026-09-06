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
use crate::describe::{truncate_at_word_boundary, DESCRIPTION_MAX_CHARS};
use crate::fingerprint::source_fingerprint;
use crate::model::{Declared, DeclaredUpdate, ProjectFacts};
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
        //
        // The declared identity is the exception to the skip: editing
        // `README.md` or `.cartog.toml` changes no byte of the graph database,
        // so the fingerprint cannot see it. A config-aware writer's `Set`
        // therefore still lands here.
        touch_last_seen(&conn, &id);
        if let DeclaredUpdate::Set(declared) = &facts.declared {
            write_declared_if_changed(&conn, &id, declared);
        }
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

/// Update the declared columns on the fingerprint-skip path, if they differ.
///
/// Compared before writing so a repeated no-op pass stays a single
/// `last_seen` update: the point of the skip path is that it is cheap.
/// Failure is logged and ignored, like every other write on this path.
fn write_declared_if_changed(conn: &Connection, id: &str, declared: &Declared) {
    let (name, text, src) = declared_params(declared);
    let stored: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT declared_name, description, description_src FROM projects WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();
    if stored.as_ref() == Some(&(name.clone(), text.clone(), src.map(str::to_string))) {
        return;
    }
    if let Err(e) = conn.execute(
        "UPDATE projects
            SET declared_name = ?2, description = ?3, description_src = ?4
          WHERE id = ?1",
        rusqlite::params![id, name, text, src],
    ) {
        tracing::warn!(
            id = %id,
            error = %e,
            "could not refresh the project's declared name/description"
        );
    }
}

/// The three declared columns as bind values.
///
/// The description text is hard-capped here regardless of source: config
/// validation caps it too, but the registry must not be able to grow
/// unbounded from a caller that skipped it.
fn declared_params(declared: &Declared) -> (Option<String>, Option<String>, Option<&'static str>) {
    let name = declared.name.clone();
    let text = declared
        .description
        .as_ref()
        .map(|d| truncate_at_word_boundary(&d.text, DESCRIPTION_MAX_CHARS));
    let src = declared.description.as_ref().map(|d| d.source.as_str());
    (name, text, src)
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
///
/// The three declared columns cannot use `COALESCE`: `Set(None)` must *clear*
/// them, which `COALESCE` would read as "unknown, keep". They are gated on the
/// `?18` flag instead — 1 for [`DeclaredUpdate::Set`], 0 for `Keep`, which
/// keeps whatever is stored (`NULL` on a fresh insert).
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
    let (declared_name, description, description_src) = match &facts.declared {
        DeclaredUpdate::Set(d) => declared_params(d),
        DeclaredUpdate::Keep => (None, None, None),
    };
    let set_declared = matches!(facts.declared, DeclaredUpdate::Set(_));
    conn.execute(
        "INSERT INTO projects (
             id, db_path, root, name, languages, schema_version,
             file_count, symbol_count, edge_count, resolved_count, embedding_count,
             embed_provider, embed_model, embed_dim,
             source_fingerprint, last_indexed, last_seen,
             declared_name, description, description_src
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?19,?20,?21)
         ON CONFLICT(id) DO UPDATE SET
             db_path            = excluded.db_path,
             root               = excluded.root,
             name               = excluded.name,
             declared_name      = CASE WHEN ?18 THEN excluded.declared_name
                                       ELSE projects.declared_name END,
             description        = CASE WHEN ?18 THEN excluded.description
                                       ELSE projects.description END,
             description_src    = CASE WHEN ?18 THEN excluded.description_src
                                       ELSE projects.description_src END,
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
            set_declared,
            declared_name,
            description,
            description_src,
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
    fn a_v2_binary_still_refuses_to_write_a_v3_registry() {
        // The version gate must survive the v1 → v2 migration: the next bump is
        // the case it exists for, not a distant 99.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute(
                "UPDATE metadata SET value = '3' WHERE key = 'registry_version'",
                [],
            )
            .unwrap();
        }

        f.record(&counted(&db, &root, 100));

        assert!(f.row(&db).is_none());
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

    fn declared(name: Option<&str>, text: Option<&str>) -> DeclaredUpdate {
        DeclaredUpdate::Set(Declared {
            name: name.map(str::to_string),
            description: text.map(|t| crate::model::Description {
                text: t.to_string(),
                source: crate::model::DescriptionSource::Config,
            }),
        })
    }

    #[test]
    fn a_set_declared_write_stores_the_name_description_and_source() {
        let f = WriteFixture::new();
        let (root, db) = f.project("a");

        f.record(&ProjectFacts {
            declared: declared(Some("svc-billing"), Some("Invoices.")),
            ..counted(&db, &root, 100)
        });

        let row = f.row(&db).unwrap();
        assert_eq!(row.declared_name.as_deref(), Some("svc-billing"));
        let d = row.description.expect("a description");
        assert_eq!(d.text, "Invoices.");
        assert_eq!(d.source, crate::model::DescriptionSource::Config);
    }

    #[test]
    fn a_set_with_no_description_clears_a_previously_stored_one() {
        // Removing `[project] description` and the README paragraph must clear
        // the row — this is why the declared columns cannot use COALESCE.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&ProjectFacts {
            declared: declared(Some("svc-billing"), Some("Invoices.")),
            ..counted(&db, &root, 100)
        });

        f.record(&ProjectFacts {
            declared: declared(None, None),
            ..counted(&db, &root, 101)
        });

        let row = f.row(&db).unwrap();
        assert_eq!(row.description, None, "an emptied Set must clear it");
        assert_eq!(row.declared_name, None);
    }

    #[test]
    fn a_keep_write_leaves_the_declared_columns_untouched() {
        // `serve` startup and the watcher have no config in scope, so they must
        // never clear a name/description an index pass resolved.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&ProjectFacts {
            declared: declared(Some("svc-billing"), Some("Invoices.")),
            ..counted(&db, &root, 100)
        });
        // Change the DB so the fingerprint skip does not hide the merge.
        std::fs::write(&db, b"seed plus a change so the write is not skipped").unwrap();

        f.record(&ProjectFacts::identity_only(&db, &root));

        let row = f.row(&db).unwrap();
        assert_eq!(row.declared_name.as_deref(), Some("svc-billing"));
        assert_eq!(
            row.description.map(|d| d.text).as_deref(),
            Some("Invoices.")
        );
    }

    #[test]
    fn a_keep_write_on_a_fresh_insert_leaves_the_declared_columns_null() {
        let f = WriteFixture::new();
        let (root, db) = f.project("a");

        f.record(&ProjectFacts::identity_only(&db, &root));

        let row = f.row(&db).unwrap();
        assert_eq!(row.declared_name, None);
        assert_eq!(row.description, None);
    }

    #[test]
    fn a_changed_description_lands_even_when_the_database_is_unchanged() {
        // Editing README.md or .cartog.toml changes no byte of the graph DB, so
        // the fingerprint cannot see it. The skip path must still compare and
        // rewrite the declared columns, or a description could never be updated
        // without a re-index that happened to change the file.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&ProjectFacts {
            declared: declared(Some("old-name"), Some("Old summary.")),
            ..counted(&db, &root, 100)
        });

        // No last_indexed and identical DB bytes: the skip path.
        f.record(&ProjectFacts {
            declared: declared(Some("new-name"), Some("New summary.")),
            ..ProjectFacts::identity_only(&db, &root)
        });

        let row = f.row(&db).unwrap();
        assert_eq!(row.declared_name.as_deref(), Some("new-name"));
        assert_eq!(
            row.description.map(|d| d.text).as_deref(),
            Some("New summary.")
        );
    }

    #[test]
    fn an_identical_set_on_the_unchanged_path_writes_nothing_but_last_seen() {
        // The skip path must stay cheap: an unchanged description is not a
        // reason to rewrite anything.
        //
        // Observed through `total_changes` on a witness connection rather than
        // through the stored values, which are identical either way and so
        // cannot tell a skipped UPDATE from a redundant one.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&ProjectFacts {
            declared: declared(Some("svc"), Some("Same summary.")),
            ..counted(&db, &root, 100)
        });
        {
            let conn = crate::open::open_read_write(&f.registry).unwrap();
            conn.execute("UPDATE projects SET name = 'SENTINEL', last_seen = 1", [])
                .unwrap();
        }

        let changes = changed_rows_during(&f, || {
            f.record(&ProjectFacts {
                declared: declared(Some("svc"), Some("Same summary.")),
                ..ProjectFacts::identity_only(&db, &root)
            });
        });

        assert_eq!(
            changes, 1,
            "only the last_seen touch may write; the declared UPDATE must be skipped"
        );
        let row = f.row(&db).unwrap();
        assert_eq!(row.name, "SENTINEL", "an identical Set must not re-upsert");
        assert!(row.last_seen > 1, "but last_seen must still refresh");
    }

    #[test]
    fn a_differing_set_on_the_unchanged_path_does_write_the_declared_columns() {
        // The complement of the skip: the compare must not suppress a real
        // change, so the same counter shows two writes rather than one.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&ProjectFacts {
            declared: declared(Some("svc"), Some("Old summary.")),
            ..counted(&db, &root, 100)
        });

        let changes = changed_rows_during(&f, || {
            f.record(&ProjectFacts {
                declared: declared(Some("svc"), Some("New summary.")),
                ..ProjectFacts::identity_only(&db, &root)
            });
        });

        assert_eq!(changes, 2, "last_seen plus the declared UPDATE");
    }

    /// How many `UPDATE`s land on `projects` while `op` runs.
    ///
    /// Counted with a temporary audit trigger: `total_changes` is
    /// per-connection and `op` writes on its own, so no counter this test
    /// holds would see them. Distinguishing a skipped write from a redundant
    /// one is the whole point — the stored values are identical either way.
    fn changed_rows_during(f: &WriteFixture, op: impl FnOnce()) -> i64 {
        let conn = crate::open::open_read_write(&f.registry).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit (n INTEGER);
             DELETE FROM audit;
             CREATE TRIGGER IF NOT EXISTS audit_updates AFTER UPDATE ON projects
             BEGIN INSERT INTO audit (n) VALUES (1); END;",
        )
        .unwrap();
        drop(conn);

        op();

        let conn = crate::open::open_read_write(&f.registry).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))
            .unwrap();
        conn.execute_batch("DROP TRIGGER audit_updates").unwrap();
        n
    }

    #[test]
    fn a_description_over_the_cap_is_truncated_at_write_time() {
        // Defense in depth: config validation caps it too, but the registry
        // must not be able to grow unbounded from a caller that skipped it.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        let long = "word ".repeat(200);

        f.record(&ProjectFacts {
            declared: declared(None, Some(&long)),
            ..counted(&db, &root, 100)
        });

        let text = f.row(&db).unwrap().description.unwrap().text;
        assert!(
            text.chars().count() <= DESCRIPTION_MAX_CHARS,
            "{} chars stored",
            text.chars().count()
        );
    }

    #[test]
    fn a_description_over_the_cap_is_truncated_on_the_unchanged_path_too() {
        // Both write paths must enforce the cap, not just the upsert.
        let f = WriteFixture::new();
        let (root, db) = f.project("a");
        f.record(&counted(&db, &root, 100));

        f.record(&ProjectFacts {
            declared: declared(None, Some(&"word ".repeat(200))),
            ..ProjectFacts::identity_only(&db, &root)
        });

        let text = f.row(&db).unwrap().description.unwrap().text;
        assert!(text.chars().count() <= DESCRIPTION_MAX_CHARS);
    }

    #[test]
    fn a_language_tag_containing_a_colon_keeps_its_count() {
        // rsplit_once, not split_once: the count is the last field.
        assert_eq!(decode_languages("c:pp:5"), vec![("c:pp".to_string(), 5)]);
    }
}
