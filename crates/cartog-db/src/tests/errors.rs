use crate::*;

// ── Typed error surface ──

#[test]
fn test_db_error_wraps_into_anyhow() {
    // Callers that keep using anyhow::Result must still compose with DbError
    // transparently via `?`, thanks to the std::error::Error blanket impl.
    fn downstream() -> anyhow::Result<()> {
        let _db = Database::open_memory()?; // returns DbResult<Database>
        Ok(())
    }
    downstream().unwrap();
}

#[test]
fn test_db_error_open_variant_has_path() {
    // Give Database::open a path inside a non-writable location to force
    // a failure. We accept either PrepareDir (mkdir failed on the parent)
    // or Open (SQLite refused), since the failure point depends on the
    // platform's handling of `/dev/null/…`.
    let bad_path = std::path::PathBuf::from("/dev/null/definitely/not/a/db.sqlite");
    let err = Database::open(&bad_path, DEFAULT_EMBEDDING_DIM).unwrap_err();
    match err {
        DbError::Open { path, .. } => assert_eq!(path, bad_path),
        DbError::PrepareDir { path, .. } => {
            assert_eq!(path, bad_path.parent().unwrap());
        }
        other => panic!("expected DbError::Open or PrepareDir, got {other:?}"),
    }
}
