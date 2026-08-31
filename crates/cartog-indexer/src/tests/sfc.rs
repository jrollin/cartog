//! Tests for SFC whole-file component symbols and the import edges they resolve.

use crate::*;

/// Index a temp tree with a Vue app importing a component from a sibling directory.
fn index_vue_app() -> (tempfile::TempDir, cartog_db::Database) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let components = src.join("components");
    std::fs::create_dir_all(&components).unwrap();
    std::fs::write(
        src.join("App.vue"),
        "<template><LoginForm /></template>\n<script setup>\nimport LoginForm from './components/LoginForm.vue';\n</script>\n",
    )
    .unwrap();
    std::fs::write(
        components.join("LoginForm.vue"),
        "<template><form/></template>\n<script setup>\nfunction submit() { return 1; }\n</script>\n",
    )
    .unwrap();

    let db = cartog_db::Database::open_memory().unwrap();
    index_directory(
        &db,
        tmp.path(),
        false,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &WalkFilter::unrestricted(),
    )
    .unwrap();
    (tmp, db)
}

#[test]
fn sfc_import_resolves_to_the_component_symbol() {
    let (_tmp, db) = index_vue_app();

    let files = db.all_files().unwrap();
    let symbols = db.symbols_for_files(&files, None).unwrap();
    let component = symbols
        .iter()
        .find(|s| s.name == "LoginForm" && s.kind == cartog_core::SymbolKind::Component)
        .expect("LoginForm component symbol");
    assert!(
        component
            .file_path
            .ends_with("src/components/LoginForm.vue"),
        "component lives in its own file, got {}",
        component.file_path
    );

    let refs = db.refs("LoginForm", None).unwrap();
    let import = refs
        .iter()
        .find(|(e, _)| e.kind == cartog_core::EdgeKind::Imports && e.file_path.ends_with("App.vue"))
        .expect("imports edge from App.vue");
    assert_eq!(import.0.target_id.as_ref(), Some(&component.id));

    // state 1 = resolved: the edge is gone from the unresolved backlog.
    assert!(
        !db.unresolved_edges()
            .unwrap()
            .iter()
            .any(|e| e.target_name == "LoginForm"),
        "resolved import must leave resolution_state 0"
    );
}
