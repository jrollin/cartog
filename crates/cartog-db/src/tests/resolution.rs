use super::test_symbol;
use crate::*;

#[test]
fn test_resolve_edges_same_dir_priority() {
    let db = Database::open_memory().unwrap();

    // "helper" exists in same dir (src/utils.py) and elsewhere (lib/utils.py)
    let caller = test_symbol("process", SymbolKind::Function, "src/main.py", 1);
    let same_dir = test_symbol("helper", SymbolKind::Function, "src/utils.py", 1);
    let other_dir = test_symbol("helper", SymbolKind::Function, "lib/utils.py", 1);
    db.insert_symbols(&[caller.clone(), same_dir.clone(), other_dir.clone()])
        .unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
        target_name: "helper".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "src/main.py".to_string(),
        line: 5,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    // Verify it resolved to the same-directory symbol
    let refs = db.refs("helper", None).unwrap();
    let call_edge = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Calls)
        .unwrap();
    assert_eq!(call_edge.0.target_id.as_ref().unwrap(), &same_dir.id);
}

#[test]
fn test_resolve_edges_ambiguous_no_resolve() {
    let db = Database::open_memory().unwrap();

    // "helper" in two different directories, caller in a third
    let caller = test_symbol("process", SymbolKind::Function, "app/main.py", 1);
    let sym1 = test_symbol("helper", SymbolKind::Function, "pkg_a/utils.py", 1);
    let sym2 = test_symbol("helper", SymbolKind::Function, "pkg_b/utils.py", 1);
    db.insert_symbols(&[caller.clone(), sym1, sym2]).unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
        target_name: "helper".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "app/main.py".to_string(),
        line: 5,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    // Should NOT resolve because "helper" is ambiguous (2 matches globally)
    assert_eq!(resolved, 0);
}

#[test]
fn test_resolve_edges_same_file_priority() {
    let db = Database::open_memory().unwrap();

    // "helper" in same file AND in another file
    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let same_file = test_symbol("helper", SymbolKind::Function, "a.py", 20);
    let other_file = test_symbol("helper", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[caller.clone(), same_file.clone(), other_file])
        .unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
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

    // Verify same-file symbol was chosen
    let refs = db.refs("helper", None).unwrap();
    let call_edge = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Calls)
        .unwrap();
    assert_eq!(call_edge.0.target_id.as_ref().unwrap(), &same_file.id);
}

#[test]
fn test_resolve_edges_php_fqcn_target_same_file() {
    let db = Database::open_memory().unwrap();

    // PHP emits namespace-qualified targets: `extends BaseService` inside
    // `namespace App\Auth` becomes "App\Auth\BaseService".
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

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    let refs = db.refs("App\\Auth\\BaseService", None).unwrap();
    assert_eq!(refs[0].0.target_id.as_ref().unwrap(), &base.id);
}

#[test]
fn test_resolve_edges_php_fqcn_target_prefers_class_over_import_symbol() {
    let db = Database::open_memory().unwrap();

    let class_sym = test_symbol("AppError", SymbolKind::Class, "exceptions.php", 1);
    let child = test_symbol("TokenError", SymbolKind::Class, "auth/tokens.php", 10);
    // PHP `use App\AppError;` extracts an Import symbol named by FQCN.
    let import_sym = test_symbol("App\\AppError", SymbolKind::Import, "auth/tokens.php", 1);
    db.insert_symbols(&[class_sym.clone(), child.clone(), import_sym])
        .unwrap();

    db.insert_edge(&Edge::new(
        &child.id,
        "App\\AppError",
        EdgeKind::Inherits,
        "auth/tokens.php",
        10,
    ))
    .unwrap();

    db.resolve_edges().unwrap();

    let refs = db.refs("App\\AppError", None).unwrap();
    let inherits = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Inherits)
        .unwrap();
    assert_eq!(inherits.0.target_id.as_ref().unwrap(), &class_sym.id);
}

#[test]
fn test_hierarchy_finds_children_of_fqcn_resolved_target() {
    let db = Database::open_memory().unwrap();

    let base = test_symbol("BaseService", SymbolKind::Class, "auth/service.php", 1);
    let child = test_symbol(
        "PaymentProcessor",
        SymbolKind::Class,
        "services/payment.php",
        5,
    );
    db.insert_symbols(&[base.clone(), child.clone()]).unwrap();

    db.insert_edge(&Edge::new(
        &child.id,
        "App\\Auth\\BaseService",
        EdgeKind::Inherits,
        "services/payment.php",
        5,
    ))
    .unwrap();
    db.resolve_edges().unwrap();

    let pairs = db.hierarchy("BaseService").unwrap();
    assert_eq!(
        pairs,
        vec![("PaymentProcessor".to_string(), "BaseService".to_string())]
    );
}

/// Register a file's language so hierarchy()'s C#-gated implements arm can see it.
fn register_file(db: &Database, path: &str, language: &str) {
    db.upsert_file(&FileInfo {
        path: path.to_string(),
        last_modified: 0.0,
        hash: String::new(),
        language: language.to_string(),
        num_symbols: 0,
    })
    .unwrap();
}

#[test]
fn test_hierarchy_treats_csharp_implements_to_class_as_extends() {
    // C#'s flat base_list emits Implements for a base class (D1); once resolved
    // to a Class symbol, hierarchy() must surface it as extends.
    let db = Database::open_memory().unwrap();
    register_file(&db, "Services/AuthService.cs", "csharp");

    let base = test_symbol(
        "BaseService",
        SymbolKind::Class,
        "Services/BaseService.cs",
        1,
    );
    let child = test_symbol(
        "AuthService",
        SymbolKind::Class,
        "Services/AuthService.cs",
        3,
    );
    db.insert_symbols(&[base.clone(), child.clone()]).unwrap();

    db.insert_edge(&Edge::new(
        &child.id,
        "BaseService",
        EdgeKind::Implements,
        "Services/AuthService.cs",
        3,
    ))
    .unwrap();
    db.resolve_edges().unwrap();

    let pairs = db.hierarchy("BaseService").unwrap();
    assert_eq!(
        pairs,
        vec![("AuthService".to_string(), "BaseService".to_string())]
    );
}

#[test]
fn test_hierarchy_ignores_csharp_implements_to_interface() {
    // A C# Implements edge whose resolved target is an Interface is NOT extends —
    // it must stay out of the hierarchy (only Class targets count).
    let db = Database::open_memory().unwrap();
    register_file(&db, "Services/AuthService.cs", "csharp");

    let iface = test_symbol(
        "IAuthProvider",
        SymbolKind::Interface,
        "Services/IAuthProvider.cs",
        1,
    );
    let child = test_symbol(
        "AuthService",
        SymbolKind::Class,
        "Services/AuthService.cs",
        3,
    );
    db.insert_symbols(&[iface.clone(), child.clone()]).unwrap();

    db.insert_edge(&Edge::new(
        &child.id,
        "IAuthProvider",
        EdgeKind::Implements,
        "Services/AuthService.cs",
        3,
    ))
    .unwrap();
    db.resolve_edges().unwrap();

    let pairs = db.hierarchy("IAuthProvider").unwrap();
    assert!(
        pairs.is_empty(),
        "implements-to-interface must not appear in the hierarchy, got {pairs:?}"
    );
}

#[test]
fn test_hierarchy_ignores_non_csharp_implements_to_class() {
    // Regression for the D1a Dart bug: in a non-C# language, `implements` means
    // interface conformance even when the target is a concrete class (Dart has
    // no Interface kind, so `class Foo implements AbstractBar` resolves to a
    // Class). This must NOT be reinterpreted as subclassing — only C# encodes
    // inheritance as `implements`.
    let db = Database::open_memory().unwrap();
    register_file(&db, "lib/models/user.dart", "dart");

    let base = test_symbol(
        "Repository",
        SymbolKind::Class,
        "lib/models/repository.dart",
        1,
    );
    let child = test_symbol(
        "UserRepository",
        SymbolKind::Class,
        "lib/models/user.dart",
        5,
    );
    db.insert_symbols(&[base.clone(), child.clone()]).unwrap();

    db.insert_edge(&Edge::new(
        &child.id,
        "Repository",
        EdgeKind::Implements,
        "lib/models/user.dart",
        5,
    ))
    .unwrap();
    db.resolve_edges().unwrap();

    let pairs = db.hierarchy("Repository").unwrap();
    assert!(
        pairs.is_empty(),
        "a Dart implements-to-class must not be reported as extends, got {pairs:?}"
    );
}

#[test]
fn test_hierarchy_shows_unresolved_csharp_base_class() {
    // A C# base class defined outside the indexed tree yields an unresolved
    // Implements edge (target_id NULL). It must still appear in the hierarchy
    // via target_name, matching the `inherits` fallback — otherwise C# class
    // inheritance vanishes whenever the base isn't resolved to a Class.
    let db = Database::open_memory().unwrap();
    register_file(&db, "Services/Foo.cs", "csharp");

    let child = test_symbol("Foo", SymbolKind::Class, "Services/Foo.cs", 1);
    db.insert_symbols(std::slice::from_ref(&child)).unwrap();

    // No symbol for ExternalBase → edge stays unresolved (target_id NULL).
    db.insert_edge(&Edge::new(
        &child.id,
        "ExternalBase",
        EdgeKind::Implements,
        "Services/Foo.cs",
        1,
    ))
    .unwrap();
    db.resolve_edges().unwrap();

    let pairs = db.hierarchy("ExternalBase").unwrap();
    assert_eq!(
        pairs,
        vec![("Foo".to_string(), "ExternalBase".to_string())],
        "an unresolved C# base class must still show in the hierarchy"
    );
}

#[test]
fn test_resolve_edges_class_over_constructor() {
    let db = Database::open_memory().unwrap();

    // Java pattern: Logger class + Logger() constructor method in same file
    let caller = test_symbol("handleLogin", SymbolKind::Method, "auth/Service.java", 10);
    let logger_class = test_symbol("Logger", SymbolKind::Class, "util/Logger.java", 1);
    let logger_ctor = test_symbol("Logger", SymbolKind::Method, "util/Logger.java", 5);
    db.insert_symbols(&[caller.clone(), logger_class.clone(), logger_ctor])
        .unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
        target_name: "Logger".to_string(),
        target_id: None,
        kind: EdgeKind::References,
        file_path: "auth/Service.java".to_string(),
        line: 12,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    let refs = db.refs("Logger", None).unwrap();
    let ref_edge = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::References)
        .unwrap();
    assert_eq!(ref_edge.0.target_id.as_ref().unwrap(), &logger_class.id);
}

#[test]
fn test_resolve_edges_class_over_constructor_still_ambiguous_with_three() {
    let db = Database::open_memory().unwrap();

    // Three matches: class + ctor + function — should NOT resolve
    let caller = test_symbol("main", SymbolKind::Function, "app.java", 1);
    let sym_class = test_symbol("Foo", SymbolKind::Class, "a/Foo.java", 1);
    let sym_ctor = test_symbol("Foo", SymbolKind::Method, "a/Foo.java", 5);
    let sym_func = test_symbol("Foo", SymbolKind::Function, "b/Foo.java", 1);
    db.insert_symbols(&[caller.clone(), sym_class, sym_ctor, sym_func])
        .unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
        target_name: "Foo".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "app.java".to_string(),
        line: 5,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 0);
}

#[test]
fn test_resolve_edges_multipass_import_then_call() {
    let db = Database::open_memory().unwrap();

    // File auth/service.java imports Logger from util/Logger.java
    // and also calls Logger.info() — a reference to Logger
    let import_sym = test_symbol("util.Logger", SymbolKind::Import, "auth/service.java", 1);
    let caller = test_symbol("authenticate", SymbolKind::Method, "auth/service.java", 10);
    let logger_class = test_symbol("Logger", SymbolKind::Class, "util/Logger.java", 1);
    let logger_ctor = test_symbol("Logger", SymbolKind::Method, "util/Logger.java", 5);
    db.insert_symbols(&[
        import_sym.clone(),
        caller.clone(),
        logger_class.clone(),
        logger_ctor,
    ])
    .unwrap();

    // Import edge: auth/service.java imports "Logger"
    let import_edge = Edge {
        source_id: import_sym.id.clone(),
        target_name: "Logger".to_string(),
        target_id: None,
        kind: EdgeKind::Imports,
        file_path: "auth/service.java".to_string(),
        line: 1,
        provenance: None,
    };
    db.insert_edge(&import_edge).unwrap();

    // Reference edge: authenticate() references Logger
    let ref_edge = Edge {
        source_id: caller.id.clone(),
        target_name: "Logger".to_string(),
        target_id: None,
        kind: EdgeKind::References,
        file_path: "auth/service.java".to_string(),
        line: 15,
        provenance: None,
    };
    db.insert_edge(&ref_edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    // Pass 1: import edge resolves via tier 6 (class over ctor)
    // Pass 2: reference edge resolves via tier 2 (import-path)
    assert_eq!(resolved, 2);

    let refs = db.refs("Logger", None).unwrap();
    let reference = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::References)
        .unwrap();
    assert_eq!(reference.0.target_id.as_ref().unwrap(), &logger_class.id);
}

#[test]
fn test_resolve_edges_function_over_method() {
    let db = Database::open_memory().unwrap();

    // Ruby pattern: get_logger as top-level function AND as module method
    let caller = test_symbol("process", SymbolKind::Function, "app/main.rb", 1);
    let top_fn = test_symbol("get_logger", SymbolKind::Function, "utils/helpers.rb", 6);
    let mod_method = test_symbol("get_logger", SymbolKind::Method, "utils/logging.rb", 6);
    db.insert_symbols(&[caller.clone(), top_fn.clone(), mod_method])
        .unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
        target_name: "get_logger".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "app/main.rb".to_string(),
        line: 5,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    let refs = db.refs("get_logger", None).unwrap();
    let call_edge = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Calls)
        .unwrap();
    assert_eq!(call_edge.0.target_id.as_ref().unwrap(), &top_fn.id);
}

#[test]
fn test_resolve_edges_two_functions_still_ambiguous() {
    let db = Database::open_memory().unwrap();

    // Two functions with same name in different files — should NOT resolve
    let caller = test_symbol("main", SymbolKind::Function, "app.rb", 1);
    let fn1 = test_symbol("helper", SymbolKind::Function, "a/utils.rb", 1);
    let fn2 = test_symbol("helper", SymbolKind::Function, "b/utils.rb", 1);
    db.insert_symbols(&[caller.clone(), fn1, fn2]).unwrap();

    let edge = Edge {
        source_id: caller.id.clone(),
        target_name: "helper".to_string(),
        target_id: None,
        kind: EdgeKind::Calls,
        file_path: "app.rb".to_string(),
        line: 5,
        provenance: None,
    };
    db.insert_edge(&edge).unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 0);
}

#[test]
fn test_callees_query() {
    let db = Database::open_memory().unwrap();

    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let callee1 = test_symbol("fetch", SymbolKind::Function, "b.py", 1);
    let callee2 = test_symbol("save", SymbolKind::Function, "c.py", 1);
    db.insert_symbols(&[caller.clone(), callee1, callee2])
        .unwrap();

    db.insert_edges(&[
        Edge {
            source_id: caller.id.clone(),
            target_name: "fetch".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "a.py".to_string(),
            line: 5,
            provenance: None,
        },
        Edge {
            source_id: caller.id.clone(),
            target_name: "save".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "a.py".to_string(),
            line: 6,
            provenance: None,
        },
    ])
    .unwrap();

    let callees = db.callees("process").unwrap();
    assert_eq!(callees.len(), 2);
    let targets: Vec<&str> = callees.iter().map(|e| e.target_name.as_str()).collect();
    assert!(targets.contains(&"fetch"));
    assert!(targets.contains(&"save"));
}
// ── Scoped edge resolution tests ──

#[test]
fn test_invalidate_dangling_edges_after_symbol_removal() {
    let db = Database::open_memory().unwrap();

    // File A: defines foo
    let sym_a = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym_a).unwrap();

    // File B: calls foo (edge from B to A)
    let sym_b = test_symbol("bar", SymbolKind::Function, "b.py", 1);
    db.insert_symbol(&sym_b).unwrap();
    let edge = Edge::new(&sym_b.id, "foo", EdgeKind::Calls, "b.py", 5);
    db.insert_edge(&edge).unwrap();

    // Resolve: edge should point to sym_a
    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    // Simulate: directly delete the symbol row (bypassing delete_symbol cascade)
    // to create a dangling edge reference
    db.conn
        .execute("DELETE FROM symbols WHERE id = ?1", params![sym_a.id])
        .unwrap();

    // Invalidate dangling edges
    let dirty = std::collections::HashSet::from(["a.py".to_string()]);
    let invalidated = db.invalidate_edges_targeting(&dirty).unwrap();
    assert_eq!(invalidated, 1);

    // Edge should now be unresolved
    let edges = db.callees("bar").unwrap();
    assert!(
        edges.iter().all(|e| e.target_id.is_none()),
        "edge should be unresolved after invalidation"
    );
}

#[test]
fn test_scoped_resolution_after_symbol_changes() {
    let db = Database::open_memory().unwrap();

    // File A: defines foo
    let sym_a = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    db.insert_symbol(&sym_a).unwrap();

    // File B: calls foo
    let sym_b = test_symbol("bar", SymbolKind::Function, "b.py", 1);
    db.insert_symbol(&sym_b).unwrap();
    db.insert_edge(&Edge::new(&sym_b.id, "foo", EdgeKind::Calls, "b.py", 5))
        .unwrap();

    // Resolve globally first
    db.resolve_edges().unwrap();

    // Simulate re-indexing a.py: delete_symbol nullifies edges, then re-insert
    db.delete_symbol(&sym_a.id).unwrap();
    db.insert_symbol(&sym_a).unwrap();

    // Scoped resolve should re-resolve the edge
    let dirty = std::collections::HashSet::from(["a.py".to_string()]);
    let re_resolved = db.resolve_edges_scoped(&dirty).unwrap();
    assert_eq!(re_resolved, 1);
}

#[test]
fn test_compute_in_degrees_scoped() {
    let db = Database::open_memory().unwrap();

    let foo = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let bar = test_symbol("bar", SymbolKind::Function, "b.py", 1);
    let baz = test_symbol("baz", SymbolKind::Function, "c.py", 1);
    db.insert_symbol(&foo).unwrap();
    db.insert_symbol(&bar).unwrap();
    db.insert_symbol(&baz).unwrap();

    // bar calls foo, baz calls foo
    db.insert_edge(&Edge::new(&bar.id, "foo", EdgeKind::Calls, "b.py", 5))
        .unwrap();
    db.insert_edge(&Edge::new(&baz.id, "foo", EdgeKind::Calls, "c.py", 3))
        .unwrap();

    db.resolve_edges().unwrap();
    db.compute_in_degrees().unwrap();

    // foo should have in_degree = 2
    let results = db.search("foo", None, None, 10).unwrap();
    assert_eq!(results[0].in_degree, 2);

    // Now scope to just b.py
    let dirty = std::collections::HashSet::from(["b.py".to_string()]);
    db.compute_in_degrees_scoped(&dirty).unwrap();

    // foo should still have in_degree = 2 (recomputed correctly)
    let results = db.search("foo", None, None, 10).unwrap();
    assert_eq!(results[0].in_degree, 2);
}

#[test]
fn test_tier2_import_resolution_plan_uses_kind_target_index() {
    // Plan regression for #109; SQL mirrors tier-2 in store/resolution.rs.
    let db = Database::open_memory().unwrap();
    let mut stmt = db
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT s.id FROM symbols s
             INNER JOIN edges ie ON ie.kind = 'imports' AND ie.target_name = ?1
                 AND ie.target_id IS NOT NULL
             INNER JOIN symbols is2 ON is2.id = ie.source_id AND is2.file_path = ?2
             INNER JOIN symbols resolved ON resolved.id = ie.target_id
             WHERE s.name = ?1 AND s.kind != 'import'
                 AND s.file_path = resolved.file_path
             LIMIT 1",
        )
        .unwrap();
    let plan = stmt
        .query_map(params!["x", "y"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        plan.contains("idx_edges_kind_target"),
        "tier-2 must drive off edges(kind, target_name); got plan:\n{plan}"
    );
}

#[test]
fn test_refs_plan_uses_multi_index_or_not_full_scan() {
    // Plan regression: both refs() branches must resolve via a MULTI-INDEX OR
    // over the edge target indexes, never the old `OR sym2.name` full scan.
    let db = Database::open_memory().unwrap();
    // Populate + ANALYZE: a zero-row DB collapses every plan to a kind-only
    // scan, hiding the target bound. Selective target_names + half-resolved
    // target_ids make both MULTI-INDEX OR arms the cheapest plan.
    let syms: Vec<Symbol> = (0..400)
        .map(|i| test_symbol(&format!("s{i}"), SymbolKind::Function, "a.py", i))
        .collect();
    db.insert_symbols(&syms).unwrap();
    let edges: Vec<Edge> = (0..400)
        .map(|i| {
            let mut e = Edge::new(
                &syms[i as usize].id,
                format!("t{i}"),
                EdgeKind::Calls,
                "a.py",
                i,
            );
            if i % 2 == 0 {
                e.target_id = Some(syms[i as usize].id.clone());
            }
            e
        })
        .collect();
    db.insert_edges(&edges).unwrap();
    db.conn.execute_batch("ANALYZE;").unwrap();

    let explain = |sql: &str| -> String {
        let mut stmt = db.conn.prepare(sql).unwrap();
        stmt.query_map(params!["x"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
    };
    let assert_no_edge_scan = |plan: &str, ctx: &str| {
        // Core invariant: edges reached by an index, never the old OR-join scan.
        assert!(
            !plan.contains("SCAN e\n") && !plan.ends_with("SCAN e") && !plan.contains("SCAN edges"),
            "refs() {ctx} must not full-scan edges; got plan:\n{plan}"
        );
    };

    // Unfiltered branch: MULTI-INDEX OR. Assert the full EQP detail (trailing
    // `(target_name=` / `(target_id=`) so `idx_edges_target` isn't subsumed
    // by the `idx_edges_target_id` prefix.
    let unfiltered = explain(
        "EXPLAIN QUERY PLAN
         SELECT e.id FROM edges e
         LEFT JOIN symbols s ON e.source_id = s.id
         WHERE e.target_name = ?1
            OR e.target_id IN (SELECT id FROM symbols WHERE name = ?1)",
    );
    assert!(
        unfiltered.contains("MULTI-INDEX OR"),
        "refs() unfiltered must use a multi-index OR; got plan:\n{unfiltered}"
    );
    assert!(
        unfiltered.contains("idx_edges_target (target_name="),
        "refs() literal arm must seek idx_edges_target on target_name; got plan:\n{unfiltered}"
    );
    assert!(
        unfiltered.contains("idx_edges_target_id (target_id="),
        "refs() resolved arm must seek idx_edges_target_id on target_id; got plan:\n{unfiltered}"
    );
    assert_no_edge_scan(&unfiltered, "unfiltered");

    // Kind-filtered branch: kind pushed into each OR arm so both stay
    // target-bounded (composite idx_edges_kind_target + idx_edges_target_id).
    let kind_filtered = explain(
        "EXPLAIN QUERY PLAN
         SELECT e.id FROM edges e
         LEFT JOIN symbols s ON e.source_id = s.id
         WHERE (e.target_name = ?1 AND e.kind = 'calls')
            OR (e.target_id IN (SELECT id FROM symbols WHERE name = ?1)
                AND e.kind = 'calls')",
    );
    assert!(
        kind_filtered.contains("MULTI-INDEX OR"),
        "refs() kind-filtered must use a multi-index OR; got plan:\n{kind_filtered}"
    );
    assert!(
        kind_filtered.contains("idx_edges_kind_target (kind=? AND target_name="),
        "refs() kind-filtered literal arm must seek (kind, target_name); got plan:\n{kind_filtered}"
    );
    assert!(
        kind_filtered.contains("idx_edges_target_id (target_id="),
        "refs() kind-filtered resolved arm must seek target_id; got plan:\n{kind_filtered}"
    );
    assert_no_edge_scan(&kind_filtered, "kind-filtered");
}

#[test]
fn test_impact_recursive_step_avoids_full_edge_scan() {
    // Plan regression: the impact() recursive step must reach edges through
    // indexes, never a full SCAN + correlated subquery. The old
    // `JOIN edges e ON (e.target_name = i.source_name OR EXISTS(...))` form
    // scanned all edges per frontier row (~310ms at d2 on a real repo);
    // splitting the OR into two recursive arms keeps each on an index seek
    // (idx_edges_target and idx_edges_target_id). SQL mirrors impact().
    let db = Database::open_memory().unwrap();
    let mut stmt = db
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN
             WITH RECURSIVE impacted(edge_id, source_id, target_name, target_id,
                 kind, file_path, line, resolution_source, source_name, depth) AS (
                 SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind,
                        e.file_path, e.line, e.resolution_source, s.name, 1
                 FROM edges e LEFT JOIN symbols s ON e.source_id = s.id
                 WHERE e.target_name = ?1
                    OR e.target_id IN (SELECT id FROM symbols WHERE name = ?1)
                 UNION
                 SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind,
                        e.file_path, e.line, e.resolution_source, s.name, i.depth + 1
                 FROM impacted i
                 JOIN edges e ON e.target_name = i.source_name
                 LEFT JOIN symbols s ON e.source_id = s.id
                 WHERE i.source_name IS NOT NULL AND i.depth < ?2
                 UNION
                 SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind,
                        e.file_path, e.line, e.resolution_source, s.name, i.depth + 1
                 FROM impacted i
                 JOIN symbols t ON t.name = i.source_name
                 JOIN edges e ON e.target_id = t.id
                 LEFT JOIN symbols s ON e.source_id = s.id
                 WHERE i.source_name IS NOT NULL AND i.depth < ?2)
             SELECT source_id, MIN(depth) FROM impacted GROUP BY edge_id
             ORDER BY depth, edge_id",
        )
        .unwrap();
    let plan = stmt
        .query_map(params!["x", 3], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    // Assert on the full EQP detail (with the trailing `(target_name=?)` /
    // `(target_id=?)`): a bare `contains("idx_edges_target")` is subsumed by
    // `idx_edges_target_id` (prefix), so it would pass even if the literal
    // arm regressed to a scan. The literal arm must seek idx_edges_target on
    // target_name; the resolved arm must seek idx_edges_target_id.
    assert!(
        plan.contains("idx_edges_target (target_name="),
        "impact() literal arm must seek idx_edges_target on target_name; got plan:\n{plan}"
    );
    assert!(
        plan.contains("idx_edges_target_id (target_id="),
        "impact() resolved arm must seek idx_edges_target_id on target_id; got plan:\n{plan}"
    );
    assert!(
        !plan.contains("CORRELATED"),
        "impact() must not run a correlated subquery per edge; got plan:\n{plan}"
    );
    // Direct anti-scan guard (mirrors the refs() plan test): neither
    // recursive arm may full-scan edges. `SCAN i` over the small frontier
    // is fine; a `SCAN e`/`SCAN edges` is the regression.
    assert!(
        !plan.contains("SCAN e\n") && !plan.ends_with("SCAN e") && !plan.contains("SCAN edges"),
        "impact() must not full-scan edges; got plan:\n{plan}"
    );
}

#[test]
fn test_per_file_edge_delete_uses_file_index() {
    // Plan regression: clear_file_data_in_tx's DELETE FROM edges WHERE
    // file_path=? must use an index, not full-scan. A scan makes
    // --force/first-index O(files×edges) (the per-file-clear quadratic).
    let db = Database::open_memory().unwrap();
    let mut stmt = db
        .conn
        .prepare("EXPLAIN QUERY PLAN DELETE FROM edges WHERE file_path = ?1")
        .unwrap();
    let plan = stmt
        .query_map(params!["a.py"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        plan.contains("idx_edges_file"),
        "per-file edge delete must drive off edges(file_path); got plan:\n{plan}"
    );
}

#[test]
fn test_compute_in_degrees_plan_has_no_correlated_subquery() {
    // Plan regression: the in-degree UPDATE must materialize counts once and
    // join by PK, not re-scan it per row (correlated subquery → O(symbols×edges)).
    let db = Database::open_memory().unwrap();
    let mut stmt = db
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN
             UPDATE symbols SET in_degree = counts.cnt
             FROM (
                 SELECT target_id, COUNT(*) AS cnt
                 FROM edges WHERE target_id IS NOT NULL
                 GROUP BY target_id
             ) AS counts
             WHERE symbols.id = counts.target_id",
        )
        .unwrap();
    let plan = stmt
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        !plan.to_uppercase().contains("CORRELATED"),
        "in-degree UPDATE must not use a correlated subquery; got plan:\n{plan}"
    );
}

#[test]
fn test_compute_in_degrees_scoped_resets_target_that_lost_edge() {
    let db = Database::open_memory().unwrap();

    let foo = test_symbol("foo", SymbolKind::Function, "a.py", 1);
    let bar = test_symbol("bar", SymbolKind::Function, "b.py", 1);
    let baz = test_symbol("baz", SymbolKind::Function, "c.py", 1);
    db.insert_symbol(&foo).unwrap();
    db.insert_symbol(&bar).unwrap();
    db.insert_symbol(&baz).unwrap();

    // bar calls foo, baz calls foo
    db.insert_edge(&Edge::new(&bar.id, "foo", EdgeKind::Calls, "b.py", 5))
        .unwrap();
    db.insert_edge(&Edge::new(&baz.id, "foo", EdgeKind::Calls, "c.py", 3))
        .unwrap();

    db.resolve_edges().unwrap();
    db.compute_in_degrees().unwrap();
    let results = db.search("foo", None, None, 10).unwrap();
    assert_eq!(results[0].in_degree, 2);

    // Re-index b.py with the call removed: the indexer clears the file's
    // old edges before the scoped recompute, so foo (unchanged a.py) has
    // already lost an incoming edge by the time the recompute runs.
    db.clear_edges_for_file("b.py").unwrap();
    let dirty = std::collections::HashSet::from(["b.py".to_string()]);
    db.invalidate_edges_targeting(&dirty).unwrap();
    db.resolve_edges_scoped(&dirty).unwrap();
    db.compute_in_degrees_scoped(&dirty).unwrap();

    let results = db.search("foo", None, None, 10).unwrap();
    assert_eq!(results[0].in_degree, 1);
}

// ── `::`-qualified target reduction (Rust path separator) ──
// The resolver tries the full target_name (split only on `.`/`\`) first, then a
// `::`-reduced name through the locality tiers (1–4) only. So `Pool::new` gains a
// tier-1..4 match on `new` (Rust stores callees bare), a namespaced Ruby symbol
// stored under its full `Baz::Quux` name still matches on the full-name pass, and
// a fully-qualified external call `std::mem::swap` stays unresolved (reduced `swap`
// is never offered to the global tier-5). The `.`/`\` splits are unchanged (see
// test_resolve_edges_same_file_priority for `.` and
// test_resolve_edges_php_fqcn_target_same_file for `\`).

fn call_edge_target(refs: &[(Edge, Option<Symbol>)]) -> &Edge {
    &refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Calls)
        .expect("expected a Calls edge")
        .0
}

#[test]
fn scoped_call_resolves_via_colon_split() {
    let db = Database::open_memory().unwrap();

    // Rust: `Pool::new(...)` is emitted as target_name "Pool::new"; the callee's
    // own name is `new`, defined in the same file (tier 1, via the reduced name).
    let caller = test_symbol("connect", SymbolKind::Function, "src/db.rs", 10);
    let new_method = test_symbol("new", SymbolKind::Method, "src/db.rs", 1);
    db.insert_symbols(&[caller.clone(), new_method.clone()])
        .unwrap();

    db.insert_edge(&Edge::new(
        &caller.id,
        "Pool::new",
        EdgeKind::Calls,
        "src/db.rs",
        12,
    ))
    .unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    let refs = db.refs("Pool::new", None).unwrap();
    let call = call_edge_target(&refs);
    assert_eq!(call.target_id.as_ref().unwrap(), &new_method.id);
    assert_eq!(call.provenance, Some(EdgeProvenance::SameFile));
}

#[test]
fn scoped_call_resolves_unique_global() {
    let db = Database::open_memory().unwrap();

    // A single project-wide `load` outside the caller's dir subtree (so tiers 1–4
    // miss the reduced name): with the full name `Config::load` unmatched, resolution
    // stays unresolved — the reduced `load` is deliberately NOT offered to tier 5.
    // Contrast scoped_call_resolves_via_colon_split, where locality lets it resolve.
    let caller = test_symbol("main", SymbolKind::Function, "app/main.rs", 1);
    let load = test_symbol("load", SymbolKind::Method, "config/mod.rs", 20);
    db.insert_symbols(&[caller.clone(), load.clone()]).unwrap();

    db.insert_edge(&Edge::new(
        &caller.id,
        "Config::load",
        EdgeKind::Calls,
        "app/main.rs",
        5,
    ))
    .unwrap();

    // No locality and no full-name global match → unresolved (guards against a
    // reduced-name false positive at the global tier, i.e. finding-2).
    assert_eq!(db.resolve_edges().unwrap(), 0);
}

#[test]
fn scoped_call_resolves_unique_global_by_full_name() {
    let db = Database::open_memory().unwrap();

    // A Ruby-style symbol stored under its FULL `::`-qualified name resolves via the
    // unchanged full-name global tier (5). No locality, single global match.
    let caller = test_symbol("main", SymbolKind::Function, "app/main.rs", 1);
    let helper = test_symbol("Mod::Helper", SymbolKind::Class, "lib/mod/helper.rb", 1);
    db.insert_symbols(&[caller.clone(), helper.clone()])
        .unwrap();

    db.insert_edge(&Edge::new(
        &caller.id,
        "Mod::Helper",
        EdgeKind::References,
        "app/main.rs",
        5,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 1);

    let refs = db.refs("Mod::Helper", None).unwrap();
    let reference = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::References)
        .unwrap();
    assert_eq!(reference.0.target_id.as_ref().unwrap(), &helper.id);
    assert_eq!(reference.0.provenance, Some(EdgeProvenance::UniqueGlobal));
}

#[test]
fn ruby_namespaced_inherits_resolves_via_full_name() {
    let db = Database::open_memory().unwrap();

    // Ruby stores `class Foo::Bar < Baz::Quux` with symbol names `Foo::Bar` /
    // `Baz::Quux` and an Inherits edge target_name "Baz::Quux" (ruby.rs
    // extract_constant_name keeps the whole scope_resolution). Full-name-first
    // resolution must match the symbol literally named `Baz::Quux`; reducing to
    // `Quux` (the pre-fix regression) would miss it entirely.
    let child = test_symbol("Foo::Bar", SymbolKind::Class, "app/models/foo/bar.rb", 1);
    let parent = test_symbol("Baz::Quux", SymbolKind::Class, "app/models/baz/quux.rb", 1);
    db.insert_symbols(&[child.clone(), parent.clone()]).unwrap();

    db.insert_edge(&Edge::new(
        &child.id,
        "Baz::Quux",
        EdgeKind::Inherits,
        "app/models/foo/bar.rb",
        1,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 1);

    let refs = db.refs("Baz::Quux", None).unwrap();
    let inherits = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Inherits)
        .unwrap();
    assert_eq!(inherits.0.target_id.as_ref().unwrap(), &parent.id);
}

#[test]
fn fully_qualified_external_call_stays_unresolved() {
    let db = Database::open_memory().unwrap();

    // `std::mem::swap(...)` is emitted verbatim as target_name "std::mem::swap".
    // A lone unrelated `swap` exists project-wide but NOT local to the caller.
    // Reducing to `swap` and resolving via global tier-5 would fabricate a call
    // edge into the wrong symbol; the reduced name must not reach the global tier.
    let caller = test_symbol("run", SymbolKind::Function, "app/game.rs", 1);
    let unrelated = test_symbol("swap", SymbolKind::Method, "lib/board/mod.rs", 10);
    db.insert_symbols(&[caller.clone(), unrelated]).unwrap();

    db.insert_edge(&Edge::new(
        &caller.id,
        "std::mem::swap",
        EdgeKind::Calls,
        "app/game.rs",
        5,
    ))
    .unwrap();

    // Full name "std::mem::swap" matches nothing; reduced "swap" is only tried at
    // locality tiers (none match here), never at the global tier → unresolved.
    assert_eq!(db.resolve_edges().unwrap(), 0);
}

#[test]
fn common_scoped_name_stays_unresolved_when_ambiguous() {
    let db = Database::open_memory().unwrap();

    // Three `new` symbols, each in its own directory distinct from the caller's
    // (no locality): `Vec::new`'s reduced `new` is only offered to tiers 1–4 (all
    // miss), never to the global tier, so it stays unresolved — no false-positive
    // edge from the reduction.
    let caller = test_symbol("run", SymbolKind::Function, "app/main.rs", 1);
    let n1 = test_symbol("new", SymbolKind::Method, "pkg_a/mod.rs", 1);
    let n2 = test_symbol("new", SymbolKind::Method, "pkg_b/mod.rs", 1);
    let n3 = test_symbol("new", SymbolKind::Method, "pkg_c/mod.rs", 1);
    db.insert_symbols(&[caller.clone(), n1, n2, n3]).unwrap();

    let edge = Edge::new(&caller.id, "Vec::new", EdgeKind::Calls, "app/main.rs", 5);
    db.insert_edge(&edge).unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 0);
}

#[test]
fn scoped_reduced_name_prefers_same_file_over_other_dir() {
    let db = Database::open_memory().unwrap();

    // Two `render` methods: one same-file with the caller, one elsewhere. The
    // reduced name from `View::render` must resolve via the caller's own file
    // (tier 1), not mis-attribute to the unrelated same-named method — locking the
    // guardless tier-1 path against reduced-name over-matching (finding-4).
    let caller = test_symbol("show", SymbolKind::Function, "ui/page.rs", 10);
    let same_file = test_symbol("render", SymbolKind::Method, "ui/page.rs", 1);
    let other = test_symbol("render", SymbolKind::Method, "gfx/canvas.rs", 1);
    db.insert_symbols(&[caller.clone(), same_file.clone(), other])
        .unwrap();

    db.insert_edge(&Edge::new(
        &caller.id,
        "View::render",
        EdgeKind::Calls,
        "ui/page.rs",
        12,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 1);

    let refs = db.refs("View::render", None).unwrap();
    let call = call_edge_target(&refs);
    assert_eq!(call.target_id.as_ref().unwrap(), &same_file.id);
    assert_eq!(call.provenance, Some(EdgeProvenance::SameFile));
}

#[test]
fn nested_path_reduces_to_final_segment() {
    let db = Database::open_memory().unwrap();

    // Multi-segment `::` path with no `.`: only the `::` reduction isolates `build`.
    // On main the `.`/`\` split leaves "a::b::build" whole (unresolved), so this
    // fails without the fix. Resolves via the reduced name at tier 1.
    let caller = test_symbol("driver", SymbolKind::Function, "src/lib.rs", 10);
    let build = test_symbol("build", SymbolKind::Method, "src/lib.rs", 1);
    db.insert_symbols(&[caller.clone(), build.clone()]).unwrap();

    db.insert_edge(&Edge::new(
        &caller.id,
        "a::b::build",
        EdgeKind::Calls,
        "src/lib.rs",
        12,
    ))
    .unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    let refs = db.refs("a::b::build", None).unwrap();
    let call = call_edge_target(&refs);
    assert_eq!(call.target_id.as_ref().unwrap(), &build.id);
}

#[test]
fn dot_then_colon_reduce_to_final_segment() {
    let db = Database::open_memory().unwrap();

    // "a.b::method": full_name splits on `.` → "b::method" (unresolved on main),
    // then the `::` reduction yields `method`, which resolves at tier 1.
    let caller = test_symbol("caller", SymbolKind::Function, "src/svc.rs", 10);
    let method = test_symbol("method", SymbolKind::Method, "src/svc.rs", 1);
    db.insert_symbols(&[caller.clone(), method.clone()])
        .unwrap();

    db.insert_edge(&Edge::new(
        &caller.id,
        "a.b::method",
        EdgeKind::Calls,
        "src/svc.rs",
        12,
    ))
    .unwrap();

    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1);

    let refs = db.refs("a.b::method", None).unwrap();
    let call = call_edge_target(&refs);
    assert_eq!(call.target_id.as_ref().unwrap(), &method.id);
}

// ── SFC component symbols at the global tiers ──

/// `resolution_state` of the single edge in the DB.
fn only_edge_state(db: &Database) -> i64 {
    db.conn
        .query_row("SELECT resolution_state FROM edges", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn component_import_resolves_via_unique_global() {
    let db = Database::open_memory().unwrap();

    // A lone `LoginForm` component in an unrelated directory: tiers 1-4 miss, tier 5 hits.
    let importer = test_symbol("App", SymbolKind::Component, "app/App.vue", 1);
    let component = test_symbol(
        "LoginForm",
        SymbolKind::Component,
        "shared/ui/LoginForm.vue",
        1,
    );
    db.insert_symbols(&[importer.clone(), component.clone()])
        .unwrap();
    db.insert_edge(&Edge::new(
        &importer.id,
        "LoginForm",
        EdgeKind::Imports,
        "app/App.vue",
        2,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 1);

    let refs = db.refs("LoginForm", None).unwrap();
    let import = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Imports)
        .unwrap();
    assert_eq!(import.0.target_id.as_ref().unwrap(), &component.id);
    assert_eq!(import.0.provenance, Some(EdgeProvenance::UniqueGlobal));
    assert_eq!(only_edge_state(&db), 1);
}

#[test]
fn component_and_variable_same_name_stay_unresolved() {
    let db = Database::open_memory().unwrap();

    // Both kinds carry kind_priority 0, so tier 6 cannot break the tie.
    let importer = test_symbol("App", SymbolKind::Component, "src/App.vue", 1);
    let component = test_symbol("Badge", SymbolKind::Component, "ui/Badge.vue", 1);
    let variable = test_symbol("Badge", SymbolKind::Variable, "lib/consts.ts", 3);
    db.insert_symbols(&[importer.clone(), component, variable])
        .unwrap();
    db.insert_edge(&Edge::new(
        &importer.id,
        "Badge",
        EdgeKind::Imports,
        "src/App.vue",
        2,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 0);
    assert_eq!(only_edge_state(&db), 0);
}

#[test]
fn component_and_class_same_name_resolves_to_class() {
    let db = Database::open_memory().unwrap();

    // Tier 6: class (priority 3) beats component (priority 0).
    let importer = test_symbol("App", SymbolKind::Component, "src/App.vue", 1);
    let component = test_symbol("Badge", SymbolKind::Component, "ui/Badge.vue", 1);
    let class = test_symbol("Badge", SymbolKind::Class, "lib/badge.ts", 3);
    db.insert_symbols(&[importer.clone(), component, class.clone()])
        .unwrap();
    db.insert_edge(&Edge::new(
        &importer.id,
        "Badge",
        EdgeKind::Imports,
        "src/App.vue",
        2,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 1);

    let refs = db.refs("Badge", None).unwrap();
    let import = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Imports)
        .unwrap();
    assert_eq!(import.0.target_id.as_ref().unwrap(), &class.id);
    assert_eq!(import.0.provenance, Some(EdgeProvenance::KindDisambig));
}

#[test]
fn two_components_same_name_stay_unresolved() {
    let db = Database::open_memory().unwrap();

    let importer = test_symbol("App", SymbolKind::Component, "src/App.vue", 1);
    let a = test_symbol("Badge", SymbolKind::Component, "pkg_a/Badge.vue", 1);
    let b = test_symbol("Badge", SymbolKind::Component, "pkg_b/Badge.vue", 1);
    db.insert_symbols(&[importer.clone(), a, b]).unwrap();
    db.insert_edge(&Edge::new(
        &importer.id,
        "Badge",
        EdgeKind::Imports,
        "src/App.vue",
        2,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 0);
    assert_eq!(only_edge_state(&db), 0);
}

#[test]
fn same_stem_component_does_not_shadow_a_cross_file_target() {
    let db = Database::open_memory().unwrap();

    // `Widget.vue` importing a `Widget` helper: the file's own component symbol
    // must not win any tier, or the real cross-file target is lost.
    let component = test_symbol("Widget", SymbolKind::Component, "src/ui/Widget.vue", 1);
    let helper = test_symbol("Widget", SymbolKind::Function, "src/lib/helpers.ts", 3);
    db.insert_symbols(&[component.clone(), helper.clone()])
        .unwrap();
    db.insert_edge(&Edge::new(
        &component.id,
        "Widget",
        EdgeKind::Imports,
        "src/ui/Widget.vue",
        2,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 1);

    let refs = db.refs("Widget", None).unwrap();
    let import = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Imports)
        .unwrap();
    assert_eq!(import.0.target_id.as_ref().unwrap(), &helper.id);
}

#[test]
fn same_stem_component_in_same_dir_does_not_shadow_a_cross_file_target() {
    let db = Database::open_memory().unwrap();

    // Tier 3 scans the whole directory, so the exclusion must be file-scoped,
    // not tier-1-only: a sibling `Badge` function in the same dir still wins.
    let component = test_symbol("Badge", SymbolKind::Component, "src/ui/Badge.vue", 1);
    let sibling = test_symbol("Badge", SymbolKind::Function, "src/ui/badge_util.ts", 4);
    db.insert_symbols(&[component.clone(), sibling.clone()])
        .unwrap();
    db.insert_edge(&Edge::new(
        &component.id,
        "Badge",
        EdgeKind::Calls,
        "src/ui/Badge.vue",
        3,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 1);

    let refs = db.refs("Badge", None).unwrap();
    let call = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Calls)
        .unwrap();
    assert_eq!(call.0.target_id.as_ref().unwrap(), &sibling.id);
}

#[test]
fn same_stem_component_leaves_an_external_import_unresolved() {
    let db = Database::open_memory().unwrap();

    // `Table.vue` importing `Table` from a node_modules package: nothing in the
    // project matches, so the edge must stay unresolved for the LSP pass rather
    // than become a false self-edge.
    let component = test_symbol("Table", SymbolKind::Component, "src/ui/Table.vue", 1);
    db.insert_symbols(std::slice::from_ref(&component)).unwrap();
    db.insert_edge(&Edge::new(
        &component.id,
        "Table",
        EdgeKind::Imports,
        "src/ui/Table.vue",
        2,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 0);
    assert_eq!(only_edge_state(&db), 0);
}
