//! Opening the registry connection with pragmas sized for a small,
//! many-writer metadata file.
//!
//! Deliberately does **not** reuse `cartog-db`'s connection setup: that is
//! private, graph-specific, and its 64 MiB `cache_size` / 256 MiB `mmap_size`
//! would have twenty concurrent cartog processes each reserving a quarter
//! gigabyte of address space for a file holding a few dozen rows.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OpenFlags};

/// Give up fast on a contended write. An order of magnitude below
/// `cartog-db`'s timeout because registration is best-effort bookkeeping
/// riding on an index pass: stalling that pass for seconds to record a row
/// is a worse outcome than skipping the row.
const BUSY_TIMEOUT_MS: u32 = 750;

/// Cap the `-wal` sidecar. SQLite's default is unlimited, which would leave
/// the high-water mark of the largest-ever transaction sitting in the user's
/// state directory forever.
const JOURNAL_SIZE_LIMIT: i64 = 1_048_576;

/// Open the registry for reading and writing, creating it if absent.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created or the file
/// cannot be opened. Pragma failures are tolerated (see [`apply_pragmas`]) —
/// a filesystem that refuses WAL yields a working connection, not an error.
pub(crate) fn open_read_write(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        // Best-effort: a failure here surfaces as the open error below, which
        // names the path. Creating it separately would report the same fault twice.
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    apply_pragmas(&conn, path);
    Ok(conn)
}

/// Open the registry read-only. Never creates the file.
///
/// # Errors
///
/// Returns an error if the file is absent or unreadable.
pub(crate) fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    // A reader sets only the busy timeout: journal_mode is a property of the
    // file (and unsettable read-only), and the size limit is the writer's job.
    let _ = conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS.into()));
    Ok(conn)
}

/// Apply the registry's pragmas, tolerating refusal of each.
///
/// WAL is requested but not required. `PRAGMA journal_mode` returns the mode
/// that is actually in effect rather than failing, so a WAL-hostile filesystem
/// (some network mounts) is detected by reading the returned value back — not
/// by catching an error. On refusal we fall back to `DELETE` and warn once.
///
/// An already-WAL file is never re-set and never downgraded: re-issuing the
/// pragma on a file other processes hold open in WAL is the one case where the
/// fallback could do damage.
fn apply_pragmas(conn: &Connection, path: &Path) {
    let _ = conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS.into()));
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");

    let current: Option<String> = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .ok();
    if current.as_deref() != Some("wal") {
        let resulting: Option<String> = conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
            .ok();
        match resulting.as_deref() {
            Some("wal") => {}
            // The pragma reported some other mode: the filesystem refused WAL.
            // DELETE works everywhere; the registry is small enough that
            // losing WAL's concurrency costs only a little more contention.
            _ => {
                let _ = conn.pragma_update(None, "journal_mode", "DELETE");
                warn_wal_fallback_once(path, resulting.as_deref());
            }
        }
    }

    let _ = conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT);
}

/// Warn once per process that WAL was refused.
///
/// Once per process, not once per path: the registry has a single path in any
/// real run, and a per-path set would keep an unbounded map alive in a
/// long-lived `serve` for a diagnostic nobody needs twice.
fn warn_wal_fallback_once(path: &Path, got: Option<&str>) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::warn!(
        path = %path.display(),
        journal_mode = got.unwrap_or("unknown"),
        "project registry could not use WAL; falling back to DELETE journaling \
         (concurrent cartog processes may contend more on registry writes)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(conn: &Connection) -> String {
        conn.pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_fresh_registry_opens_in_wal_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        let conn = open_read_write(&dir.path().join("projects.sqlite")).unwrap();
        assert_eq!(mode(&conn), "wal");
    }

    #[test]
    fn the_journal_size_limit_is_capped() {
        let dir = tempfile::TempDir::new().unwrap();
        let conn = open_read_write(&dir.path().join("projects.sqlite")).unwrap();
        let limit: i64 = conn
            .pragma_query_value(None, "journal_size_limit", |row| row.get(0))
            .unwrap();
        assert_eq!(limit, JOURNAL_SIZE_LIMIT);
    }

    #[test]
    fn a_contended_write_gives_up_quickly_rather_than_stalling() {
        // The intent behind the short timeout: a registry write rides on an
        // index pass, so losing a race must cost that pass a moment, not
        // seconds. Measured against the connection's real busy behaviour
        // rather than asserted on the constant (which clippy rightly rejects
        // as a compile-time truth).
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");
        let holder = open_read_write(&path).unwrap();
        holder.execute_batch("CREATE TABLE t(x)").unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE").unwrap();

        let contender = open_read_write(&path).unwrap();
        let start = std::time::Instant::now();
        let result = contender.execute_batch("INSERT INTO t VALUES (1)");
        let waited = start.elapsed();

        assert!(
            result.is_err(),
            "the contended write must fail, not block forever"
        );
        // Generous upper bound: the point is "gave up in well under a graph
        // DB's multi-second timeout", not a precise duration.
        assert!(
            waited < std::time::Duration::from_secs(3),
            "gave up after {waited:?}; a contended registry write must not stall an index pass"
        );
        holder.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn an_existing_wal_registry_is_never_downgraded() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");
        {
            let conn = open_read_write(&path).unwrap();
            assert_eq!(mode(&conn), "wal");
        }
        // Re-open: the mode must survive, and the fallback must not fire.
        let conn = open_read_write(&path).unwrap();
        assert_eq!(mode(&conn), "wal");
    }

    #[test]
    fn parent_directories_are_created_on_demand() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir
            .path()
            .join("deep")
            .join("nested")
            .join("projects.sqlite");
        let _conn = open_read_write(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_read_only_open_of_an_absent_file_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(open_read_only(&dir.path().join("nope.sqlite")).is_err());
    }

    #[test]
    fn a_read_only_connection_cannot_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("projects.sqlite");
        {
            let conn = open_read_write(&path).unwrap();
            conn.execute_batch("CREATE TABLE t(x)").unwrap();
        }
        let ro = open_read_only(&path).unwrap();
        assert!(ro.execute_batch("INSERT INTO t VALUES(1)").is_err());
    }
}
