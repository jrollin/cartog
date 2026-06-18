//! Tests for Merkle subtree hashing and symbol diffing.

use crate::*;

#[test]
fn test_compute_merkle_hashes_populates_fields() {
    let source = "def foo():\n    pass\n";
    let mut symbols = vec![cartog_core::Symbol::new(
        "foo",
        cartog_core::SymbolKind::Function,
        "test.py",
        1,
        2,
        0,
        source.len() as u32,
        None,
    )];

    compute_merkle_hashes(&mut symbols, source);

    assert!(symbols[0].content_hash.is_some());
    assert!(symbols[0].subtree_hash.is_some());
}

#[test]
fn test_merkle_hashes_stable_across_position_changes() {
    let source_v1 = "def foo():\n    pass\n";
    let source_v2 = "\n\ndef foo():\n    pass\n";

    let mut sym_v1 = vec![cartog_core::Symbol::new(
        "foo",
        cartog_core::SymbolKind::Function,
        "test.py",
        1,
        2,
        0,
        source_v1.len() as u32,
        None,
    )];
    let mut sym_v2 = vec![cartog_core::Symbol::new(
        "foo",
        cartog_core::SymbolKind::Function,
        "test.py",
        3,
        4,
        2,
        source_v2.len() as u32,
        None,
    )];

    compute_merkle_hashes(&mut sym_v1, source_v1);
    compute_merkle_hashes(&mut sym_v2, source_v2);

    // content_hash depends on body text — different offset means different body slice
    // but if the body text is the same, hashes should match
    // Here the body text is the same "def foo():\n    pass\n"
    assert_eq!(sym_v1[0].content_hash, sym_v2[0].content_hash);
}

#[test]
fn test_merkle_diff_detects_added_symbol() {
    let old_hashes: Vec<(String, Option<String>, Option<String>)> = vec![];

    let mut new_symbols = vec![cartog_core::Symbol::new(
        "foo",
        cartog_core::SymbolKind::Function,
        "test.py",
        1,
        5,
        0,
        50,
        None,
    )];
    new_symbols[0].content_hash = Some("abc".to_string());
    new_symbols[0].subtree_hash = Some("def".to_string());

    let diff = merkle_diff(&new_symbols, &old_hashes);
    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.removed.len(), 0);
    assert_eq!(diff.modified.len(), 0);
}

#[test]
fn test_merkle_diff_detects_removed_symbol() {
    let old_hashes = vec![(
        "test.py:function:foo".to_string(),
        Some("abc".to_string()),
        Some("def".to_string()),
    )];

    let new_symbols: Vec<cartog_core::Symbol> = vec![];

    let diff = merkle_diff(&new_symbols, &old_hashes);
    assert_eq!(diff.added.len(), 0);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.removed[0], "test.py:function:foo");
}

#[test]
fn test_merkle_diff_detects_unchanged() {
    let old_hashes = vec![(
        "test.py:function:foo".to_string(),
        Some("abc".to_string()),
        Some("def".to_string()),
    )];

    let mut new_symbols = vec![cartog_core::Symbol::new(
        "foo",
        cartog_core::SymbolKind::Function,
        "test.py",
        1,
        5,
        0,
        50,
        None,
    )];
    new_symbols[0].content_hash = Some("abc".to_string());
    new_symbols[0].subtree_hash = Some("def".to_string());

    let diff = merkle_diff(&new_symbols, &old_hashes);
    assert_eq!(diff.unchanged, 1);
    assert_eq!(diff.added.len(), 0);
    assert_eq!(diff.modified.len(), 0);
}

#[test]
fn test_merkle_diff_detects_modified() {
    let old_hashes = vec![(
        "test.py:function:foo".to_string(),
        Some("old_hash".to_string()),
        Some("old_subtree".to_string()),
    )];

    let mut new_symbols = vec![cartog_core::Symbol::new(
        "foo",
        cartog_core::SymbolKind::Function,
        "test.py",
        1,
        5,
        0,
        50,
        None,
    )];
    new_symbols[0].content_hash = Some("new_hash".to_string());
    new_symbols[0].subtree_hash = Some("new_subtree".to_string());

    let diff = merkle_diff(&new_symbols, &old_hashes);
    assert_eq!(diff.modified.len(), 1);
    assert_eq!(diff.unchanged, 0);
}

#[test]
fn test_incremental_merkle_diff_pipeline() {
    use cartog_db::Database;

    let tmp = tempfile::TempDir::new().unwrap();
    // Create a non-dot subdirectory (tempfile may create .tmpXXX on macOS,
    // which is_ignored_dirname skips)
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();

    // Initial files
    let a_py = dir.join("a.py");
    let b_py = dir.join("b.py");

    std::fs::write(
        &a_py,
        r#"class Greeter:
def hello(self):
    return "hi"
def goodbye(self):
    return "bye"
"#,
    )
    .unwrap();

    std::fs::write(
        &b_py,
        r#"from a import Greeter
def main():
g = Greeter()
g.hello()
"#,
    )
    .unwrap();

    let db = Database::open_memory().unwrap();

    // ── Index 1: initial full index ──
    let r1 = index_directory(
        &db,
        &dir,
        true,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert_eq!(r1.files_indexed, 2);
    assert!(r1.symbols_added > 0, "should have symbols");

    let outline_a = db.outline("a.py").unwrap();
    assert_eq!(outline_a.len(), 3, "Greeter + hello + goodbye");
    let names_a: Vec<&str> = outline_a.iter().map(|s| s.name.as_str()).collect();
    assert!(names_a.contains(&"Greeter"));
    assert!(names_a.contains(&"hello"));
    assert!(names_a.contains(&"goodbye"));

    // Capture stable IDs
    let hello_id_v1 = outline_a
        .iter()
        .find(|s| s.name == "hello")
        .unwrap()
        .id
        .clone();
    let greeter_id_v1 = outline_a
        .iter()
        .find(|s| s.name == "Greeter")
        .unwrap()
        .id
        .clone();

    // Verify Merkle hashes populated
    let hashes = db.get_symbol_hashes_for_file("a.py").unwrap();
    assert!(
        hashes
            .iter()
            .all(|(_, ch, sh)| ch.is_some() && sh.is_some()),
        "all symbols should have hashes after indexing"
    );

    // ── Index 2: add a function to a.py ──
    std::fs::write(
        &a_py,
        r#"class Greeter:
def hello(self):
    return "hi"
def goodbye(self):
    return "bye"

def standalone():
return "I am new"
"#,
    )
    .unwrap();

    let r2 = index_directory(
        &db,
        &dir,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert_eq!(r2.files_indexed, 1, "only a.py changed");
    assert!(r2.files_skipped > 0, "b.py should be skipped");
    assert_eq!(r2.symbols_added, 1, "standalone is new");
    assert!(
        r2.symbols_unchanged >= 2,
        "hello and goodbye should be unchanged, got {}",
        r2.symbols_unchanged
    );

    let outline_a2 = db.outline("a.py").unwrap();
    assert_eq!(
        outline_a2.len(),
        4,
        "Greeter + hello + goodbye + standalone"
    );
    assert!(outline_a2.iter().any(|s| s.name == "standalone"));

    // Verify ID stability: hello and Greeter keep same IDs
    let hello_id_v2 = outline_a2
        .iter()
        .find(|s| s.name == "hello")
        .unwrap()
        .id
        .clone();
    let greeter_id_v2 = outline_a2
        .iter()
        .find(|s| s.name == "Greeter")
        .unwrap()
        .id
        .clone();
    assert_eq!(hello_id_v1, hello_id_v2, "hello ID should be stable");
    assert_eq!(greeter_id_v1, greeter_id_v2, "Greeter ID should be stable");

    // ── Index 3: remove goodbye from a.py ──
    std::fs::write(
        &a_py,
        r#"class Greeter:
def hello(self):
    return "hi"

def standalone():
return "I am new"
"#,
    )
    .unwrap();

    let r3 = index_directory(
        &db,
        &dir,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert_eq!(r3.files_indexed, 1);
    assert!(r3.symbols_removed >= 1, "goodbye should be removed");

    let outline_a3 = db.outline("a.py").unwrap();
    assert_eq!(outline_a3.len(), 3, "Greeter + hello + standalone");
    assert!(
        !outline_a3.iter().any(|s| s.name == "goodbye"),
        "goodbye should be gone"
    );

    // hello ID still stable after removal of sibling
    let hello_id_v3 = outline_a3
        .iter()
        .find(|s| s.name == "hello")
        .unwrap()
        .id
        .clone();
    assert_eq!(
        hello_id_v1, hello_id_v3,
        "hello ID stable after sibling removal"
    );
}
