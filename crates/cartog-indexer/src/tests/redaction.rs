//! Tests for secret redaction during indexing.

use crate::*;

fn project_with_secret() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("conf.py"),
        "def connect():\n    token = \"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"\n    return token\n",
    )
    .unwrap();
    (tmp, root)
}

fn only_content(db: &Database) -> String {
    let ids = db.all_content_symbol_ids().unwrap();
    let map = db.get_symbol_contents_batch(&ids).unwrap();
    map.values()
        .map(|(c, _)| c.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn indexing_redacts_secret_in_symbol_content() {
    let (_tmp, root) = project_with_secret();
    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &root,
        true,
        false,
        None,
        None,
        RedactionConfig::enabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let content = only_content(&db);
    assert!(content.contains("[REDACTED_SECRET]"));
    assert!(!content.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
}

#[test]
fn redaction_disabled_keeps_secret_verbatim() {
    let (_tmp, root) = project_with_secret();
    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &root,
        true,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let content = only_content(&db);
    assert!(content.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!content.contains("[REDACTED_SECRET]"));
}

#[test]
fn redacted_secret_is_not_searchable_in_fts() {
    let (_tmp, root) = project_with_secret();
    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &root,
        true,
        false,
        None,
        None,
        RedactionConfig::enabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let hits = db
        .fts5_search("\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"", 10)
        .unwrap();
    assert!(
        hits.is_empty(),
        "secret must not be searchable after redaction"
    );
}

#[test]
fn sensitive_file_is_never_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.py"), "def f():\n    pass\n").unwrap();
    // A code-extension file whose name matches the deny-list.
    std::fs::write(root.join("id_rsa"), "PRIVATE KEY").unwrap();
    std::fs::write(root.join(".env"), "API_KEY=ghp_xxxxxxxxxxxxxxxxxxxx").unwrap();

    let db = Database::open_memory().unwrap();
    let r = index_directory(
        &db,
        &root,
        true,
        false,
        None,
        None,
        RedactionConfig::enabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    assert_eq!(r.files_indexed, 1, "only a.py indexes");
    assert!(
        r.files_redacted_skipped >= 1,
        "deny-listed files are skipped"
    );
}

#[test]
fn enabling_redaction_on_warm_index_reindexes_and_scrubs() {
    let (_tmp, root) = project_with_secret();
    let db = Database::open_memory().unwrap();

    // First index with redaction OFF: secret is stored verbatim.
    index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert!(only_content(&db).contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));

    // Plain re-index (no --force) with redaction ON must promote to a full
    // re-index via the policy fingerprint and scrub the stored secret.
    let r = index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        RedactionConfig::enabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert!(r.redaction_backfilled, "policy change must flag a backfill");
    let content = only_content(&db);
    assert!(content.contains("[REDACTED_SECRET]"));
    assert!(!content.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
}

#[test]
fn content_hash_is_identical_with_redaction_on_vs_off() {
    let (_tmp, root) = project_with_secret();

    let db_on = Database::open_memory().unwrap();
    index_directory(
        &db_on,
        &root,
        true,
        false,
        None,
        None,
        RedactionConfig::enabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let db_off = Database::open_memory().unwrap();
    index_directory(
        &db_off,
        &root,
        true,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    // Hashing keys off raw source, so redaction must not perturb identity.
    let mut ids_on = db_on.all_content_symbol_ids().unwrap();
    let mut ids_off = db_off.all_content_symbol_ids().unwrap();
    ids_on.sort();
    ids_off.sort();
    for id in &ids_on {
        let h_on = db_on.get_symbol(id).unwrap().unwrap().content_hash;
        let h_off = db_off.get_symbol(id).unwrap().unwrap().content_hash;
        assert_eq!(h_on, h_off, "content_hash must not depend on redaction");
    }
}
