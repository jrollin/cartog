//! The registry's SQLite schema and its version gate.

use rusqlite::Connection;

/// Registry schema version, tracked independently of the graph DB's
/// `CURRENT_SCHEMA_VERSION`.
///
/// The two must never be coupled: a registry bump must not force a re-index,
/// and a graph-schema bump must not invalidate the registry. Step-3 columns
/// (`description`, `description_src`) are added under this gate rather than
/// shipped now as dead nullable columns.
pub(crate) const REGISTRY_VERSION: u32 = 1;

pub(crate) const REGISTRY_VERSION_KEY: &str = "registry_version";

/// One row per indexed project, keyed on the project's serve slot.
///
/// Every count is nullable because the registry caches whatever the writing
/// trigger happened to know: an absent count means "not known", never zero.
/// `name` and `last_seen` are the exceptions — every writer has both.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS projects (
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

CREATE TABLE IF NOT EXISTS metadata (
    key   TEXT PRIMARY KEY,
    value TEXT
);
";

/// Create the schema if absent and seed the registry version.
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
    conn.execute(
        "INSERT OR IGNORE INTO metadata (key, value) VALUES (?1, ?2)",
        rusqlite::params![REGISTRY_VERSION_KEY, REGISTRY_VERSION.to_string()],
    )?;
    Ok(())
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
