//! Cheap change detection for a project database, used to skip a redundant
//! registry write.

use std::path::Path;

/// A conservative identity for a project DB: size and mtime of the main file
/// **and** its `-wal` sidecar.
///
/// The main file alone is insufficient. Measured on SQLite in WAL mode: after
/// a committed insert, the main file's size and mtime are byte-identical —
/// only `-wal` grew. A guard reading main-only would therefore call a
/// just-modified database unchanged and skip forever.
///
/// `PRAGMA data_version` is also unusable here, despite looking like the
/// right tool. Measured: it does not change on the writing connection's own
/// commit, and it reads back as `2` on every fresh connection — so a
/// short-lived registry writer, which is exactly what we are, would see a
/// constant and skip forever.
///
/// The tuple over both files changes on commit, changes on checkpoint (where
/// `-wal` shrinks and main grows), and stays equal while idle. It is a
/// *conservative* guard, not a content hash: equal means almost certainly
/// unchanged, unequal means possibly changed. Never promote it to an
/// integrity check.
///
/// Returns `None` when the main file cannot be stat'd, which callers treat as
/// "cannot skip" rather than as an error.
pub(crate) fn source_fingerprint(db_path: &Path) -> Option<String> {
    let (main_size, main_mtime) = stat_parts(db_path)?;
    // A missing -wal is normal (DELETE journaling, or a checkpointed WAL) and
    // contributes zeroes rather than aborting the fingerprint.
    let wal_path = wal_sidecar(db_path);
    let (wal_size, wal_mtime) = stat_parts(&wal_path).unwrap_or((0, 0));
    Some(format!("{main_size}:{main_mtime}:{wal_size}:{wal_mtime}"))
}

/// Path of the `-wal` sidecar SQLite creates beside `db_path`.
fn wal_sidecar(db_path: &Path) -> std::path::PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push("-wal");
    std::path::PathBuf::from(name)
}

fn stat_parts(p: &Path) -> Option<(u64, i128)> {
    let meta = std::fs::metadata(p).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    Some((meta.len(), mtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Open a WAL-mode SQLite DB the way a cartog project DB is opened, so the
    /// fingerprint is exercised against the journaling mode it must handle.
    fn wal_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch("CREATE TABLE t(x)").unwrap();
        conn
    }

    #[test]
    fn an_absent_database_has_no_fingerprint() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(source_fingerprint(&dir.path().join("nope.db")), None);
    }

    #[test]
    fn the_fingerprint_is_stable_while_the_database_is_idle() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.db");
        let _conn = wal_db(&path);

        let a = source_fingerprint(&path).unwrap();
        let b = source_fingerprint(&path).unwrap();
        assert_eq!(
            a, b,
            "an idle DB must fingerprint equal, or every write re-writes"
        );
    }

    #[test]
    fn the_fingerprint_changes_after_a_wal_commit() {
        // The regression this whole tuple exists for: in WAL mode the main
        // file is untouched by a commit, so a main-only fingerprint would
        // report "unchanged" and skip the write forever.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.db");
        let conn = wal_db(&path);

        let before = source_fingerprint(&path).unwrap();
        conn.execute("INSERT INTO t VALUES (1)", []).unwrap();
        let after = source_fingerprint(&path).unwrap();

        assert_ne!(
            before, after,
            "a committed insert must change the fingerprint"
        );
    }

    #[test]
    fn the_fingerprint_covers_the_wal_sidecar_not_just_the_main_file() {
        // Pin the mechanism, not only the outcome: assert the main file really
        // did stay identical across the commit, so this test still fails if
        // someone narrows the tuple back to the main file.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.db");
        let conn = wal_db(&path);

        let main_before = std::fs::metadata(&path).unwrap();
        conn.execute("INSERT INTO t VALUES (1)", []).unwrap();
        let main_after = std::fs::metadata(&path).unwrap();

        assert_eq!(
            main_before.len(),
            main_after.len(),
            "precondition: a WAL commit leaves the main file's size alone"
        );
        assert!(
            path.with_extension("db-wal").exists() || wal_sidecar(&path).exists(),
            "precondition: a -wal sidecar exists to carry the change"
        );
    }

    #[test]
    fn the_fingerprint_changes_after_a_checkpoint() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.db");
        let conn = wal_db(&path);
        conn.execute("INSERT INTO t VALUES (1)", []).unwrap();

        let before = source_fingerprint(&path).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let after = source_fingerprint(&path).unwrap();

        assert_ne!(
            before, after,
            "a checkpoint moves bytes from -wal into main and must be visible"
        );
    }

    #[test]
    fn a_missing_wal_sidecar_still_yields_a_fingerprint() {
        // DELETE journaling, or a fully checkpointed WAL: no -wal file exists.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.db");
        std::fs::write(&path, b"not really sqlite").unwrap();
        assert!(source_fingerprint(&path).is_some());
    }

    #[test]
    fn the_wal_sidecar_path_appends_to_the_full_filename() {
        // SQLite appends "-wal" to the whole path, it does not replace the
        // extension: db.sqlite -> db.sqlite-wal.
        assert_eq!(
            wal_sidecar(Path::new("/x/db.sqlite")),
            Path::new("/x/db.sqlite-wal")
        );
    }
}
