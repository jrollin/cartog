//! Quarantining a corrupt registry file.
//!
//! The distinction this module exists to enforce: **corruption is renamed
//! aside, contention is not.** A `SQLITE_BUSY` from twenty concurrent index
//! passes is normal operation; quarantining on it would have twenty processes
//! racing to rename the file away from each other, turning a best-effort cache
//! into a data-loss bug. Only "this is not a database" gets quarantined.

use std::path::{Path, PathBuf};

use crate::open::open_read_only;

/// Rename a corrupt registry aside so a fresh one can take its place.
///
/// A no-op unless the file exists *and* fails to open or read as SQLite. In
/// particular this does nothing for an absent file, an unreadable-by-permission
/// file, or a busy one — none of those is corruption.
///
/// The corrupt file is preserved, never truncated: it still holds every row
/// the user accumulated, and its bytes are the only evidence of what went
/// wrong. The unix-timestamp suffix means a second corruption cannot clobber
/// the first quarantine.
pub(crate) fn quarantine_if_corrupt(path: &Path) {
    if !path.exists() {
        return;
    }
    match classify(path) {
        Health::Usable | Health::Inconclusive => {}
        Health::Corrupt(reason) => {
            let Some(moved) = rename_aside(path) else {
                tracing::warn!(
                    path = %path.display(),
                    reason = %reason,
                    "project registry looks corrupt but could not be renamed aside; \
                     continuing without a registry"
                );
                return;
            };
            // stderr as well as tracing: the user needs to know a file of
            // theirs was moved, and most cartog commands run with no
            // subscriber attached to show a warn.
            eprintln!(
                "cartog: the project registry at {} was unreadable ({reason}); \
                 moved it to {} and started a fresh one",
                path.display(),
                moved.display()
            );
            tracing::warn!(
                from = %path.display(),
                to = %moved.display(),
                reason = %reason,
                "quarantined a corrupt project registry"
            );
        }
    }
}

enum Health {
    Usable,
    /// Corrupt beyond use, with a human-readable reason.
    Corrupt(String),
    /// Could not tell — busy, permission-denied, or any other transient
    /// fault. Never a reason to touch the file.
    Inconclusive,
}

fn classify(path: &Path) -> Health {
    let conn = match open_read_only(path) {
        Ok(c) => c,
        Err(e) => return classify_error(&e),
    };
    // Read the schema catalogue: cheap, and it is the first thing that fails
    // on a file whose header is not SQLite. Deliberately not
    // `PRAGMA integrity_check` — a full verification on every write would put
    // a whole-file scan on the index path.
    match conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(_) => Health::Usable,
        Err(e) => classify_error(&e),
    }
}

/// Map a rusqlite error to a health verdict.
///
/// Only `NotADatabase` and `Corrupt` are treated as corruption. `Busy` and
/// `Locked` are explicitly inconclusive — see the module docs for why that
/// distinction is the important one here.
fn classify_error(e: &rusqlite::Error) -> Health {
    use rusqlite::ffi::ErrorCode;
    if let rusqlite::Error::SqliteFailure(err, _) = e {
        return match err.code {
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => Health::Corrupt(format!("{e}")),
            // Contention and permissions are transient states of a
            // perfectly good file.
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Health::Inconclusive,
            _ => Health::Inconclusive,
        };
    }
    Health::Inconclusive
}

/// Rename `path` to `<path>.corrupt.<unix-ts>`, returning the new path.
fn rename_aside(path: &Path) -> Option<PathBuf> {
    let ts = crate::write::now_unix();
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".corrupt.{ts}"));
    let target = PathBuf::from(name);
    // An existing target means a second corruption within the same second.
    // Don't overwrite the earlier evidence.
    if target.exists() {
        return None;
    }
    // Remove the sidecars BEFORE the rename, not after. After the rename the
    // original path is free, so a concurrent cartog process — and this module
    // exists precisely because there are many — can create a fresh registry
    // there with its own `-wal`. Deleting sidecars at that point would discard
    // that healthy file's committed-but-uncheckpointed rows. Cleaning up
    // first means the only `-wal` we can touch is the corrupt file's own,
    // which must not outlive it: a stale WAL would otherwise be recovered
    // against whatever new main file appears.
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_os_string();
        side.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(side));
    }
    std::fs::rename(path, &target).ok()?;
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corrupt_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                p.to_string_lossy().contains(".corrupt.").then_some(p)
            })
            .collect()
    }

    #[test]
    fn a_corrupt_registry_is_renamed_aside_with_its_bytes_intact() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");
        let garbage = b"this is definitely not a sqlite database";
        std::fs::write(&path, garbage).unwrap();

        quarantine_if_corrupt(&path);

        let moved = corrupt_files(dir.path());
        assert_eq!(moved.len(), 1, "the corrupt file must be preserved");
        assert_eq!(
            std::fs::read(&moved[0]).unwrap(),
            garbage,
            "quarantine must never truncate — the bytes are the evidence"
        );
        assert!(
            !path.exists(),
            "the original path must be freed for a fresh registry"
        );
    }

    #[test]
    fn an_absent_registry_is_not_quarantined() {
        let dir = tempfile::TempDir::new().unwrap();
        quarantine_if_corrupt(&dir.path().join("projects.sqlite"));
        assert!(corrupt_files(dir.path()).is_empty());
    }

    #[test]
    fn a_healthy_registry_is_left_alone() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");
        {
            let conn = crate::open::open_read_write(&path).unwrap();
            crate::schema::ensure_schema(&conn).unwrap();
        }

        quarantine_if_corrupt(&path);

        assert!(corrupt_files(dir.path()).is_empty());
        assert!(path.exists());
    }

    #[test]
    fn a_busy_registry_is_never_quarantined() {
        // The dangerous mistake this guards: under a burst of concurrent
        // writers, quarantine-on-BUSY would have each process rename the file
        // out from under the others.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");
        let holder = crate::open::open_read_write(&path).unwrap();
        crate::schema::ensure_schema(&holder).unwrap();
        // Hold an exclusive write lock for the duration of the check.
        holder.execute_batch("BEGIN EXCLUSIVE").unwrap();

        quarantine_if_corrupt(&path);

        assert!(
            corrupt_files(dir.path()).is_empty(),
            "contention is not corruption"
        );
        assert!(path.exists());
        holder.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn busy_and_locked_errors_classify_as_inconclusive() {
        // Direct test of the mapping, since provoking a real BUSY through the
        // whole path is timing-dependent.
        for code in [
            rusqlite::ffi::ErrorCode::DatabaseBusy,
            rusqlite::ffi::ErrorCode::DatabaseLocked,
        ] {
            let e = rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code,
                    extended_code: 0,
                },
                Some("locked".to_string()),
            );
            assert!(
                matches!(classify_error(&e), Health::Inconclusive),
                "{code:?} must never be treated as corruption"
            );
        }
    }

    #[test]
    fn not_a_database_classifies_as_corrupt() {
        let e = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::NotADatabase,
                extended_code: 0,
            },
            Some("file is not a database".to_string()),
        );
        assert!(matches!(classify_error(&e), Health::Corrupt(_)));
    }

    #[test]
    fn a_second_corruption_does_not_clobber_the_first_quarantine() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");

        std::fs::write(&path, b"first corruption").unwrap();
        quarantine_if_corrupt(&path);
        std::fs::write(&path, b"second corruption").unwrap();
        quarantine_if_corrupt(&path);

        let moved = corrupt_files(dir.path());
        // Either two distinct quarantines, or the second was declined to
        // protect the first — never the first being overwritten.
        let preserved = moved
            .iter()
            .any(|p| std::fs::read(p).unwrap() == b"first corruption");
        assert!(preserved, "the first quarantine's bytes must survive");
    }

    #[test]
    fn quarantine_removes_the_sidecars_of_the_corrupt_file() {
        // A leftover -wal would let SQLite try to recover a journal belonging
        // to the quarantined file against the fresh one.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");
        std::fs::write(&path, b"not a database").unwrap();
        let wal = dir.path().join("projects.sqlite-wal");
        std::fs::write(&wal, b"stale wal").unwrap();

        quarantine_if_corrupt(&path);

        assert!(!wal.exists(), "the corrupt file's -wal must not outlive it");
    }
}
