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

// ── No-create open path (consent gate) ──

#[test]
fn open_existing_errors_without_creating_dir_when_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join(".cartog").join("db.sqlite");

    let err = Database::open_existing(&db_path, DEFAULT_EMBEDDING_DIM).unwrap_err();
    match err {
        DbError::NotFound { path } => assert_eq!(path, db_path),
        other => panic!("expected DbError::NotFound, got {other:?}"),
    }
    assert!(
        !db_path.parent().unwrap().exists(),
        "open_existing must NOT create the .cartog/ parent for an absent DB"
    );
}

#[test]
fn open_existing_opens_an_existing_db() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join(".cartog").join("db.sqlite");

    // open() creates it; open_existing() then succeeds against the same file.
    Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
    let db = Database::open_existing(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
    assert!(db.is_empty().unwrap());
}

#[test]
fn open_existing_does_not_create_db_when_parent_dir_exists() {
    // The TOCTOU case: parent dir present, main file absent. A non-creating open
    // must error rather than materialize a fresh DB at db_path.
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("db.sqlite"); // parent (tmp) already exists

    let err = Database::open_existing(&db_path, DEFAULT_EMBEDDING_DIM).unwrap_err();
    assert!(matches!(err, DbError::NotFound { .. }), "got {err:?}");
    assert!(
        !db_path.exists(),
        "open_existing must NOT create the DB file even when the parent dir exists"
    );
}

#[test]
fn open_existing_treats_stray_wal_without_main_file_as_absent() {
    // A crash mid-first-index can leave a `-wal`/`-shm` without the main file.
    // The existence check is keyed on the main file, so this is still gated.
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join(".cartog");
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("db.sqlite");
    std::fs::write(dir.join("db.sqlite-wal"), b"stray").unwrap();

    let err = Database::open_existing(&db_path, DEFAULT_EMBEDDING_DIM).unwrap_err();
    assert!(matches!(err, DbError::NotFound { .. }), "got {err:?}");
}

#[test]
fn open_still_creates_dir() {
    // The consent gate sits ABOVE open(); open() itself must keep creating.
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join(".cartog").join("db.sqlite");
    Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
    assert!(db_path.exists(), "open() must create the DB file");
}
