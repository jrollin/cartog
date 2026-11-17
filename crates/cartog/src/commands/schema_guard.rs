//! Refuse to upgrade an index's schema under a running older cartog.
//!
//! The decision lives in `cartog_registry::schema_upgrade_blocker`, shared with
//! `cartog serve`; this wrapper reads the on-disk version for it.

use std::path::Path;

use anyhow::Result;

/// Refuse when the index at `db_path` is on an older schema than this binary
/// and another `serve`/`watch` for it still holds its lock in `state_dir`.
pub(crate) fn refuse_schema_upgrade_under_live_peer(
    db_path: &Path,
    state_dir: Option<&Path>,
) -> Result<()> {
    cartog_registry::refuse_schema_upgrade(
        db_path,
        state_dir,
        |p| cartog_db::read_schema_version_at(p).ok(),
        cartog_db::CURRENT_SCHEMA_VERSION,
    )
    .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An on-disk index stamped with `version`, plus an empty state dir.
    fn index_at_schema(version: u32) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("db.sqlite");
        drop(cartog_db::Database::open(&db_path, cartog_db::DEFAULT_EMBEDDING_DIM).unwrap());
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'schema_version'",
            [version.to_string()],
        )
        .unwrap();
        std::fs::create_dir(tmp.path().join("state")).unwrap();
        (tmp, db_path)
    }

    /// A live process other than this one, holding the serve lock; killed on drop.
    #[cfg(unix)]
    struct Holder(std::process::Child);

    #[cfg(unix)]
    impl Holder {
        fn serve(state_dir: &Path, db_path: &Path) -> Self {
            let child = std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("spawn sleep");
            let slot = crate::state::slot_for_db("serve", db_path);
            std::fs::write(
                state_dir.join(format!("{slot}.pid")),
                child.id().to_string(),
            )
            .unwrap();
            Self(child)
        }
    }

    #[cfg(unix)]
    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_older_on_disk_schema_held_by_another_serve_is_refused() {
        let (tmp, db_path) = index_at_schema(cartog_db::CURRENT_SCHEMA_VERSION - 1);
        let state_dir = tmp.path().join("state");
        let _holder = Holder::serve(&state_dir, &db_path);

        let err = refuse_schema_upgrade_under_live_peer(&db_path, Some(&state_dir))
            .expect_err("migrating under a live older serve must be refused");

        assert!(
            err.to_string().contains("older cartog still has it open"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_current_on_disk_schema_is_opened_even_with_a_serve_running() {
        let (tmp, db_path) = index_at_schema(cartog_db::CURRENT_SCHEMA_VERSION);
        let state_dir = tmp.path().join("state");
        let _holder = Holder::serve(&state_dir, &db_path);

        assert!(refuse_schema_upgrade_under_live_peer(&db_path, Some(&state_dir)).is_ok());
    }

    #[test]
    fn an_older_on_disk_schema_with_no_running_process_may_be_upgraded() {
        let (tmp, db_path) = index_at_schema(cartog_db::CURRENT_SCHEMA_VERSION - 1);

        let result =
            refuse_schema_upgrade_under_live_peer(&db_path, Some(&tmp.path().join("state")));

        assert!(result.is_ok());
    }
}
