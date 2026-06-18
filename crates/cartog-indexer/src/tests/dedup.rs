//! Tests for symbol-id dedup on hash collision.

use crate::*;

#[test]
fn test_dedup_3way_collision_preserves_invariant() {
    // Three symbols with the same stable id — simulates conditional
    // redefinitions (e.g. `if/elif/else: def foo`).
    let mk_sym = || {
        cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            2,
            0,
            10,
            None,
        )
    };
    let base_id = mk_sym().id.clone();
    let mut symbols = vec![mk_sym(), mk_sym(), mk_sym()];
    let mut edges = vec![
        cartog_core::Edge::new(
            base_id.clone(),
            "bar",
            cartog_core::EdgeKind::Calls,
            "test.py",
            1,
        ),
        cartog_core::Edge::new(
            base_id.clone(),
            "baz",
            cartog_core::EdgeKind::Calls,
            "test.py",
            2,
        ),
    ];

    dedup_symbol_ids(&mut symbols, &mut edges);

    // All three ids must now be distinct.
    let ids: std::collections::HashSet<_> = symbols.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "3-way collision should produce 3 unique ids");

    // First instance keeps the short id; 2nd and 3rd get numeric suffixes.
    assert_eq!(symbols[0].id, base_id);
    assert_eq!(symbols[1].id, format!("{base_id}:2"));
    assert_eq!(symbols[2].id, format!("{base_id}:3"));

    // Invariant: every edge.source_id must resolve to a surviving symbol.
    for edge in &edges {
        assert!(
            ids.contains(edge.source_id.as_str()),
            "edge source_id {:?} has no matching symbol after dedup",
            edge.source_id
        );
    }
}

#[test]
fn test_dedup_no_collision_leaves_ids_unchanged() {
    let mut symbols = vec![
        cartog_core::Symbol::new(
            "a",
            cartog_core::SymbolKind::Function,
            "f.py",
            1,
            2,
            0,
            10,
            None,
        ),
        cartog_core::Symbol::new(
            "b",
            cartog_core::SymbolKind::Function,
            "f.py",
            3,
            4,
            11,
            20,
            None,
        ),
    ];
    let id_a = symbols[0].id.clone();
    let id_b = symbols[1].id.clone();
    let mut edges: Vec<cartog_core::Edge> = vec![];
    dedup_symbol_ids(&mut symbols, &mut edges);
    assert_eq!(symbols[0].id, id_a);
    assert_eq!(symbols[1].id, id_b);
}
