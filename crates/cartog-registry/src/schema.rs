//! The registry's SQLite schema and its version gate.

use rusqlite::Connection;

/// Registry schema version, tracked independently of the graph DB's
/// `CURRENT_SCHEMA_VERSION`.
///
/// The two must never be coupled: a registry bump must not force a re-index,
/// and a graph-schema bump must not invalidate the registry.
///
/// - v1 — the original registry.
/// - v2 — `declared_name`, `description`, `description_src` (step 3). Added by
///   [`migrate`] as additive `ALTER TABLE`s, so a v1 file keeps every row.
pub(crate) const REGISTRY_VERSION: u32 = 2;

pub(crate) const REGISTRY_VERSION_KEY: &str = "registry_version";

/// One row per indexed project, keyed on the project's serve slot.
///
/// Every count is nullable because the registry caches whatever the writing
/// trigger happened to know: an absent count means "not known", never zero.
/// `name` and `last_seen` are the exceptions — every writer has both.
///
/// `name` always means the project root's basename. A declared `[project]
/// name` lives in the nullable `declared_name`, so a writer with no config in
/// scope can refresh the row without having to invent one.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS projects (
    id                 TEXT PRIMARY KEY,
    db_path            TEXT NOT NULL,
    root               TEXT NOT NULL,
    name               TEXT NOT NULL,
    declared_name      TEXT,
    description        TEXT,
    description_src    TEXT,
    languages          TEXT,
    schema_version     INTEGER,
    file_count         INTEGER,
    symbol_count       INTEGER,
    edge_count         INTEGER,
    resolved_count     INTEGER,
    embedding_count    INTEGER,
    embed_provider     TEXT,
    embed_model        TEXT,
    embed_dim          INTEGER,
    source_fingerprint TEXT,
    last_indexed       INTEGER,
    last_seen          INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS metadata (
    key   TEXT PRIMARY KEY,
    value TEXT
);
";

/// Columns added by the v1 → v2 migration, as `(name, DDL type)`.
///
/// A fresh registry gets them from [`SCHEMA`]; an existing v1 file gets them
/// here. Both paths must produce the same shape, which the
/// `a_fresh_registry_and_a_migrated_one_have_the_same_columns` test pins.
const V2_COLUMNS: [(&str, &str); 3] = [
    ("declared_name", "TEXT"),
    ("description", "TEXT"),
    ("description_src", "TEXT"),
];

/// Create the schema if absent, migrate an older one, and seed the version.
///
/// No index beyond the primary key: the registry holds tens of rows scanned in
/// full by `projects list`, and every extra index is another page a burst of
/// concurrent writers has to touch.
///
/// # Errors
///
/// Returns an error if the DDL cannot be applied.
pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    migrate(conn)?;
    conn.execute(
        "INSERT OR IGNORE INTO metadata (key, value) VALUES (?1, ?2)",
        rusqlite::params![REGISTRY_VERSION_KEY, REGISTRY_VERSION.to_string()],
    )?;
    Ok(())
}

/// Bring an older registry up to [`REGISTRY_VERSION`], in place.
///
/// Purely additive: three nullable columns, so every existing row survives and
/// reads back with no declared identity. A *newer* registry is left untouched —
/// this binary does not know its shape, and `write::try_record` is what
/// declines to write it.
///
/// The version row is bumped last, so a failed `ALTER` leaves the file
/// claiming v1 and the next open retries rather than skipping the migration.
///
/// Driven by **column presence**, not by the version row: the row is a hint a
/// v1 file may have lost or had corrupted, and gating on it left such a file
/// column-less while [`ensure_schema`]'s `INSERT OR IGNORE` seeded '2' — after
/// which every upsert failed on `no such column: declared_name`, permanently.
/// The version row is still what stops this binary touching a *newer* file,
/// whose shape it does not know.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    if read_registry_version(conn).is_some_and(|found| found > REGISTRY_VERSION) {
        return Ok(());
    }

    let existing = column_names(conn, "projects")?;
    for (name, ty) in V2_COLUMNS {
        // Idempotent: a half-applied migration (the version bump lost a race)
        // must not fail on a column that is already there.
        if existing.iter().any(|c| c == name) {
            continue;
        }
        conn.execute_batch(&format!("ALTER TABLE projects ADD COLUMN {name} {ty}"))?;
    }

    conn.execute(
        "UPDATE metadata SET value = ?2 WHERE key = ?1",
        rusqlite::params![REGISTRY_VERSION_KEY, REGISTRY_VERSION.to_string()],
    )?;
    Ok(())
}

/// Column names of `table`, via `PRAGMA table_info`.
///
/// `table` is interpolated because a pragma argument cannot be bound; every
/// caller passes an in-crate literal, never untrusted input.
pub(crate) fn column_names(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

/// True when `projects` carries the v2 declared-identity columns.
///
/// A reader opens the registry read-only and must never migrate it (that is a
/// writer's job), so between the upgrade and the first write the file is still
/// v1 and a v2 `SELECT` would fail on `declared_name`. Readers branch on this.
pub(crate) fn has_declared_columns(conn: &Connection) -> bool {
    column_names(conn, "projects")
        .map(|cols| cols.iter().any(|c| c == "declared_name"))
        .unwrap_or(false)
}

/// Read the registry version, or `None` when the row is absent or unparseable.
pub(crate) fn read_registry_version(conn: &Connection) -> Option<u32> {
    conn.query_row(
        "SELECT value FROM metadata WHERE key = ?1",
        rusqlite::params![REGISTRY_VERSION_KEY],
        |row| row.get::<_, String>(0),
    )
    .ok()?
    .parse()
    .ok()
}

/// The v1 `projects` DDL verbatim (with one row), so migration and read-path
/// tests run against the shape that actually shipped rather than today's minus
/// a column.
#[cfg(test)]
pub(crate) const V1_SCHEMA: &str = "
CREATE TABLE projects (
    id                 TEXT PRIMARY KEY,
    db_path            TEXT NOT NULL,
    root               TEXT NOT NULL,
    name               TEXT NOT NULL,
    languages          TEXT,
    schema_version     INTEGER,
    file_count         INTEGER,
    symbol_count       INTEGER,
    edge_count         INTEGER,
    resolved_count     INTEGER,
    embedding_count    INTEGER,
    embed_provider     TEXT,
    embed_model        TEXT,
    embed_dim          INTEGER,
    source_fingerprint TEXT,
    last_indexed       INTEGER,
    last_seen          INTEGER NOT NULL
);
CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
INSERT INTO metadata (key, value) VALUES ('registry_version', '1');
INSERT INTO projects (id, db_path, root, name, symbol_count, last_seen)
VALUES ('serve-v1row', '/w/svc/.cartog/db.sqlite', '/w/svc', 'svc', 8134, 42);
";

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn the_registry_version_is_seeded_on_first_use() {
        assert_eq!(read_registry_version(&fresh()), Some(REGISTRY_VERSION));
    }

    #[test]
    fn ensure_schema_is_idempotent_and_preserves_rows() {
        let conn = fresh();
        conn.execute(
            "INSERT INTO projects (id, db_path, root, name, last_seen) VALUES ('a','/d','/r','r',1)",
            [],
        )
        .unwrap();

        ensure_schema(&conn).unwrap();

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "re-running the DDL must not drop rows");
    }

    #[test]
    fn a_newer_registry_version_is_read_back_verbatim() {
        // An older cartog binary meeting a newer registry must be able to see
        // that it is newer, so it can decline to write rather than corrupt it.
        let conn = fresh();
        conn.execute(
            "UPDATE metadata SET value = '99' WHERE key = ?1",
            rusqlite::params![REGISTRY_VERSION_KEY],
        )
        .unwrap();
        assert_eq!(read_registry_version(&conn), Some(99));
    }

    #[test]
    fn an_unparseable_registry_version_reads_as_none() {
        let conn = fresh();
        conn.execute(
            "UPDATE metadata SET value = 'not-a-number' WHERE key = ?1",
            rusqlite::params![REGISTRY_VERSION_KEY],
        )
        .unwrap();
        assert_eq!(read_registry_version(&conn), None);
    }

    fn v1() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn
    }

    #[test]
    fn a_v1_registry_migrates_to_v2_and_keeps_its_rows() {
        let conn = v1();

        ensure_schema(&conn).unwrap();

        assert_eq!(read_registry_version(&conn), Some(2));
        let (name, symbols): (String, i64) = conn
            .query_row(
                "SELECT name, symbol_count FROM projects WHERE id = 'serve-v1row'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "svc", "the migration must be additive");
        assert_eq!(symbols, 8134);
    }

    #[test]
    fn a_migrated_v1_row_reads_back_with_no_declared_identity() {
        let conn = v1();

        ensure_schema(&conn).unwrap();

        let cols: (Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT declared_name, description, description_src FROM projects",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(cols, (None, None, None));
    }

    #[test]
    fn a_fresh_registry_and_a_migrated_one_have_the_same_columns() {
        // The two paths that produce the v2 shape must not drift. Compared as
        // sets: `ALTER TABLE ADD COLUMN` appends, so ordinal position
        // necessarily differs — every statement in this crate binds by name.
        let migrated = v1();
        ensure_schema(&migrated).unwrap();

        let mut fresh_cols = column_names(&fresh(), "projects").unwrap();
        let mut migrated_cols = column_names(&migrated, "projects").unwrap();
        fresh_cols.sort();
        migrated_cols.sort();
        assert_eq!(fresh_cols, migrated_cols);
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let conn = v1();
        ensure_schema(&conn).unwrap();

        ensure_schema(&conn).unwrap();

        assert_eq!(read_registry_version(&conn), Some(2));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_newer_registry_is_left_at_its_own_version() {
        // This binary does not know a v3 shape, so it must not touch it; the
        // write path is what declines to write it.
        let conn = fresh();
        conn.execute(
            "UPDATE metadata SET value = '3' WHERE key = ?1",
            rusqlite::params![REGISTRY_VERSION_KEY],
        )
        .unwrap();

        ensure_schema(&conn).unwrap();

        assert_eq!(read_registry_version(&conn), Some(3));
    }

    /// A v1-shaped registry whose version row says `value`, or has none at all.
    fn v1_with_version(value: Option<&str>) -> Connection {
        let conn = v1();
        match value {
            Some(v) => conn
                .execute(
                    "UPDATE metadata SET value = ?2 WHERE key = ?1",
                    rusqlite::params![REGISTRY_VERSION_KEY, v],
                )
                .map(|_| ()),
            None => conn
                .execute(
                    "DELETE FROM metadata WHERE key = ?1",
                    rusqlite::params![REGISTRY_VERSION_KEY],
                )
                .map(|_| ()),
        }
        .unwrap();
        conn
    }

    /// Every v2 column is present and a v2-shaped upsert succeeds.
    fn assert_migrated_to_v2(conn: &Connection) {
        let cols = column_names(conn, "projects").unwrap();
        for (name, _) in V2_COLUMNS {
            assert!(cols.iter().any(|c| c == name), "missing column {name}");
        }
        assert_eq!(read_registry_version(conn), Some(REGISTRY_VERSION));
        conn.execute(
            "INSERT INTO projects (id, db_path, root, name, declared_name, description, \
             description_src, last_seen) VALUES ('u','/d','/r','r','dn','desc','config',9)",
            [],
        )
        .expect("a v2-shaped upsert must succeed after the migration");
    }

    #[test]
    fn a_v1_table_with_no_version_row_still_gains_the_v2_columns() {
        // The version row is a *hint*, not the schema: a v1 file whose metadata
        // was lost would otherwise be seeded with '2' while still missing the
        // columns, and every later upsert would fail on `declared_name` forever.
        let conn = v1_with_version(None);

        ensure_schema(&conn).unwrap();

        assert_migrated_to_v2(&conn);
    }

    #[test]
    fn a_v1_table_with_an_unparseable_version_still_gains_the_v2_columns() {
        let conn = v1_with_version(Some("garbage"));

        ensure_schema(&conn).unwrap();

        assert_migrated_to_v2(&conn);
    }

    #[test]
    fn counts_are_nullable_but_name_and_last_seen_are_not() {
        let conn = fresh();
        // A bare row (no counts) is valid: "not known" is a real state.
        conn.execute(
            "INSERT INTO projects (id, db_path, root, name, last_seen) VALUES ('a','/d','/r','r',1)",
            [],
        )
        .unwrap();
        // A row with no name is not.
        assert!(conn
            .execute(
                "INSERT INTO projects (id, db_path, root, last_seen) VALUES ('b','/d','/r',1)",
                [],
            )
            .is_err());
    }
}
