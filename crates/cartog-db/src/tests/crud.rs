use super::test_symbol;
use crate::*;

#[test]
fn test_insert_and_query_symbols() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("my_func", SymbolKind::Function, "test.py", 10);
    db.insert_symbol(&sym).unwrap();

    let outline = db.outline("test.py").unwrap();
    assert_eq!(outline.len(), 1);
    assert_eq!(outline[0].name, "my_func");
}

#[test]
fn test_optimize_populates_planner_stats() {
    // PRAGMA optimize must build sqlite_stat1 once the tables are large
    // enough to be worth analyzing — proving the planner has real stats to
    // pick join order from (the #110 misplan was a no-stats guess).
    let db = Database::open_memory().unwrap();
    let syms: Vec<_> = (0..2000)
        .map(|i| test_symbol(&format!("f{i}"), SymbolKind::Function, "a.py", i + 1))
        .collect();
    db.insert_symbols(&syms).unwrap();

    db.optimize().unwrap();

    let analyzed: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'sqlite_stat1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(analyzed, 1, "PRAGMA optimize must create sqlite_stat1");
}

#[test]
fn test_optimize_is_safe_on_empty_db() {
    let db = Database::open_memory().unwrap();
    db.optimize().unwrap(); // no-op, must not error
}

#[test]
fn is_empty_reflects_symbol_presence() {
    let db = Database::open_memory().unwrap();
    assert!(db.is_empty().unwrap(), "fresh DB should be empty");
    db.insert_symbol(&test_symbol("f", SymbolKind::Function, "a.py", 1))
        .unwrap();
    assert!(!db.is_empty().unwrap(), "DB with a symbol is not empty");
}

#[test]
fn test_insert_and_query_edges() {
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("caller_fn", SymbolKind::Function, "a.py", 1);
    let callee = test_symbol("callee_fn", SymbolKind::Function, "b.py", 1);
    db.insert_symbol(&caller).unwrap();
    db.insert_symbol(&callee).unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
        target_name: "callee_fn".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "a.py".to_string(),
        line: 5,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let refs = db.refs("callee_fn", None).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].0.source_id, caller.id);
}

#[test]
fn test_edge_resolution() {
    let db = Database::open_memory().unwrap();
    let sym_a = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let sym_b = test_symbol("helper", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[sym_a.clone(), sym_b.clone()]).unwrap();

    let edge = Edge {
        source_id: sym_a.id.clone(),
        target_name: "helper".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "a.py".to_string(),
        line: 5,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);
}

#[test]
fn test_stats() {
    let db = Database::open_memory().unwrap();
    let file = FileInfo {
        path: "test.py".to_string(),
        last_modified: 0.0,
        hash: "abc".to_string(),
        language: "python".to_string(),
        num_symbols: 2,
    };
    db.upsert_file(&file).unwrap();
    let sym = test_symbol("foo", SymbolKind::Function, "test.py", 1);
    db.insert_symbol(&sym).unwrap();

    let stats = db.stats().unwrap();
    assert_eq!(stats.num_files, 1);
    assert_eq!(stats.num_symbols, 1);
}

#[test]
fn savings_breakdown_empty_returns_zero() {
    let db = Database::open_memory().unwrap();
    let r = db.savings_breakdown().unwrap();
    assert_eq!(r.total_queries, 0);
    assert_eq!(r.tokens_used_cartog, 0);
    assert_eq!(r.tokens_used_grep, 0);
    assert_eq!(r.estimated_tokens_saved, 0);
    assert_eq!(r.percent_saved, 0);
    assert!(r.by_tool.is_empty());
    assert!(r.by_source.is_empty());
    assert_eq!(r.baseline_delta, TOKENS_SAVED_PER_QUERY);
}

#[test]
fn log_query_persists_rows_aggregated_by_tool_and_source() {
    let db = Database::open_memory().unwrap();
    db.log_query("search", "cli");
    db.log_query("search", "cli");
    db.log_query("refs", "cli");
    db.log_query("search", "mcp");
    db.log_query("impact", "mcp");

    let r = db.savings_breakdown().unwrap();
    assert_eq!(r.total_queries, 5);
    // With/without/saved derived from the per-query constants.
    assert_eq!(r.tokens_used_cartog, 5 * TOKENS_PER_QUERY_CARTOG as u64);
    assert_eq!(r.tokens_used_grep, 5 * TOKENS_PER_QUERY_GREP as u64);
    assert_eq!(r.estimated_tokens_saved, 5 * TOKENS_SAVED_PER_QUERY as u64);
    // ~83% saved given 280 vs 1700 baseline.
    assert_eq!(r.percent_saved, 83);

    // by_tool sorted by count desc, then name
    let tool_counts: Vec<_> = r.by_tool.iter().map(|(t, c)| (t.as_str(), *c)).collect();
    assert_eq!(tool_counts, vec![("search", 3), ("impact", 1), ("refs", 1)]);

    let src_counts: Vec<_> = r.by_source.iter().map(|(s, c)| (s.as_str(), *c)).collect();
    assert_eq!(src_counts, vec![("cli", 3), ("mcp", 2)]);
}

#[test]
fn log_query_noop_on_read_only_attach() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let primary = Database::open(&db_path, 384).unwrap();
        primary.log_query("search", "cli"); // primary write succeeds
    }

    let reader = Database::open_readonly(&db_path).unwrap();
    assert!(reader.is_read_only());
    // log_query on read-only attach must silently no-op (no panic, no insert).
    reader.log_query("search", "mcp");
    reader.log_query("refs", "mcp");

    let r = reader.savings_breakdown().unwrap();
    // Only the primary's row is visible — secondary writes were dropped.
    assert_eq!(r.total_queries, 1);
    assert_eq!(r.by_tool, vec![("search".to_string(), 1)]);
}
#[test]
fn test_hierarchy_query() {
    let db = Database::open_memory().unwrap();

    let parent = test_symbol("Animal", SymbolKind::Class, "a.py", 1);
    let child = test_symbol("Dog", SymbolKind::Class, "a.py", 10);
    db.insert_symbols(&[parent, child.clone()]).unwrap();

    db.insert_edge(&Edge {
        source_id: child.id.clone(),
        target_name: "Animal".to_string(),
        target_id: None,
        kind: EdgeKind::Inherits,
        file_path: "a.py".to_string(),
        line: 10,
        provenance: None,
    })
    .unwrap();

    let pairs = db.hierarchy("Dog").unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].0, "Dog");
    assert_eq!(pairs[0].1, "Animal");
}

#[test]
fn test_file_deps_query() {
    let db = Database::open_memory().unwrap();

    let import_sym = test_symbol("os", SymbolKind::Import, "main.py", 1);
    db.insert_symbol(&import_sym).unwrap();

    db.insert_edge(&Edge {
        source_id: import_sym.id.clone(),
        target_name: "os".to_string(),
        target_id: None,
        kind: EdgeKind::Imports,
        file_path: "main.py".to_string(),
        line: 1,
        provenance: None,
    })
    .unwrap();

    let deps = db.file_deps("main.py").unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].target_name, "os");
}

#[test]
fn test_remove_file_clears_all_data() {
    let db = Database::open_memory().unwrap();

    let sym = test_symbol("foo", SymbolKind::Function, "test.py", 1);
    db.insert_symbol(&sym).unwrap();
    db.insert_edge(&Edge {
        source_id: sym.id.clone(),
        target_name: "bar".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "test.py".to_string(),
        line: 5,
        provenance: None,
    })
    .unwrap();
    db.upsert_file(&FileInfo {
        path: "test.py".to_string(),
        last_modified: 0.0,
        hash: "abc".to_string(),
        language: "python".to_string(),
        num_symbols: 1,
    })
    .unwrap();

    db.remove_file("test.py").unwrap();

    assert!(db.outline("test.py").unwrap().is_empty());
    assert!(db.get_file("test.py").unwrap().is_none());
}

#[test]
fn test_refs_with_kind_filter() {
    let db = Database::open_memory().unwrap();
    let parent = test_symbol("AuthService", SymbolKind::Class, "a.py", 1);
    let child = test_symbol("AdminService", SymbolKind::Class, "a.py", 20);
    let caller = test_symbol("login", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[parent.clone(), child.clone(), caller.clone()])
        .unwrap();

    db.insert_edges(&[
        Edge {
            source_id: child.id.clone(),
            target_name: "AuthService".to_string(),
            target_id: None,
            kind: EdgeKind::Inherits,
            file_path: "a.py".to_string(),
            line: 20,
            provenance: None,
        },
        Edge {
            source_id: caller.id.clone(),
            target_name: "AuthService".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "b.py".to_string(),
            line: 5,
            provenance: None,
        },
    ])
    .unwrap();

    // No filter → both edges
    let all = db.refs("AuthService", None).unwrap();
    assert_eq!(all.len(), 2);

    // Filter inherits only
    let inherits = db.refs("AuthService", Some(EdgeKind::Inherits)).unwrap();
    assert_eq!(inherits.len(), 1);
    assert_eq!(inherits[0].0.kind, EdgeKind::Inherits);

    // Filter calls only
    let calls = db.refs("AuthService", Some(EdgeKind::Calls)).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.kind, EdgeKind::Calls);

    // Filter with no matches
    let raises = db.refs("AuthService", Some(EdgeKind::Raises)).unwrap();
    assert!(raises.is_empty());
}

#[test]
fn test_refs_matches_via_resolved_target_id_short_name() {
    // The edge's literal target_name is qualified (never equals the short
    // name), so it only matches `refs("BaseService")` through its resolved
    // target_id → a symbol named "BaseService". Guards the `target_id IN
    // (SELECT id ... WHERE name = ?)` arm of refs() against regressing to a
    // plain `target_name = ?` match. Mirrors the kind-filtered branch too.
    let db = Database::open_memory().unwrap();
    let base = test_symbol("BaseService", SymbolKind::Class, "auth/service.php", 1);
    let child = test_symbol("AuthService", SymbolKind::Class, "auth/service.php", 30);
    db.insert_symbols(&[base.clone(), child.clone()]).unwrap();
    db.insert_edge(&Edge::new(
        &child.id,
        "App\\Auth\\BaseService",
        EdgeKind::Inherits,
        "auth/service.php",
        30,
    ))
    .unwrap();
    db.resolve_edges().unwrap();

    // Short name finds the edge only via the resolved target_id arm.
    let by_short = db.refs("BaseService", None).unwrap();
    assert_eq!(by_short.len(), 1, "short name must match via target_id");
    assert_eq!(by_short[0].0.target_id.as_ref().unwrap(), &base.id);

    // Same through the kind-filtered branch.
    let by_short_kind = db.refs("BaseService", Some(EdgeKind::Inherits)).unwrap();
    assert_eq!(by_short_kind.len(), 1);

    // A non-matching kind filter still excludes it.
    assert!(db
        .refs("BaseService", Some(EdgeKind::Calls))
        .unwrap()
        .is_empty());
}

#[test]
fn test_search_exact_match_ranks_first() {
    let db = Database::open_memory().unwrap();
    let exact = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
    let prefix = test_symbol("parse_config_file", SymbolKind::Function, "a.py", 10);
    let substr = test_symbol("get_parse_config", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[exact.clone(), prefix, substr]).unwrap();

    let results = db.search("parse_config", None, None, 20).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].name, "parse_config");
}

#[test]
fn test_search_definitions_outrank_variables() {
    let db = Database::open_memory().unwrap();
    // Variables with exact match on "token"
    let var1 = test_symbol("token", SymbolKind::Variable, "routes/auth.ts", 20);
    let var2 = test_symbol("token", SymbolKind::Variable, "routes/admin.ts", 11);
    // Class with prefix match
    let class = test_symbol("TokenError", SymbolKind::Class, "auth/tokens.ts", 14);
    // Function with substring match
    let func = test_symbol("validateToken", SymbolKind::Function, "auth/tokens.ts", 59);
    // Class with substring match
    let subclass = test_symbol("ExpiredTokenError", SymbolKind::Class, "auth/tokens.ts", 22);
    db.insert_symbols(&[var1, var2, class, func, subclass])
        .unwrap();

    let results = db.search("token", None, None, 20).unwrap();
    assert_eq!(results.len(), 5);
    // Definitions (class, function) should all rank above variables
    let def_names: Vec<&str> = results[..3].iter().map(|s| s.name.as_str()).collect();
    assert!(def_names.contains(&"TokenError"));
    assert!(def_names.contains(&"validateToken"));
    assert!(def_names.contains(&"ExpiredTokenError"));
    // Variables should be last
    assert_eq!(results[3].name, "token");
    assert_eq!(results[4].name, "token");
}

#[test]
fn test_search_prefix_match() {
    let db = Database::open_memory().unwrap();
    let a = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
    let b = test_symbol("parse_args", SymbolKind::Function, "a.py", 10);
    let c = test_symbol("unrelated", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[a, b, c]).unwrap();

    let results = db.search("parse", None, None, 20).unwrap();
    assert_eq!(results.len(), 2);
    let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"parse_config"));
    assert!(names.contains(&"parse_args"));
}

#[test]
fn test_search_substring_match() {
    let db = Database::open_memory().unwrap();
    let a = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
    let b = test_symbol("get_config", SymbolKind::Function, "a.py", 10);
    let c = test_symbol("unrelated", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[a, b, c]).unwrap();

    let results = db.search("config", None, None, 20).unwrap();
    assert_eq!(results.len(), 2);
    let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"parse_config"));
    assert!(names.contains(&"get_config"));
}

#[test]
fn test_search_case_insensitive() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym).unwrap();

    let results = db.search("Parse", None, None, 20).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "parse_config");
}

#[test]
fn test_search_kind_filter() {
    let db = Database::open_memory().unwrap();
    let func = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
    let class = test_symbol("parse_result", SymbolKind::Class, "a.py", 10);
    db.insert_symbols(&[func, class]).unwrap();

    let results = db
        .search("parse", Some(SymbolKind::Function), None, 20)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, SymbolKind::Function);
}

#[test]
fn test_search_file_filter() {
    let db = Database::open_memory().unwrap();
    let a = test_symbol("parse_config", SymbolKind::Function, "src/a.rs", 1);
    let b = test_symbol("parse_config", SymbolKind::Function, "src/b.rs", 1);
    db.insert_symbols(&[a, b]).unwrap();

    let results = db.search("parse", None, Some("src/a.rs"), 20).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, "src/a.rs");
}

#[test]
fn test_search_empty_query_returns_error() {
    let db = Database::open_memory().unwrap();
    let err = db.search("", None, None, 20).unwrap_err();
    assert!(err.to_string().contains("cannot be empty"));
}

#[test]
fn test_search_zero_limit_returns_error() {
    let db = Database::open_memory().unwrap();
    let err = db.search("parse", None, None, 0).unwrap_err();
    assert!(err.to_string().contains("at least 1"));
}

#[test]
fn test_search_limit_caps_results() {
    let db = Database::open_memory().unwrap();
    // Insert 5 symbols all matching "fn"
    for i in 0..5u32 {
        let sym = test_symbol(&format!("fn_{i}"), SymbolKind::Function, "a.py", i * 10 + 1);
        db.insert_symbol(&sym).unwrap();
    }
    let results = db.search("fn", None, None, 3).unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_search_limit_one_returns_top_ranked() {
    let db = Database::open_memory().unwrap();
    let exact = test_symbol("resolve", SymbolKind::Function, "a.py", 1);
    let prefix = test_symbol("resolve_edges", SymbolKind::Function, "a.py", 10);
    db.insert_symbols(&[exact, prefix]).unwrap();

    let results = db.search("resolve", None, None, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "resolve");
}

#[test]
fn test_search_wildcard_chars_treated_as_literals() {
    let db = Database::open_memory().unwrap();
    let sym = test_symbol("get_foo", SymbolKind::Function, "a.py", 1);
    let unrelated = test_symbol("getXfoo", SymbolKind::Function, "a.py", 10);
    db.insert_symbols(&[sym, unrelated]).unwrap();

    // "get_foo" with literal underscore should NOT match "getXfoo"
    let results = db.search("get_foo", None, None, 20).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "get_foo");
}

#[test]
fn test_search_percent_treated_as_literal() {
    let db = Database::open_memory().unwrap();
    // No symbol contains a literal %, so searching for "%" should return empty
    let sym = test_symbol("get_config", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym).unwrap();

    let results = db.search("%", None, None, 20).unwrap();
    assert!(results.is_empty(), "% should not act as a wildcard");
}
