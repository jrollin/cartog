use super::test_symbol;
use crate::*;

// ── RAG: Symbol Content Tests ──

#[test]
fn test_upsert_and_get_symbol_content() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("my_func", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym).unwrap();

    db.upsert_symbol_content(
        &sym.id,
        "my_func",
        "def my_func(): pass",
        "// File: a.py\n// Type: function\n// Name: my_func",
    )
    .unwrap();

    let result = db.get_symbol_content(&sym.id).unwrap();
    assert!(result.is_some());
    let (content, header) = result.unwrap();
    assert_eq!(content, "def my_func(): pass");
    assert!(header.contains("my_func"));
}

#[test]
fn test_insert_symbol_contents_batch() {
    let db = Database::open_memory().unwrap();
    let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let sym2 = test_symbol("bar", SymbolKind::Function, "a.py", 10);
    db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

    let items = vec![
        (
            sym1.id.clone(),
            "foo".to_string(),
            "def foo(): pass".to_string(),
            "header1".to_string(),
        ),
        (
            sym2.id.clone(),
            "bar".to_string(),
            "def bar(): pass".to_string(),
            "header2".to_string(),
        ),
    ];
    db.insert_symbol_contents(&items).unwrap();

    assert_eq!(db.symbol_content_count().unwrap(), 2);
    assert!(db.get_symbol_content(&sym1.id).unwrap().is_some());
    assert!(db.get_symbol_content(&sym2.id).unwrap().is_some());
}

#[test]
fn test_clear_symbol_content_for_file() {
    let db = Database::open_memory().unwrap();
    let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let sym2 = test_symbol("bar", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

    db.upsert_symbol_content(&sym1.id, "foo", "content1", "header1")
        .unwrap();
    db.upsert_symbol_content(&sym2.id, "bar", "content2", "header2")
        .unwrap();
    assert_eq!(db.symbol_content_count().unwrap(), 2);

    db.clear_symbol_content_for_file("a.py").unwrap();
    assert_eq!(db.symbol_content_count().unwrap(), 1);
    assert!(db.get_symbol_content(&sym1.id).unwrap().is_none());
    assert!(db.get_symbol_content(&sym2.id).unwrap().is_some());
}

// ── RAG: FTS5 Tests ──

#[test]
fn test_fts5_search_by_content() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("validate_token", SymbolKind::Function, "auth.py", 1);
    db.insert_symbol(&sym).unwrap();

    db.upsert_symbol_content(
        &sym.id,
        "validate_token",
        "def validate_token(token: str) -> bool:\n    return token.is_valid()",
        "// File: auth.py",
    )
    .unwrap();

    // Search by content keyword
    let results = db.fts5_search("\"validate\"", 10).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0], sym.id);
}

#[test]
fn test_fts5_search_no_match() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym).unwrap();
    db.upsert_symbol_content(&sym.id, "foo", "def foo(): pass", "header")
        .unwrap();

    let results = db.fts5_search("\"nonexistent_term_xyz\"", 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn fts5_drops_old_content_when_symbol_content_is_replaced() {
    // Re-indexing a symbol (INSERT OR REPLACE on symbol_content) must not
    // leave the previous content searchable. Without an explicit delete the
    // FTS5 external-content delete trigger does not fire on REPLACE-conflict
    // (recursive_triggers is off), so a stale secret stays searchable.
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("load", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym).unwrap();

    db.upsert_symbol_content(&sym.id, "load", "key = ghp_oldsecrettoken_value", "h")
        .unwrap();
    assert!(!db
        .fts5_search("\"ghp_oldsecrettoken_value\"", 10)
        .unwrap()
        .is_empty());

    db.upsert_symbol_content(&sym.id, "load", "key = [REDACTED_SECRET]", "h")
        .unwrap();

    // Assert against the raw FTS index, not the JOIN-filtered fts5_search:
    // an orphaned FTS row survives the JOIN filter but still leaks the
    // plaintext token at the index level.
    let stale: i64 = db
        .conn
        .query_row(
            "SELECT count(*) FROM symbol_fts WHERE symbol_fts MATCH 'ghp_oldsecrettoken_value'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0, "old plaintext must not remain in the FTS index");
    assert_eq!(db.symbol_content_count().unwrap(), 1);
}

// ── RAG: Embedding Map Tests ──

#[test]
fn test_get_or_create_embedding_id() {
    let db = Database::open_memory().unwrap();

    let id1 = db.get_or_create_embedding_id("a.py:foo:1").unwrap();
    let id2 = db.get_or_create_embedding_id("a.py:foo:1").unwrap();
    let id3 = db.get_or_create_embedding_id("b.py:bar:5").unwrap();

    assert_eq!(id1, id2, "same symbol should return same ID");
    assert_ne!(id1, id3, "different symbols should get different IDs");
}

#[test]
fn test_symbol_id_for_embedding() {
    let db = Database::open_memory().unwrap();
    let eid = db.get_or_create_embedding_id("test:sym:1").unwrap();

    let sym_id = db.symbol_id_for_embedding(eid).unwrap();
    assert_eq!(sym_id, Some("test:sym:1".to_string()));

    let none = db.symbol_id_for_embedding(99999).unwrap();
    assert!(none.is_none());
}

#[test]
fn test_symbol_ids_for_embeddings_batch() {
    let db = Database::open_memory().unwrap();
    let eid1 = db.get_or_create_embedding_id("a:foo:1").unwrap();
    let eid2 = db.get_or_create_embedding_id("b:bar:2").unwrap();

    let results = db.symbol_ids_for_embeddings(&[eid1, eid2]).unwrap();
    assert_eq!(results.len(), 2);
}

// ── RAG: Vector Storage Tests ──

#[test]
fn test_upsert_and_search_embedding() {
    let db = Database::open_memory().unwrap();
    let eid = db.get_or_create_embedding_id("a:foo:1").unwrap();

    // Create a simple 384-dim vector
    let mut embedding = vec![0.0f32; 384];
    embedding[0] = 1.0;
    let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

    db.upsert_embedding(eid, &bytes).unwrap();

    // Search with a similar vector
    let query = bytes.clone();
    let results = db.vector_search(&query, 5).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, eid);
    assert!(
        results[0].1 < 0.01,
        "self-match should have near-zero distance"
    );
}

#[test]
fn test_insert_embeddings_batch() {
    let db = Database::open_memory().unwrap();
    let eid1 = db.get_or_create_embedding_id("a:foo:1").unwrap();
    let eid2 = db.get_or_create_embedding_id("b:bar:2").unwrap();

    let make_vec = |val: f32| -> Vec<u8> {
        let v = vec![val; 384];
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    };

    let items = vec![(eid1, make_vec(0.1)), (eid2, make_vec(0.9))];
    db.insert_embeddings(&items).unwrap();

    assert_eq!(db.embedding_count().unwrap(), 2);
}

#[test]
fn test_has_embedding() {
    let db = Database::open_memory().unwrap();
    assert!(!db.has_embedding("nonexistent").unwrap());

    let eid = db.get_or_create_embedding_id("a:foo:1").unwrap();
    // Map exists but no vector yet
    assert!(!db.has_embedding("a:foo:1").unwrap());

    // Insert vector
    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    db.upsert_embedding(eid, &bytes).unwrap();
    assert!(db.has_embedding("a:foo:1").unwrap());
}

#[test]
fn test_clear_all_embeddings() {
    let db = Database::open_memory().unwrap();
    let eid1 = db.get_or_create_embedding_id("a:foo:1").unwrap();
    let eid2 = db.get_or_create_embedding_id("b:bar:2").unwrap();

    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    db.upsert_embedding(eid1, &bytes).unwrap();
    db.upsert_embedding(eid2, &bytes).unwrap();
    assert_eq!(db.embedding_count().unwrap(), 2);

    db.clear_all_embeddings().unwrap();
    assert_eq!(db.embedding_count().unwrap(), 0);
}

#[test]
fn embedding_count_excludes_orphan_map_rows() {
    let db = Database::open_memory().unwrap();
    // Orphan map row (no vector yet) must not count as a usable embedding.
    let _eid = db.get_or_create_embedding_id("a:foo:1").unwrap();
    assert_eq!(db.embedding_count().unwrap(), 0);

    // Once a vector is written it counts.
    let eid = db.get_or_create_embedding_id("a:foo:1").unwrap();
    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    db.upsert_embedding(eid, &bytes).unwrap();
    assert_eq!(db.embedding_count().unwrap(), 1);
}

#[test]
fn test_symbols_needing_embeddings() {
    let db = Database::open_memory().unwrap();
    let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let sym2 = test_symbol("bar", SymbolKind::Function, "a.py", 10);
    db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

    // Add content for both
    db.upsert_symbol_content(&sym1.id, "foo", "def foo(): pass", "header")
        .unwrap();
    db.upsert_symbol_content(&sym2.id, "bar", "def bar(): pass", "header")
        .unwrap();

    // Both need embeddings initially
    let needing = db.symbols_needing_embeddings().unwrap();
    assert_eq!(needing.len(), 2);

    // Embed one
    let eid = db.get_or_create_embedding_id(&sym1.id).unwrap();
    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    db.upsert_embedding(eid, &bytes).unwrap();

    // Only one needs embedding now
    let needing = db.symbols_needing_embeddings().unwrap();
    assert_eq!(needing.len(), 1);
    assert_eq!(needing[0], sym2.id);
}

#[test]
fn test_clear_rag_data_for_file() {
    let db = Database::open_memory().unwrap();
    let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let sym2 = test_symbol("bar", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

    db.upsert_symbol_content(&sym1.id, "foo", "content1", "header1")
        .unwrap();
    db.upsert_symbol_content(&sym2.id, "bar", "content2", "header2")
        .unwrap();

    let eid1 = db.get_or_create_embedding_id(&sym1.id).unwrap();
    let eid2 = db.get_or_create_embedding_id(&sym2.id).unwrap();
    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    db.upsert_embedding(eid1, &bytes).unwrap();
    db.upsert_embedding(eid2, &bytes).unwrap();

    // Clear RAG data for a.py only
    db.clear_rag_data_for_file("a.py").unwrap();

    // a.py data gone
    assert!(db.get_symbol_content(&sym1.id).unwrap().is_none());
    assert!(!db.has_embedding(&sym1.id).unwrap());

    // b.py data intact
    assert!(db.get_symbol_content(&sym2.id).unwrap().is_some());
    assert!(db.has_embedding(&sym2.id).unwrap());
}

#[test]
fn clear_embeddings_for_symbols_drops_only_named_ids() {
    let db = Database::open_memory().unwrap();
    let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let sym2 = test_symbol("bar", SymbolKind::Function, "a.py", 10);
    db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();
    db.upsert_symbol_content(&sym1.id, "foo", "def foo(): pass", "header")
        .unwrap();
    db.upsert_symbol_content(&sym2.id, "bar", "def bar(): pass", "header")
        .unwrap();

    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    for sym in [&sym1, &sym2] {
        let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
        db.upsert_embedding(eid, &bytes).unwrap();
    }
    assert_eq!(db.embedding_count().unwrap(), 2);

    let tx = db.begin_indexing_tx().unwrap();
    db.clear_embeddings_for_symbols_in_tx(std::slice::from_ref(&sym1.id))
        .unwrap();
    tx.commit().unwrap();

    // sym1's embedding gone, sym2 intact, content untouched for both.
    assert!(!db.has_embedding(&sym1.id).unwrap());
    assert!(db.has_embedding(&sym2.id).unwrap());
    assert!(db.get_symbol_content(&sym1.id).unwrap().is_some());
    // The cleared symbol is now back in the needs-embedding set.
    let needing = db.symbols_needing_embeddings().unwrap();
    assert_eq!(needing, vec![sym1.id.clone()]);
}

#[test]
fn clear_embeddings_for_symbols_is_noop_for_unembedded_id() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    db.insert_symbols(std::slice::from_ref(&sym)).unwrap();
    db.upsert_symbol_content(&sym.id, "foo", "def foo(): pass", "header")
        .unwrap();

    let tx = db.begin_indexing_tx().unwrap();
    db.clear_embeddings_for_symbols_in_tx(std::slice::from_ref(&sym.id))
        .unwrap();
    tx.commit().unwrap();

    assert_eq!(db.embedding_count().unwrap(), 0);
}

#[test]
fn test_all_content_symbol_ids() {
    let db = Database::open_memory().unwrap();
    let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let sym2 = test_symbol("bar", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

    db.upsert_symbol_content(&sym1.id, "foo", "content1", "header1")
        .unwrap();
    db.upsert_symbol_content(&sym2.id, "bar", "content2", "header2")
        .unwrap();

    let all = db.all_content_symbol_ids().unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_symbols_needing_embeddings_excludes_variables() {
    let db = Database::open_memory().unwrap();
    let func = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let var = test_symbol("MAX_RETRIES", SymbolKind::Variable, "a.py", 10);
    let cls = test_symbol("Service", SymbolKind::Class, "a.py", 20);
    db.insert_symbols(&[func.clone(), var.clone(), cls.clone()])
        .unwrap();

    // Add content for all three
    db.upsert_symbol_content(&func.id, "process", "def process(): pass", "header")
        .unwrap();
    db.upsert_symbol_content(&var.id, "MAX_RETRIES", "MAX_RETRIES = 3", "header")
        .unwrap();
    db.upsert_symbol_content(&cls.id, "Service", "class Service: pass", "header")
        .unwrap();

    // Only function and class should need embeddings (variable excluded)
    let needing = db.symbols_needing_embeddings().unwrap();
    assert_eq!(needing.len(), 2);
    assert!(!needing.contains(&var.id), "variables should be excluded");
    assert!(needing.contains(&func.id));
    assert!(needing.contains(&cls.id));
}

#[test]
fn test_all_content_symbol_ids_excludes_variables() {
    let db = Database::open_memory().unwrap();
    let func = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let var = test_symbol("MY_VAR", SymbolKind::Variable, "a.py", 10);
    let method = test_symbol("bar", SymbolKind::Method, "a.py", 20);
    db.insert_symbols(&[func.clone(), var.clone(), method.clone()])
        .unwrap();

    db.upsert_symbol_content(&func.id, "foo", "def foo(): pass", "header")
        .unwrap();
    db.upsert_symbol_content(&var.id, "MY_VAR", "MY_VAR = 42", "header")
        .unwrap();
    db.upsert_symbol_content(&method.id, "bar", "def bar(self): pass", "header")
        .unwrap();

    let all = db.all_content_symbol_ids().unwrap();
    assert_eq!(all.len(), 2, "variables should be excluded");
    assert!(!all.contains(&var.id));
}

#[test]
fn test_get_symbol_contents_batch() {
    let db = Database::open_memory().unwrap();
    let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let sym2 = test_symbol("bar", SymbolKind::Function, "a.py", 10);
    let sym3 = test_symbol("baz", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[sym1.clone(), sym2.clone(), sym3.clone()])
        .unwrap();

    db.upsert_symbol_content(&sym1.id, "foo", "def foo(): pass", "h1")
        .unwrap();
    db.upsert_symbol_content(&sym2.id, "bar", "def bar(): pass", "h2")
        .unwrap();
    // sym3 has no content

    let ids = vec![sym1.id.clone(), sym2.id.clone(), sym3.id.clone()];
    let map = db.get_symbol_contents_batch(&ids).unwrap();
    assert_eq!(map.len(), 2);
    assert!(map.contains_key(&sym1.id));
    assert!(map.contains_key(&sym2.id));
    assert!(!map.contains_key(&sym3.id));
    assert_eq!(map[&sym1.id].0, "def foo(): pass");
}

#[test]
fn test_get_symbol_contents_batch_empty() {
    let db = Database::open_memory().unwrap();
    let map = db.get_symbol_contents_batch(&[]).unwrap();
    assert!(map.is_empty());
}

#[test]
fn test_get_symbol_by_id() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym).unwrap();

    let found = db.get_symbol(&sym.id).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "foo");

    let not_found = db.get_symbol("nonexistent").unwrap();
    assert!(not_found.is_none());
}

#[test]
fn test_symbols_for_files_basic() {
    let db = Database::open_memory().unwrap();
    let s1 = test_symbol("func_a", SymbolKind::Function, "src/a.py", 1);
    let s2 = test_symbol("func_b", SymbolKind::Function, "src/a.py", 10);
    let s3 = test_symbol("ClassC", SymbolKind::Class, "src/b.py", 1);
    let s4 = test_symbol("func_d", SymbolKind::Function, "src/c.py", 1);
    db.insert_symbols(&[s1, s2, s3, s4]).unwrap();

    // Query for two files
    let files = vec!["src/a.py".to_string(), "src/b.py".to_string()];
    let results = db.symbols_for_files(&files, None).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].file_path, "src/a.py");
    assert_eq!(results[2].file_path, "src/b.py");
}

#[test]
fn test_symbols_for_files_kind_filter() {
    let db = Database::open_memory().unwrap();
    let s1 = test_symbol("func_a", SymbolKind::Function, "src/a.py", 1);
    let s2 = test_symbol("ClassB", SymbolKind::Class, "src/a.py", 10);
    db.insert_symbols(&[s1, s2]).unwrap();

    let files = vec!["src/a.py".to_string()];
    let results = db
        .symbols_for_files(&files, Some(SymbolKind::Function))
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "func_a");
}

#[test]
fn test_symbols_for_files_empty_input() {
    let db = Database::open_memory().unwrap();
    let results = db.symbols_for_files(&[], None).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_symbols_for_files_no_matching_files() {
    let db = Database::open_memory().unwrap();
    let s1 = test_symbol("func_a", SymbolKind::Function, "src/a.py", 1);
    db.insert_symbol(&s1).unwrap();

    let files = vec!["src/nonexistent.py".to_string()];
    let results = db.symbols_for_files(&files, None).unwrap();
    assert!(results.is_empty());
}

// ── In-degree centrality tests ──

#[test]
fn test_compute_in_degrees() {
    let db = Database::open_memory().unwrap();
    let s1 = test_symbol("func_a", SymbolKind::Function, "a.py", 1);
    let s2 = test_symbol("func_b", SymbolKind::Function, "b.py", 1);
    let s3 = test_symbol("func_c", SymbolKind::Function, "c.py", 1);
    db.insert_symbols(&[s1.clone(), s2.clone(), s3.clone()])
        .unwrap();

    // func_b calls func_a (2 call sites), func_c calls func_a (1 call site)
    let e1 = Edge::new(&s2.id, "func_a", EdgeKind::Calls, "b.py", 5);
    let e2 = Edge::new(&s2.id, "func_a", EdgeKind::Calls, "b.py", 10);
    let e3 = Edge::new(&s3.id, "func_a", EdgeKind::Calls, "c.py", 3);
    // func_c also calls func_b
    let e4 = Edge::new(&s3.id, "func_b", EdgeKind::Calls, "c.py", 7);
    db.insert_edges(&[e1, e2, e3, e4]).unwrap();
    db.resolve_edges().unwrap();
    db.compute_in_degrees().unwrap();

    let sym_a = db.get_symbol(&s1.id).unwrap().unwrap();
    let sym_b = db.get_symbol(&s2.id).unwrap().unwrap();
    let sym_c = db.get_symbol(&s3.id).unwrap().unwrap();

    assert_eq!(sym_a.in_degree, 3, "func_a should have 3 incoming edges");
    assert_eq!(sym_b.in_degree, 1, "func_b should have 1 incoming edge");
    assert_eq!(sym_c.in_degree, 0, "func_c should have 0 incoming edges");
}

#[test]
fn test_compute_in_degrees_resets() {
    let db = Database::open_memory().unwrap();
    let s1 = test_symbol("func_a", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&s1).unwrap();

    // Manually set in_degree to 99
    db.conn
        .execute(
            "UPDATE symbols SET in_degree = 99 WHERE id = ?1",
            params![s1.id],
        )
        .unwrap();

    // compute_in_degrees should reset to 0 (no edges)
    db.compute_in_degrees().unwrap();
    let sym = db.get_symbol(&s1.id).unwrap().unwrap();
    assert_eq!(sym.in_degree, 0);
}

#[test]
fn test_top_symbols_ordered_by_centrality() {
    let db = Database::open_memory().unwrap();
    let s1 = test_symbol("hub", SymbolKind::Function, "a.py", 1);
    let s2 = test_symbol("leaf", SymbolKind::Function, "b.py", 1);
    let s3 = test_symbol("mid", SymbolKind::Function, "c.py", 1);
    db.insert_symbols(&[s1.clone(), s2.clone(), s3.clone()])
        .unwrap();

    // Set in-degrees directly for testing
    db.conn
        .execute(
            "UPDATE symbols SET in_degree = 10 WHERE id = ?1",
            params![s1.id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE symbols SET in_degree = 1 WHERE id = ?1",
            params![s2.id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE symbols SET in_degree = 5 WHERE id = ?1",
            params![s3.id],
        )
        .unwrap();

    let top = db.top_symbols(10).unwrap();
    assert_eq!(top.len(), 3);
    assert_eq!(top[0].name, "hub");
    assert_eq!(top[0].in_degree, 10);
    assert_eq!(top[1].name, "mid");
    assert_eq!(top[2].name, "leaf");
}

#[test]
fn test_search_uses_in_degree_tiebreaker() {
    let db = Database::open_memory().unwrap();
    // Two functions with same name prefix, different centrality
    let s1 = test_symbol("parse_request", SymbolKind::Function, "a.py", 1);
    let s2 = test_symbol("parse_response", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[s1.clone(), s2.clone()]).unwrap();

    db.conn
        .execute(
            "UPDATE symbols SET in_degree = 20 WHERE id = ?1",
            params![s1.id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE symbols SET in_degree = 5 WHERE id = ?1",
            params![s2.id],
        )
        .unwrap();

    let results = db.search("parse", None, None, 10).unwrap();
    assert_eq!(results.len(), 2);
    // parse_request (in_degree=20) should come before parse_response (in_degree=5)
    assert_eq!(results[0].name, "parse_request");
    assert_eq!(results[1].name, "parse_response");
}

#[test]
fn test_schema_version_stored() {
    let db = Database::open_memory().unwrap();
    let version = db.get_metadata("schema_version").unwrap();
    assert!(version.is_some());
    assert_eq!(version.unwrap(), SCHEMA_VERSION.to_string());
}
