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
    // A path whose parent component is a regular file (not a directory) can
    // never host a database on any platform. We accept either PrepareDir
    // (mkdir on the parent failed) or Open (SQLite refused).
    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    let bad_path = file.join("db.sqlite");
    let err = Database::open(&bad_path, DEFAULT_EMBEDDING_DIM).unwrap_err();
    match err {
        DbError::Open { path, .. } => assert_eq!(path, bad_path),
        DbError::PrepareDir { path, .. } => {
            assert_eq!(path, bad_path.parent().unwrap());
        }
        other => panic!("expected DbError::Open or PrepareDir, got {other:?}"),
    }
}
