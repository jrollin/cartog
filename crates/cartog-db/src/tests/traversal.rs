use super::test_symbol;
use crate::*;

#[test]
fn test_impact_transitive() {
    let db = Database::open_memory().unwrap();

    let a = test_symbol("a", SymbolKind::Function, "a.py", 1);
    let b = test_symbol("b", SymbolKind::Function, "b.py", 1);
    let c = test_symbol("c", SymbolKind::Function, "c.py", 1);
    db.insert_symbols(&[a.clone(), b.clone(), c.clone()])
        .unwrap();

    // b calls a, c calls b
    db.insert_edges(&[
        Edge {
            source_id: b.id.clone(),
            target_name: "a".to_string(),
            target_id: Some(a.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "b.py".to_string(),
            line: 5,
            provenance: None,
        },
        Edge {
            source_id: c.id.clone(),
            target_name: "b".to_string(),
            target_id: Some(b.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "c.py".to_string(),
            line: 5,
            provenance: None,
        },
    ])
    .unwrap();

    // Impact of "a" with depth 2 should find b (depth 1) and c (depth 2)
    let results = db.impact("a", 2).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].1, 1); // first hop
    assert_eq!(results[1].1, 2); // second hop
}

#[test]
fn test_impact_depth_zero_returns_empty() {
    let db = Database::open_memory().unwrap();
    let a = test_symbol("a", SymbolKind::Function, "a.py", 1);
    db.insert_symbols(&[a]).unwrap();
    assert!(db.impact("a", 0).unwrap().is_empty());
}

#[test]
fn test_impact_cycle_terminates() {
    // Cycle: a → b → a. impact("a", 3) must not loop forever.
    let db = Database::open_memory().unwrap();
    let a = test_symbol("a", SymbolKind::Function, "a.py", 1);
    let b = test_symbol("b", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[a.clone(), b.clone()]).unwrap();
    db.insert_edges(&[
        Edge {
            source_id: a.id.clone(),
            target_name: "b".to_string(),
            target_id: Some(b.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "a.py".to_string(),
            line: 2,
            provenance: None,
        },
        Edge {
            source_id: b.id.clone(),
            target_name: "a".to_string(),
            target_id: Some(a.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "b.py".to_string(),
            line: 2,
            provenance: None,
        },
    ])
    .unwrap();

    // Each of the two edges is returned once, labeled with its shallowest depth.
    let results = db.impact("a", 5).unwrap();
    assert_eq!(results.len(), 2);
    for (_, depth) in &results {
        assert!(*depth >= 1 && *depth <= 5);
    }
}

#[test]
fn test_impact_fanout_dedupes_by_edge() {
    // Two callers of `shared`, each also calling each other → diamond.
    // Each edge should appear once.
    let db = Database::open_memory().unwrap();
    let shared = test_symbol("shared", SymbolKind::Function, "s.py", 1);
    let x = test_symbol("x", SymbolKind::Function, "x.py", 1);
    let y = test_symbol("y", SymbolKind::Function, "y.py", 1);
    db.insert_symbols(&[shared.clone(), x.clone(), y.clone()])
        .unwrap();
    db.insert_edges(&[
        Edge {
            source_id: x.id.clone(),
            target_name: "shared".to_string(),
            target_id: Some(shared.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "x.py".to_string(),
            line: 1,
            provenance: None,
        },
        Edge {
            source_id: y.id.clone(),
            target_name: "shared".to_string(),
            target_id: Some(shared.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "y.py".to_string(),
            line: 1,
            provenance: None,
        },
        Edge {
            source_id: y.id.clone(),
            target_name: "x".to_string(),
            target_id: Some(x.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "y.py".to_string(),
            line: 2,
            provenance: None,
        },
    ])
    .unwrap();

    let results = db.impact("shared", 3).unwrap();
    // 3 distinct edges, each reported exactly once.
    assert_eq!(results.len(), 3);
}

/// Build `a → b → c → d` over `calls` edges for trace tests.
fn chain_db() -> Database {
    let db = Database::open_memory().unwrap();
    let names = ["a", "b", "c", "d"];
    let syms: Vec<Symbol> = names
        .iter()
        .map(|n| test_symbol(n, SymbolKind::Function, &format!("{n}.py"), 1))
        .collect();
    db.insert_symbols(&syms).unwrap();
    let edges: Vec<Edge> = syms
        .windows(2)
        .map(|w| Edge {
            source_id: w[0].id.clone(),
            target_name: w[1].name.clone(),
            target_id: Some(w[1].id.clone()),
            kind: EdgeKind::Calls,
            file_path: w[0].file_path.clone(),
            line: 2,
            provenance: None,
        })
        .collect();
    db.insert_edges(&edges).unwrap();
    db
}

#[test]
fn trace_returns_shortest_path_in_order() {
    let db = chain_db();
    let hops = db.trace("a", "d", 8).unwrap().expect("path a→d exists");
    let names: Vec<&str> = hops.iter().map(|h| h.source_name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert_eq!(hops.last().unwrap().target_name, "d");
}

#[test]
fn trace_returns_none_when_unreachable() {
    let db = chain_db();
    assert!(db.trace("d", "a", 8).unwrap().is_none());
}

#[test]
fn trace_same_symbol_is_empty_path() {
    let db = chain_db();
    assert_eq!(db.trace("a", "a", 8).unwrap(), Some(Vec::new()));
}

#[test]
fn trace_respects_depth_limit() {
    let db = chain_db();
    // a→d is 3 hops; depth 2 cannot reach it.
    assert!(db.trace("a", "d", 2).unwrap().is_none());
}

#[test]
fn trace_terminates_on_cycle() {
    // a → b → a. trace("a","b") returns the single hop without looping.
    let db = Database::open_memory().unwrap();
    let a = test_symbol("a", SymbolKind::Function, "a.py", 1);
    let b = test_symbol("b", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[a.clone(), b.clone()]).unwrap();
    db.insert_edges(&[
        Edge {
            source_id: a.id.clone(),
            target_name: "b".to_string(),
            target_id: Some(b.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "a.py".to_string(),
            line: 2,
            provenance: None,
        },
        Edge {
            source_id: b.id.clone(),
            target_name: "a".to_string(),
            target_id: Some(a.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "b.py".to_string(),
            line: 2,
            provenance: None,
        },
    ])
    .unwrap();
    let hops = db.trace("a", "b", 8).unwrap().expect("a→b exists");
    assert_eq!(hops.len(), 1);
}

#[test]
fn trace_dense_cycle_does_not_loop_and_finds_target() {
    // A fully-connected clique a,b,c,d (every node calls every other).
    // Without the visited-set guard, BFS would re-expand nodes endlessly /
    // explode; with it, each node is expanded once and the path to the
    // target is the direct 1-hop edge.
    let db = Database::open_memory().unwrap();
    let names = ["a", "b", "c", "d"];
    let syms: Vec<Symbol> = names
        .iter()
        .map(|n| test_symbol(n, SymbolKind::Function, &format!("{n}.py"), 1))
        .collect();
    db.insert_symbols(&syms).unwrap();
    let mut edges = Vec::new();
    for src in &syms {
        for tgt in &syms {
            if src.id != tgt.id {
                edges.push(Edge {
                    source_id: src.id.clone(),
                    target_name: tgt.name.clone(),
                    target_id: Some(tgt.id.clone()),
                    kind: EdgeKind::Calls,
                    file_path: src.file_path.clone(),
                    line: 2,
                    provenance: None,
                });
            }
        }
    }
    db.insert_edges(&edges).unwrap();
    // Direct edge a→d exists, so the shortest path is exactly one hop.
    let hops = db.trace("a", "d", 20).unwrap().expect("a reaches d");
    assert_eq!(hops.len(), 1, "shortest path in a clique is one hop");
    assert_eq!(hops[0].source_name, "a");
    assert_eq!(hops[0].target_name, "d");
}

#[test]
fn trace_unaffected_by_comma_in_symbol_ids() {
    // File paths (hence symbol ids) containing commas must not corrupt the
    // cycle guard — visited tracking is on exact ids, not a delimited string.
    let db = Database::open_memory().unwrap();
    let a = test_symbol("a", SymbolKind::Function, "a,b.py", 1);
    let b = test_symbol("b", SymbolKind::Function, "c,d.py", 1);
    let c = test_symbol("c", SymbolKind::Function, "e,f.py", 1);
    db.insert_symbols(&[a.clone(), b.clone(), c.clone()])
        .unwrap();
    db.insert_edges(&[
        Edge {
            source_id: a.id.clone(),
            target_name: "b".to_string(),
            target_id: Some(b.id.clone()),
            kind: EdgeKind::Calls,
            file_path: a.file_path.clone(),
            line: 2,
            provenance: None,
        },
        Edge {
            source_id: b.id.clone(),
            target_name: "c".to_string(),
            target_id: Some(c.id.clone()),
            kind: EdgeKind::Calls,
            file_path: b.file_path.clone(),
            line: 2,
            provenance: None,
        },
    ])
    .unwrap();
    let hops = db
        .trace("a", "c", 8)
        .unwrap()
        .expect("a→b→c despite commas");
    assert_eq!(hops.len(), 2);
    assert_eq!(hops[0].source_id, a.id);
    assert_eq!(hops[1].source_id, b.id);
}

#[test]
fn trace_hop_carries_exact_source_id_for_overloaded_name() {
    // Two symbols share the name `helper` in the same file; the hop must
    // carry the id of the symbol actually on the path, not a name lookup.
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("caller", SymbolKind::Function, "m.py", 1);
    let h1 = Symbol::new("helper", SymbolKind::Function, "m.py", 10, 12, 0, 5, None);
    let h2 = Symbol::new("helper", SymbolKind::Method, "m.py", 20, 22, 6, 11, None);
    db.insert_symbols(&[caller.clone(), h1.clone(), h2.clone()])
        .unwrap();
    // caller → the method overload (h2) specifically (resolved target_id).
    db.insert_edges(&[Edge {
        source_id: caller.id.clone(),
        target_name: "helper".to_string(),
        target_id: Some(h2.id.clone()),
        kind: EdgeKind::Calls,
        file_path: caller.file_path.clone(),
        line: 2,
        provenance: None,
    }])
    .unwrap();
    let hops = db
        .trace("caller", "helper", 8)
        .unwrap()
        .expect("caller→helper");
    assert_eq!(hops.len(), 1);
    assert_eq!(hops[0].source_id, caller.id, "hop names the exact source");
}
