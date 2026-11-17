//! Who blocks upgrading an index's schema.
//!
//! Opening a DB migrates it. After a forced self-update, a `serve`/`watch`
//! started before the swap still runs the old binary and writes with the old
//! schema's SQL, so a newer binary must not migrate the file under it. Schema
//! versions come in as primitives: this crate does not depend on `cartog-db`.

use std::path::Path;

use cartog_process_lock::ActiveLock;

use crate::slot::slot_for_db;

/// The live `serve`/`watch` lock on `db_path` that blocks upgrading it from
/// `on_disk_schema` to `current_schema`, if any.
///
/// Such a holder runs an older cartog: a current one migrates the file when it
/// opens it. The calling process's own locks never block it (a `serve`
/// checks after taking its own lock).
#[must_use]
pub fn schema_upgrade_blocker(
    db_path: &Path,
    state_dir: &Path,
    on_disk_schema: u32,
    current_schema: u32,
) -> Option<ActiveLock> {
    if on_disk_schema >= current_schema {
        return None;
    }
    let slots = [slot_for_db("serve", db_path), slot_for_db("watch", db_path)];
    let own_pid = std::process::id();
    cartog_process_lock::find_active_locks(state_dir)
        .into_iter()
        .find(|lock| lock.pid != own_pid && slots.contains(&lock.slot))
}

/// The refusal shown when [`schema_upgrade_blocker`] finds a holder.
#[must_use]
pub fn schema_upgrade_refusal(
    db_path: &Path,
    on_disk_schema: u32,
    current_schema: u32,
    holder: &ActiveLock,
) -> String {
    format!(
        "index at {path} uses schema v{on_disk_schema}, older than this cartog's \
         v{current_schema}, and an older cartog still has it open ({slot}, PID {pid}). \
         Opening it here would upgrade the schema under that process. Restart or close it \
         (usually a Claude Code session, editor, or terminal `cartog watch` started before \
         the update), then retry.",
        path = db_path.display(),
        slot = holder.slot,
        pid = holder.pid,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live process that is not this one; killed on drop.
    #[cfg(unix)]
    struct Other(std::process::Child);

    #[cfg(unix)]
    impl Other {
        fn spawn() -> Self {
            Self(
                std::process::Command::new("sleep")
                    .arg("60")
                    .spawn()
                    .expect("spawn sleep"),
            )
        }
    }

    #[cfg(unix)]
    impl Drop for Other {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn hold(state_dir: &Path, prefix: &str, db_path: &Path, pid: u32) {
        let slot = slot_for_db(prefix, db_path);
        std::fs::write(state_dir.join(format!("{slot}.pid")), pid.to_string()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_older_schema_held_by_another_serve_is_blocked() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("db.sqlite");
        let other = Other::spawn();
        hold(dir.path(), "serve", &db, other.0.id());

        let blocker = schema_upgrade_blocker(&db, dir.path(), 7, 8);

        assert_eq!(blocker.map(|l| l.pid), Some(other.0.id()));
    }

    #[cfg(unix)]
    #[test]
    fn an_older_schema_held_by_another_watch_is_blocked() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("db.sqlite");
        let other = Other::spawn();
        hold(dir.path(), "watch", &db, other.0.id());

        assert!(schema_upgrade_blocker(&db, dir.path(), 7, 8).is_some());
    }

    #[test]
    fn the_calling_process_never_blocks_itself() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("db.sqlite");
        hold(dir.path(), "serve", &db, std::process::id());

        assert!(schema_upgrade_blocker(&db, dir.path(), 7, 8).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_current_schema_is_never_blocked() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("db.sqlite");
        let other = Other::spawn();
        hold(dir.path(), "serve", &db, other.0.id());

        assert!(schema_upgrade_blocker(&db, dir.path(), 8, 8).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn another_projects_lock_does_not_block() {
        let dir = tempfile::TempDir::new().unwrap();
        let other = Other::spawn();
        hold(
            dir.path(),
            "serve",
            &dir.path().join("other.sqlite"),
            other.0.id(),
        );

        assert!(schema_upgrade_blocker(&dir.path().join("db.sqlite"), dir.path(), 7, 8).is_none());
    }

    #[test]
    fn the_refusal_names_both_versions_and_the_holder() {
        let holder = ActiveLock {
            slot: "watch-ab".to_string(),
            pid: 42,
            start_time: None,
        };
        let msg = schema_upgrade_refusal(Path::new("/w/.cartog/db.sqlite"), 7, 8, &holder);
        assert!(
            msg.contains("schema v7, older than this cartog's v8"),
            "{msg}"
        );
        assert!(msg.contains("(watch-ab, PID 42)"), "{msg}");
    }
}
