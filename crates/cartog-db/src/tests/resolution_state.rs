use super::test_symbol;
use crate::*;

// ── resolution_state (edge marker) tests ──

fn resolution_state_of(db: &Database, edge_id: i64) -> i64 {
    db.conn
        .query_row(
            "SELECT resolution_state FROM edges WHERE id = ?1",
            params![edge_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn resolution_source_of(db: &Database, edge_id: i64) -> Option<String> {
    db.conn
        .query_row(
            "SELECT resolution_source FROM edges WHERE id = ?1",
            params![edge_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn insert_test_edge(db: &Database, target_name: &str) -> i64 {
    let sym = test_symbol("src", SymbolKind::Function, "a.py", 1);
    db.insert_symbols(std::slice::from_ref(&sym)).unwrap();
    let edge = Edge::new(&sym.id, target_name, EdgeKind::Calls, "a.py", 1);
    db.insert_edge(&edge).unwrap();
    db.conn.last_insert_rowid()
}

#[test]
fn test_new_edge_has_default_state_zero() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "missing_target");
    assert_eq!(resolution_state_of(&db, id), 0);
}

#[test]
fn test_update_edge_target_flips_state_to_one() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "anything");
    db.update_edge_target(id, "some:symbol:id").unwrap();
    assert_eq!(resolution_state_of(&db, id), 1);
}

#[test]
fn test_mark_edge_unresolvable_sets_state_to_two() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "anything");
    db.mark_edge_unresolvable(id).unwrap();
    assert_eq!(resolution_state_of(&db, id), 2);
}

#[test]
fn test_unresolved_edges_excludes_state_two() {
    let db = Database::open_memory().unwrap();
    let _unresolved = insert_test_edge(&db, "still_unresolved");
    let burned = insert_test_edge(&db, "burned");
    db.mark_edge_unresolvable(burned).unwrap();

    let edges = db.unresolved_edges().unwrap();
    let names: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
    assert!(names.contains(&"still_unresolved"));
    assert!(!names.contains(&"burned"));
}

#[test]
fn test_reset_unresolvable_for_names_targets_only_matching() {
    let db = Database::open_memory().unwrap();
    let burned_foo = insert_test_edge(&db, "foo");
    let burned_bar = insert_test_edge(&db, "bar");
    db.mark_edge_unresolvable(burned_foo).unwrap();
    db.mark_edge_unresolvable(burned_bar).unwrap();

    let reopened = db
        .reset_unresolvable_for_names(&["foo".to_string()])
        .unwrap();
    assert_eq!(reopened, 1);
    assert_eq!(resolution_state_of(&db, burned_foo), 0);
    assert_eq!(resolution_state_of(&db, burned_bar), 2);
}

#[test]
fn test_reset_unresolvable_for_names_empty_is_noop() {
    let db = Database::open_memory().unwrap();
    let n = db.reset_unresolvable_for_names(&[]).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn test_reset_unresolvable_for_names_does_not_touch_state_zero_or_one() {
    // The reset reopens state {2, 3} → state=0. Resolved (state=1) and
    // already-open (state=0) edges with matching names must be left alone.
    let db = Database::open_memory().unwrap();
    let still_open = insert_test_edge(&db, "foo"); // state=0
    let already_resolved = insert_test_edge(&db, "foo");
    db.update_edge_target(already_resolved, "some:id").unwrap(); // state=1

    db.reset_unresolvable_for_names(&["foo".to_string()])
        .unwrap();
    assert_eq!(resolution_state_of(&db, still_open), 0);
    assert_eq!(resolution_state_of(&db, already_resolved), 1);
}

#[test]
fn test_mark_edge_external_sets_state_to_three() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "anything");
    db.mark_edge_external(id).unwrap();
    assert_eq!(resolution_state_of(&db, id), 3);
    assert_eq!(db.edge_resolution_state(id).unwrap(), 3);
}

#[test]
fn test_unresolved_edges_excludes_state_three() {
    // External (state=3) edges must be skipped by the LSP retry loop, same
    // as state=2 — otherwise we re-query dep targets on every dirty run.
    let db = Database::open_memory().unwrap();
    let _open = insert_test_edge(&db, "still_open");
    let ext = insert_test_edge(&db, "external_dep");
    db.mark_edge_external(ext).unwrap();

    let edges = db.unresolved_edges().unwrap();
    let names: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
    assert!(names.contains(&"still_open"));
    assert!(!names.contains(&"external_dep"));
}

#[test]
fn test_reset_all_unresolvable_resets_state_two_and_three() {
    // `cartog index --force` must clear BOTH definitive markers (2 and 3)
    // so a forced re-index honors the "retry everything" contract.
    let db = Database::open_memory().unwrap();
    let burned = insert_test_edge(&db, "burned");
    let external = insert_test_edge(&db, "external");
    db.mark_edge_unresolvable(burned).unwrap();
    db.mark_edge_external(external).unwrap();

    let reset = db.reset_all_unresolvable().unwrap();
    assert_eq!(reset, 2);
    assert_eq!(resolution_state_of(&db, burned), 0);
    assert_eq!(resolution_state_of(&db, external), 0);
}

#[test]
fn test_reset_unresolvable_for_names_reopens_state_three() {
    // External edges must also reopen when a matching symbol is added —
    // this is the "vendored dependency in-tree" path.
    let db = Database::open_memory().unwrap();
    let ext_foo = insert_test_edge(&db, "foo");
    let ext_bar = insert_test_edge(&db, "bar");
    db.mark_edge_external(ext_foo).unwrap();
    db.mark_edge_external(ext_bar).unwrap();

    let reopened = db
        .reset_unresolvable_for_names(&["foo".to_string()])
        .unwrap();
    assert_eq!(reopened, 1);
    assert_eq!(resolution_state_of(&db, ext_foo), 0);
    assert_eq!(resolution_state_of(&db, ext_bar), 3);
}

// ── state=4 (heuristic-exhausted) tests ──

#[test]
fn test_mark_heuristic_exhausted_seals_unresolved_state_zero() {
    // Edges the heuristic couldn't resolve (state=0, target NULL) flip to
    // state=4 so the next re-index's resolution scan skips them.
    let db = Database::open_memory().unwrap();
    let unresolved = insert_test_edge(&db, "nowhere");
    let resolved = insert_test_edge(&db, "somewhere");
    db.update_edge_target(resolved, "some:id").unwrap();

    let marked = db.mark_heuristic_exhausted_in_tx().unwrap();
    assert_eq!(marked, 1);
    assert_eq!(resolution_state_of(&db, unresolved), 4);
    assert_eq!(resolution_state_of(&db, resolved), 1, "resolved untouched");
}

#[test]
fn test_count_edges_in_state_buckets_by_state() {
    let db = Database::open_memory().unwrap();
    let resolved = insert_test_edge(&db, "somewhere");
    db.update_edge_target(resolved, "some:id").unwrap();
    let burned = insert_test_edge(&db, "burned");
    db.mark_edge_unresolvable(burned).unwrap();

    assert_eq!(db.count_edges_in_state(0).unwrap(), 0);
    assert_eq!(db.count_edges_in_state(1).unwrap(), 1);
    assert_eq!(db.count_edges_in_state(2).unwrap(), 1);
}

#[test]
fn test_has_heuristic_exhausted_tracks_state_four() {
    let db = Database::open_memory().unwrap();
    let _edge = insert_test_edge(&db, "nowhere");
    assert!(!db.has_heuristic_exhausted().unwrap(), "state 0 not sealed");
    db.mark_heuristic_exhausted_in_tx().unwrap();
    assert!(db.has_heuristic_exhausted().unwrap());
}

#[test]
fn test_resolve_edges_skips_heuristic_exhausted_state_four() {
    // The state=0-only scan in resolve_edges_pass must not re-walk sealed
    // state=4 edges — this is the watch-mode amplification guard (#109).
    let db = Database::open_memory().unwrap();
    let eid = insert_test_edge(&db, "nowhere");
    db.mark_heuristic_exhausted_in_tx().unwrap();
    assert_eq!(resolution_state_of(&db, eid), 4);

    // A fresh resolve pass finds nothing to do and leaves the seal intact.
    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 0);
    assert_eq!(resolution_state_of(&db, eid), 4);
}

#[test]
fn test_unresolved_edges_excludes_state_four() {
    // The LSP retry loop must skip state=4 too, same as {2, 3}. The blanket
    // mark seals every open edge, so insert the still-open one afterward.
    let db = Database::open_memory().unwrap();
    let exhausted = insert_test_edge(&db, "exhausted");
    db.mark_heuristic_exhausted_in_tx().unwrap();
    let _open = insert_test_edge(&db, "still_open");

    let edges = db.unresolved_edges().unwrap();
    let names: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
    assert!(names.contains(&"still_open"));
    assert!(!names.contains(&"exhausted"));
    let _ = exhausted;
}

#[test]
fn test_reopen_heuristic_exhausted_resets_only_state_four() {
    // Before an LSP-enabled reindex, state=4 → 0, but genuine LSP verdicts
    // (state {2, 3}) stay sealed.
    let db = Database::open_memory().unwrap();
    let exhausted = insert_test_edge(&db, "exhausted");
    db.mark_heuristic_exhausted_in_tx().unwrap();
    let burned = insert_test_edge(&db, "burned");
    db.mark_edge_unresolvable(burned).unwrap();
    let external = insert_test_edge(&db, "external");
    db.mark_edge_external(external).unwrap();

    let reopened = db.reopen_heuristic_exhausted().unwrap();
    assert_eq!(reopened, 1);
    assert_eq!(resolution_state_of(&db, exhausted), 0);
    assert_eq!(resolution_state_of(&db, burned), 2, "LSP verdict sealed");
    assert_eq!(resolution_state_of(&db, external), 3, "LSP verdict sealed");
}

#[test]
fn test_reset_all_unresolvable_also_resets_state_four() {
    // --force must clear state=4 alongside {2, 3}.
    let db = Database::open_memory().unwrap();
    let exhausted = insert_test_edge(&db, "exhausted");
    db.mark_heuristic_exhausted_in_tx().unwrap();
    let burned = insert_test_edge(&db, "burned");
    db.mark_edge_unresolvable(burned).unwrap();

    let reset = db.reset_all_unresolvable().unwrap();
    assert_eq!(reset, 2);
    assert_eq!(resolution_state_of(&db, exhausted), 0);
    assert_eq!(resolution_state_of(&db, burned), 0);
}

#[test]
fn test_reset_unresolvable_for_names_reopens_state_four() {
    // A heuristic-exhausted edge reopens when a matching symbol is added.
    let db = Database::open_memory().unwrap();
    let foo = insert_test_edge(&db, "foo");
    let bar = insert_test_edge(&db, "bar");
    db.mark_heuristic_exhausted_in_tx().unwrap();

    let reopened = db
        .reset_unresolvable_for_names(&["foo".to_string()])
        .unwrap();
    assert_eq!(reopened, 1);
    assert_eq!(resolution_state_of(&db, foo), 0);
    assert_eq!(resolution_state_of(&db, bar), 4);
}

#[test]
fn test_stats_surfaces_external_and_unresolvable_counts() {
    let db = Database::open_memory().unwrap();
    let resolved = insert_test_edge(&db, "resolved_target");
    db.update_edge_target(resolved, "some:id").unwrap();
    let burned = insert_test_edge(&db, "burned");
    db.mark_edge_unresolvable(burned).unwrap();
    let external = insert_test_edge(&db, "external");
    db.mark_edge_external(external).unwrap();
    let _open = insert_test_edge(&db, "open");

    let stats = db.stats().unwrap();
    assert_eq!(stats.num_resolved, 1);
    assert_eq!(stats.num_unresolvable, 1);
    assert_eq!(stats.num_external, 1);
    assert_eq!(stats.num_edges, 4);
}

#[test]
fn test_invalidate_edges_targeting_resets_state_when_target_disappears() {
    // When a symbol referenced by a resolved edge is removed, the edge
    // must drop back to (target_id NULL, state=0) so it re-enters the
    // unresolved set on the next pass.
    let db = Database::open_memory().unwrap();

    // Set up: source edge points to symbol "ghost" via update_edge_target,
    // then drop the symbol so the edge becomes dangling.
    let src = test_symbol("src", SymbolKind::Function, "a.py", 1);
    let target = test_symbol("ghost", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[src.clone(), target.clone()]).unwrap();
    let edge = Edge::new(&src.id, "ghost", EdgeKind::Calls, "a.py", 1);
    db.insert_edge(&edge).unwrap();
    let eid = db.conn.last_insert_rowid();
    db.update_edge_target(eid, &target.id).unwrap();
    assert_eq!(resolution_state_of(&db, eid), 1);

    // Remove the target symbol — leaves edge.target_id pointing at nothing.
    db.conn
        .execute("DELETE FROM symbols WHERE id = ?1", params![target.id])
        .unwrap();

    let mut dirty = std::collections::HashSet::new();
    dirty.insert("b.py".to_string());
    db.invalidate_edges_targeting(&dirty).unwrap();

    assert_eq!(
        resolution_state_of(&db, eid),
        0,
        "dangling edge must return to state=0 so unresolved_edges() can see it"
    );
    let row: Option<String> = db
        .conn
        .query_row(
            "SELECT target_id FROM edges WHERE id = ?1",
            params![eid],
            |r| r.get(0),
        )
        .unwrap();
    assert!(row.is_none(), "target_id must be NULL after invalidation");
}

#[test]
fn test_delete_symbol_resets_state_on_dangling_incoming_edges() {
    // Regression for the "(target_id=NULL, state=1) zombie" bug: when a
    // resolved target symbol is deleted, every edge pointing to it must
    // drop back to state=0 — otherwise the edge becomes invisible to both
    // unresolved_edges() (state=1 filter) and graph traversal (NULL target).
    let db = Database::open_memory().unwrap();
    let src = test_symbol("caller", SymbolKind::Function, "a.py", 1);
    let target = test_symbol("ghost", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[src.clone(), target.clone()]).unwrap();
    let edge = Edge::new(&src.id, "ghost", EdgeKind::Calls, "a.py", 1);
    db.insert_edge(&edge).unwrap();
    let eid = db.conn.last_insert_rowid();
    db.update_edge_target(eid, &target.id).unwrap();

    db.delete_symbol(&target.id).unwrap();

    assert_eq!(resolution_state_of(&db, eid), 0);
    assert_eq!(resolution_source_of(&db, eid), None, "stale tag must clear");
    let visible = db
        .unresolved_edges()
        .unwrap()
        .iter()
        .any(|e| e.edge_id == eid);
    assert!(
        visible,
        "orphaned edge must resurface in unresolved_edges()"
    );
}

#[test]
fn test_delete_symbols_in_tx_resets_state_on_dangling_incoming_edges() {
    // Same invariant as test_delete_symbol_..., for the batched path used
    // by the indexer's Merkle-diff `removed` set.
    let db = Database::open_memory().unwrap();
    let src = test_symbol("caller", SymbolKind::Function, "a.py", 1);
    let t1 = test_symbol("ghost1", SymbolKind::Function, "b.py", 1);
    let t2 = test_symbol("ghost2", SymbolKind::Function, "c.py", 1);
    db.insert_symbols(&[src.clone(), t1.clone(), t2.clone()])
        .unwrap();
    let e1 = Edge::new(&src.id, "ghost1", EdgeKind::Calls, "a.py", 1);
    db.insert_edge(&e1).unwrap();
    let eid1 = db.conn.last_insert_rowid();
    db.update_edge_target(eid1, &t1.id).unwrap();
    let e2 = Edge::new(&src.id, "ghost2", EdgeKind::Calls, "a.py", 2);
    db.insert_edge(&e2).unwrap();
    let eid2 = db.conn.last_insert_rowid();
    db.update_edge_target(eid2, &t2.id).unwrap();

    assert_eq!(resolution_source_of(&db, eid1).as_deref(), Some("lsp"));

    db.delete_symbols(&[t1.id.clone(), t2.id.clone()]).unwrap();

    assert_eq!(resolution_state_of(&db, eid1), 0);
    assert_eq!(resolution_state_of(&db, eid2), 0);
    // Deleting the target unresolves the edge; its provenance must clear too,
    // else refs/callees report a stale tier for an edge pointing nowhere.
    assert_eq!(resolution_source_of(&db, eid1), None);
    assert_eq!(resolution_source_of(&db, eid2), None);
}

#[test]
fn test_heuristic_resolve_flips_state_to_one() {
    // Regression: resolve_edge_batch's UPDATE must set state=1 alongside
    // target_id. Otherwise heuristically-resolved edges stay state=0 and
    // get re-queried by LSP on the next pass — pure waste.
    let db = Database::open_memory().unwrap();
    let src = test_symbol("caller", SymbolKind::Function, "a.py", 1);
    let target = test_symbol("foo", SymbolKind::Function, "a.py", 10);
    db.insert_symbols(&[src.clone(), target.clone()]).unwrap();
    let edge = Edge::new(&src.id, "foo", EdgeKind::Calls, "a.py", 2);
    db.insert_edge(&edge).unwrap();
    let eid = db.conn.last_insert_rowid();
    assert_eq!(resolution_state_of(&db, eid), 0);

    db.resolve_edges().unwrap();

    assert_eq!(
        resolution_state_of(&db, eid),
        1,
        "heuristic resolve must set state=1 so LSP doesn't re-attack the edge"
    );
    assert!(
        db.unresolved_edges()
            .unwrap()
            .iter()
            .all(|e| e.edge_id != eid),
        "resolved edge must drop out of unresolved_edges()"
    );
}

#[test]
fn test_partial_unresolved_index_exists() {
    // The partial index speeds up the unresolved_edges() query on large
    // repos. Verify it actually got created by inspecting sqlite_master.
    let db = Database::open_memory().unwrap();
    let n: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='index' AND name='idx_edges_unresolved'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn test_resolution_state_default_via_insert_edges_batch() {
    // The batched insert path is the production hot path. Make sure
    // it honors the DEFAULT 0 just like single-row inserts do.
    let db = Database::open_memory().unwrap();
    let src = test_symbol("src", SymbolKind::Function, "a.py", 1);
    db.insert_symbols(std::slice::from_ref(&src)).unwrap();
    let edges = vec![
        Edge::new(&src.id, "x", EdgeKind::Calls, "a.py", 1),
        Edge::new(&src.id, "y", EdgeKind::Calls, "a.py", 2),
    ];
    db.insert_edges(&edges).unwrap();
    let states: Vec<i64> = db
        .conn
        .prepare("SELECT resolution_state FROM edges ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(states, vec![0, 0]);
}

#[test]
fn test_migration_v3_to_v4_backfills_resolved_to_state_one() {
    // Simulate a pre-v4 database: open with v3-equivalent schema (no
    // resolution_state column, schema_version=3), insert edges with
    // and without target_ids, then re-open to trigger the migration.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("v3.sqlite");

    {
        let conn = Connection::open(&path).unwrap();
        // Bootstrap a v3-shaped edges table by hand.
        conn.execute_batch(
            "CREATE TABLE symbols (
                id TEXT PRIMARY KEY, name TEXT, kind TEXT, file_path TEXT,
                start_line INTEGER, end_line INTEGER, start_byte INTEGER, end_byte INTEGER,
                parent_id TEXT, signature TEXT, visibility TEXT, is_async BOOLEAN,
                docstring TEXT, in_degree INTEGER DEFAULT 0,
                content_hash TEXT, subtree_hash TEXT);
             CREATE TABLE edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id TEXT NOT NULL, target_name TEXT NOT NULL, target_id TEXT,
                kind TEXT NOT NULL, file_path TEXT NOT NULL, line INTEGER);
             CREATE TABLE files (path TEXT PRIMARY KEY);
             CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '3');
             INSERT INTO symbols (id, name, kind, file_path) VALUES ('s:1', 'foo', 'function', 'a.py');
             INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line)
               VALUES ('s:1', 'foo', 's:1', 'calls', 'a.py', 1);
             INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line)
               VALUES ('s:1', 'missing', NULL, 'calls', 'a.py', 2);",
        )
        .unwrap();
    }

    // Re-open through the production path so migrate() runs the full ladder.
    let db = Database::open(&path, DEFAULT_EMBEDDING_DIM).unwrap();

    // The v3→4 migration adds the resolution_state column (schema transform);
    // verify it is present and queryable. The v7 stable-ID-escaping migration
    // clears the seeded rows, so the per-row backfill is no longer observable
    // after a full chain — assert the durable column + cleared-index contract.
    let has_resolution_state = db
        .conn
        .prepare("SELECT resolution_state FROM edges LIMIT 0")
        .is_ok();
    assert!(has_resolution_state, "v3→4 added resolution_state column");

    let edge_count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(edge_count, 0, "v7 cleared the index for full rebuild");

    let bumped: String = db
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bumped, SCHEMA_VERSION.to_string());
}

// ── Edge provenance ──

/// Resolve a single `name`-targeting `calls` edge and return the provenance
/// the resolver tagged on it. The caller wires up the symbol graph so a
/// specific tier wins.
fn resolve_one_and_get_provenance(db: &Database, name: &str) -> Option<EdgeProvenance> {
    let resolved = db.resolve_edges().unwrap();
    assert_eq!(resolved, 1, "expected exactly one edge to resolve");
    let refs = db.refs(name, None).unwrap();
    refs.into_iter()
        .find(|(e, _)| e.target_id.is_some())
        .and_then(|(e, _)| e.provenance)
}

#[test]
fn resolve_tags_provenance_same_file() {
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let same_file = test_symbol("helper", SymbolKind::Function, "a.py", 20);
    let other_file = test_symbol("helper", SymbolKind::Function, "b.py", 1);
    db.insert_symbols(&[caller.clone(), same_file, other_file])
        .unwrap();
    db.insert_edge(&Edge::new(&caller.id, "helper", EdgeKind::Calls, "a.py", 5))
        .unwrap();

    assert_eq!(
        resolve_one_and_get_provenance(&db, "helper"),
        Some(EdgeProvenance::SameFile)
    );
}

#[test]
fn resolve_tags_provenance_same_dir() {
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("process", SymbolKind::Function, "pkg/a.py", 1);
    let same_dir = test_symbol("helper", SymbolKind::Function, "pkg/b.py", 1);
    let far = test_symbol("helper", SymbolKind::Function, "other/c.py", 1);
    db.insert_symbols(&[caller.clone(), same_dir, far]).unwrap();
    db.insert_edge(&Edge::new(
        &caller.id,
        "helper",
        EdgeKind::Calls,
        "pkg/a.py",
        5,
    ))
    .unwrap();

    assert_eq!(
        resolve_one_and_get_provenance(&db, "helper"),
        Some(EdgeProvenance::SameDir)
    );
}

#[test]
fn resolve_tags_provenance_unique_global() {
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let target = test_symbol("only_one", SymbolKind::Function, "far/away.py", 1);
    db.insert_symbols(&[caller.clone(), target]).unwrap();
    db.insert_edge(&Edge::new(
        &caller.id,
        "only_one",
        EdgeKind::Calls,
        "a.py",
        5,
    ))
    .unwrap();

    assert_eq!(
        resolve_one_and_get_provenance(&db, "only_one"),
        Some(EdgeProvenance::UniqueGlobal)
    );
}

#[test]
fn resolve_tags_provenance_kind_disambig() {
    let db = Database::open_memory().unwrap();
    // Two global matches: a class beats the constructor method (tier 6).
    let caller = test_symbol("handleLogin", SymbolKind::Method, "auth/Service.java", 10);
    let logger_class = test_symbol("Logger", SymbolKind::Class, "util/Logger.java", 1);
    let logger_ctor = test_symbol("Logger", SymbolKind::Method, "util/Logger.java", 5);
    db.insert_symbols(&[caller.clone(), logger_class, logger_ctor])
        .unwrap();
    db.insert_edge(&Edge::new(
        &caller.id,
        "Logger",
        EdgeKind::References,
        "auth/Service.java",
        12,
    ))
    .unwrap();

    db.resolve_edges().unwrap();
    let refs = db.refs("Logger", None).unwrap();
    let edge = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::References)
        .unwrap();
    assert_eq!(edge.0.provenance, Some(EdgeProvenance::KindDisambig));
}

#[test]
fn resolve_tags_provenance_parent_scope() {
    // Tier 4 only fires when same-file/import/same-dir miss and there are
    // multiple global matches, one sharing the caller's parent scope. Build
    // two `helper`s in different dirs from the caller's file, both children
    // of the same parent as the caller, so only parent-scope disambiguates.
    let db = Database::open_memory().unwrap();
    let mut caller = test_symbol("run", SymbolKind::Method, "app/svc.py", 10);
    caller.parent_id = Some("app/svc.py:class:Svc".to_string());
    let mut same_scope = test_symbol("helper", SymbolKind::Method, "lib/a.py", 1);
    same_scope.parent_id = Some("app/svc.py:class:Svc".to_string());
    let mut other_scope = test_symbol("helper", SymbolKind::Method, "lib/b.py", 1);
    other_scope.parent_id = Some("other/x.py:class:Other".to_string());
    db.insert_symbols(&[caller.clone(), same_scope.clone(), other_scope])
        .unwrap();
    db.insert_edge(&Edge::new(
        &caller.id,
        "helper",
        EdgeKind::Calls,
        "app/svc.py",
        12,
    ))
    .unwrap();

    assert_eq!(
        resolve_one_and_get_provenance(&db, "helper"),
        Some(EdgeProvenance::ParentScope)
    );
}

#[test]
fn callees_surfaces_provenance() {
    // Read-back coverage for the callees() path (uses the shared row_to_edge).
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let same_file = test_symbol("helper", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[caller.clone(), same_file]).unwrap();
    db.insert_edge(&Edge::new(&caller.id, "helper", EdgeKind::Calls, "a.py", 5))
        .unwrap();
    db.resolve_edges().unwrap();

    let callees = db.callees("process").unwrap();
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].provenance, Some(EdgeProvenance::SameFile));
}

#[test]
fn impact_surfaces_provenance() {
    // Read-back coverage for the impact() CTE mapper (depth at index 7,
    // provenance at index 6).
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let target = test_symbol("helper", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[caller.clone(), target]).unwrap();
    db.insert_edge(&Edge::new(&caller.id, "helper", EdgeKind::Calls, "a.py", 5))
        .unwrap();
    db.resolve_edges().unwrap();

    let impact = db.impact("helper", 3).unwrap();
    let call = impact
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::Calls)
        .unwrap();
    assert_eq!(call.0.provenance, Some(EdgeProvenance::SameFile));
}

#[test]
fn reset_unresolvable_for_names_clears_provenance() {
    // The per-reindex reopen path (indexer calls this on every incremental
    // run) must clear the stale LSP tag, not just the state.
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "foo");
    db.mark_edge_unresolvable(id).unwrap();
    assert_eq!(
        resolution_source_of(&db, id).as_deref(),
        Some("lsp_unresolvable")
    );

    let reopened = db
        .reset_unresolvable_for_names(&["foo".to_string()])
        .unwrap();
    assert_eq!(reopened, 1);
    assert_eq!(resolution_source_of(&db, id), None, "stale tag cleared");
}

#[test]
fn insert_edge_round_trips_provenance() {
    // A reconstructed (already-resolved) edge persists its provenance through
    // insert and reads back identically.
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let target = test_symbol("helper", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[caller.clone(), target.clone()])
        .unwrap();
    let mut edge = Edge::new(&caller.id, "helper", EdgeKind::Calls, "a.py", 5);
    edge.target_id = Some(target.id.clone());
    edge.provenance = Some(EdgeProvenance::Lsp);
    db.insert_edge(&edge).unwrap();
    let eid = db.conn.last_insert_rowid();

    let callees = db.callees("process").unwrap();
    assert_eq!(callees[0].provenance, Some(EdgeProvenance::Lsp));
    // An inserted edge that already has a target must persist resolution_state=1,
    // not the column default 0 — else stats()/unresolved_edges() misreport it.
    assert_eq!(resolution_state_of(&db, eid), 1);
    assert!(
        !db.unresolved_edges()
            .unwrap()
            .iter()
            .any(|e| e.edge_id == eid),
        "a resolved insert must not resurface as unresolved"
    );
}

#[test]
fn insert_edge_without_target_is_unresolved() {
    // The extraction path inserts edges with no target_id; they must land at
    // resolution_state=0 so resolve_edges()/LSP pick them up.
    let db = Database::open_memory().unwrap();
    let src = test_symbol("src", SymbolKind::Function, "a.py", 1);
    db.insert_symbols(std::slice::from_ref(&src)).unwrap();
    db.insert_edge(&Edge::new(&src.id, "missing", EdgeKind::Calls, "a.py", 1))
        .unwrap();
    let eid = db.conn.last_insert_rowid();
    assert_eq!(resolution_state_of(&db, eid), 0);
}

#[test]
fn resolve_tags_provenance_import_path() {
    // Two-pass: the import edge resolves in pass 1 (tier 6, class over ctor),
    // then the reference in the importing file resolves via import-path in
    // pass 2. Mirrors test_resolve_edges_multipass_import_then_call.
    let db = Database::open_memory().unwrap();
    let import_sym = test_symbol("util.Logger", SymbolKind::Import, "auth/service.java", 1);
    let caller = test_symbol("authenticate", SymbolKind::Method, "auth/service.java", 10);
    let logger_class = test_symbol("Logger", SymbolKind::Class, "util/Logger.java", 1);
    let logger_ctor = test_symbol("Logger", SymbolKind::Method, "util/Logger.java", 5);
    db.insert_symbols(&[
        import_sym.clone(),
        caller.clone(),
        logger_class,
        logger_ctor,
    ])
    .unwrap();
    db.insert_edge(&Edge::new(
        &import_sym.id,
        "Logger",
        EdgeKind::Imports,
        "auth/service.java",
        1,
    ))
    .unwrap();
    db.insert_edge(&Edge::new(
        &caller.id,
        "Logger",
        EdgeKind::References,
        "auth/service.java",
        15,
    ))
    .unwrap();

    assert_eq!(db.resolve_edges().unwrap(), 2);
    let refs = db.refs("Logger", None).unwrap();
    let reference = refs
        .iter()
        .find(|(e, _)| e.kind == EdgeKind::References)
        .unwrap();
    assert_eq!(reference.0.provenance, Some(EdgeProvenance::ImportPath));
}

#[test]
fn lsp_resolve_tags_provenance_lsp() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "anything");
    db.update_edge_target(id, "some:symbol:id").unwrap();
    assert_eq!(resolution_source_of(&db, id).as_deref(), Some("lsp"));
}

#[test]
fn lsp_overwrite_retags_heuristic_as_lsp() {
    let db = Database::open_memory().unwrap();
    let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
    let same_file = test_symbol("helper", SymbolKind::Function, "a.py", 20);
    db.insert_symbols(&[caller.clone(), same_file.clone()])
        .unwrap();
    db.insert_edge(&Edge::new(&caller.id, "helper", EdgeKind::Calls, "a.py", 5))
        .unwrap();
    db.resolve_edges().unwrap();

    let edge_id: i64 = db
        .conn
        .query_row("SELECT id FROM edges LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        resolution_source_of(&db, edge_id).as_deref(),
        Some("same_file")
    );

    db.update_edge_target(edge_id, &same_file.id).unwrap();
    assert_eq!(resolution_source_of(&db, edge_id).as_deref(), Some("lsp"));
}

#[test]
fn mark_external_tags_lsp_external() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "anything");
    db.mark_edge_external(id).unwrap();
    assert_eq!(
        resolution_source_of(&db, id).as_deref(),
        Some("lsp_external")
    );
}

#[test]
fn mark_unresolvable_tags_lsp_unresolvable() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "anything");
    db.mark_edge_unresolvable(id).unwrap();
    assert_eq!(
        resolution_source_of(&db, id).as_deref(),
        Some("lsp_unresolvable")
    );
}

#[test]
fn reset_unresolvable_clears_provenance() {
    let db = Database::open_memory().unwrap();
    let id = insert_test_edge(&db, "foo");
    db.mark_edge_external(id).unwrap();
    assert_eq!(
        resolution_source_of(&db, id).as_deref(),
        Some("lsp_external")
    );

    db.reset_all_unresolvable().unwrap();
    assert_eq!(resolution_source_of(&db, id), None, "stale tag cleared");
}

/// Bootstrap a pre-v6-shaped DB at `path`: edges have `resolution_state` but
/// no `resolution_source` column, stamped at `schema_version`. Shared by the
/// migration tests so both exercise the same "old" shape.
fn bootstrap_pre_v6_db(path: &std::path::Path, schema_version: u32, seed_edges: bool) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE symbols (
            id TEXT PRIMARY KEY, name TEXT, kind TEXT, file_path TEXT,
            start_line INTEGER, end_line INTEGER, start_byte INTEGER, end_byte INTEGER,
            parent_id TEXT, signature TEXT, visibility TEXT, is_async BOOLEAN,
            docstring TEXT, in_degree INTEGER DEFAULT 0,
            content_hash TEXT, subtree_hash TEXT);
         CREATE TABLE edges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id TEXT NOT NULL, target_name TEXT NOT NULL, target_id TEXT,
            kind TEXT NOT NULL, file_path TEXT NOT NULL, line INTEGER,
            resolution_state INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE files (path TEXT PRIMARY KEY);
         CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE query_log (id INTEGER PRIMARY KEY AUTOINCREMENT,
            tool TEXT NOT NULL, source TEXT NOT NULL, ts INTEGER NOT NULL);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('schema_version', ?1)",
        params![schema_version.to_string()],
    )
    .unwrap();
    if seed_edges {
        conn.execute_batch(
            "INSERT INTO symbols (id, name, kind, file_path) VALUES ('s:1', 'foo', 'function', 'a.py');
             INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line, resolution_state)
               VALUES ('s:1', 'foo', 's:1', 'calls', 'a.py', 1, 1);
             INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line, resolution_state)
               VALUES ('s:1', 'missing', NULL, 'calls', 'a.py', 2, 0);",
        )
        .unwrap();
    }
}

#[test]
fn migration_v5_to_v6_adds_resolution_source_column() {
    // A pre-v6 DB (resolution_state present, resolution_source absent) gains
    // the resolution_source column on open. The v7 stable-ID-escaping
    // migration then clears the seeded rows, so assert the durable column +
    // cleared-index contract rather than the now-wiped per-row backfill.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("v5.sqlite");
    bootstrap_pre_v6_db(&path, 5, true);

    let db = Database::open(&path, DEFAULT_EMBEDDING_DIM).unwrap();

    let has_resolution_source = db
        .conn
        .prepare("SELECT resolution_source FROM edges LIMIT 0")
        .is_ok();
    assert!(has_resolution_source, "v5→6 added resolution_source column");

    let edge_count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(edge_count, 0, "v7 cleared the index for full rebuild");

    let bumped: String = db
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bumped, SCHEMA_VERSION.to_string());
}

#[test]
fn migration_v6_self_heals_missing_column() {
    // schema_version says 6 but the column is absent (partial-migration
    // crash). The probe guard must re-add it on open.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("partial.sqlite");
    bootstrap_pre_v6_db(&path, 6, false);

    let db = Database::open(&path, DEFAULT_EMBEDDING_DIM).unwrap();
    let has_col = db
        .conn
        .prepare("SELECT resolution_source FROM edges LIMIT 0")
        .is_ok();
    assert!(has_col, "missing resolution_source column was re-added");
}

/// Bootstrap a v6-shaped DB (all columns present, stamped at v6) with one
/// seeded row in every table the v7 wipe clears, plus a `last_commit`, so the
/// wipe is observable per table. `symbol_content` uses the real shape and the
/// FTS5 vtable + insert/delete triggers so its row inserts (and the wipe's
/// delete) keep the external-content index consistent.
fn bootstrap_v6_db(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE symbols (
            id TEXT PRIMARY KEY, name TEXT, kind TEXT, file_path TEXT,
            start_line INTEGER, end_line INTEGER, start_byte INTEGER, end_byte INTEGER,
            parent_id TEXT, signature TEXT, visibility TEXT, is_async BOOLEAN,
            docstring TEXT, in_degree INTEGER DEFAULT 0,
            content_hash TEXT, subtree_hash TEXT);
         CREATE TABLE edges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id TEXT NOT NULL, target_name TEXT NOT NULL, target_id TEXT,
            kind TEXT NOT NULL, file_path TEXT NOT NULL, line INTEGER,
            resolution_state INTEGER NOT NULL DEFAULT 0, resolution_source TEXT);
         CREATE TABLE files (path TEXT PRIMARY KEY);
         CREATE TABLE symbol_content (
            symbol_id TEXT PRIMARY KEY, content TEXT NOT NULL, header TEXT NOT NULL,
            normalized_name TEXT NOT NULL DEFAULT '');
         CREATE VIRTUAL TABLE symbol_fts USING fts5(
            symbol_name, normalized_name, content,
            content=symbol_content, content_rowid=rowid);
         CREATE TRIGGER symbol_content_ai AFTER INSERT ON symbol_content BEGIN
            INSERT INTO symbol_fts(rowid, symbol_name, normalized_name, content)
            VALUES (new.rowid, (SELECT name FROM symbols WHERE id = new.symbol_id),
                    new.normalized_name, new.content);
         END;
         CREATE TRIGGER symbol_content_ad AFTER DELETE ON symbol_content BEGIN
            INSERT INTO symbol_fts(symbol_fts, rowid, symbol_name, normalized_name, content)
            VALUES ('delete', old.rowid, (SELECT name FROM symbols WHERE id = old.symbol_id),
                    old.normalized_name, old.content);
         END;
         CREATE TABLE symbol_embedding_map (symbol_id TEXT NOT NULL);
         CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE query_log (id INTEGER PRIMARY KEY AUTOINCREMENT,
            tool TEXT NOT NULL, source TEXT NOT NULL, ts INTEGER NOT NULL);
         INSERT INTO symbols (id, name, kind, file_path) VALUES ('a.py:import:os.path', 'os.path', 'import', 'a.py');
         INSERT INTO files (path) VALUES ('a.py');
         INSERT INTO edges (source_id, target_name, kind, file_path, line)
            VALUES ('a.py:import:os.path', 'os', 'imports', 'a.py', 1);
         INSERT INTO symbol_content (symbol_id, content, header)
            VALUES ('a.py:import:os.path', 'body', 'sig');
         INSERT INTO symbol_embedding_map (symbol_id) VALUES ('a.py:import:os.path');
         INSERT INTO metadata (key, value) VALUES ('schema_version', '6');
         INSERT INTO metadata (key, value) VALUES ('last_commit', 'deadbeef');",
    )
    .unwrap();
}

#[test]
fn migration_v6_to_v7_clears_index_for_full_rebuild() {
    // The v7 symbol-ID escaping changes the ID format, so old (collidable)
    // rows must be wiped and last_commit cleared so the next index rebuilds
    // every file from scratch.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("v6.sqlite");
    bootstrap_v6_db(&path);

    let db = Database::open(&path, DEFAULT_EMBEDDING_DIM).unwrap();

    let count = |table: &str| -> i64 {
        db.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count("symbols"), 0, "symbols cleared");
    assert_eq!(count("edges"), 0, "edges cleared");
    assert_eq!(count("files"), 0, "files cleared");
    assert_eq!(count("symbol_content"), 0, "symbol_content cleared");
    assert_eq!(
        count("symbol_embedding_map"),
        0,
        "symbol_embedding_map cleared"
    );

    let last_commit: Option<String> = db
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'last_commit'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(
        last_commit, None,
        "last_commit cleared to force full reindex"
    );

    let bumped: String = db
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bumped, SCHEMA_VERSION.to_string());

    // The v7 wipe is destructive, so the pre-migration DB must be backed up
    // first — same safety contract as the v2→3 wipe.
    let backups = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("v6.sqlite.pre-v")
        })
        .count();
    assert_eq!(backups, 1, "v6→7 wipe must back up the index first");
}

#[test]
fn read_metadata_at_returns_value_when_present() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.set_metadata("last_commit", "abc1234").unwrap();
    }
    assert_eq!(
        read_metadata_at(&db_path, "last_commit").unwrap(),
        Some("abc1234".to_string())
    );
}

#[test]
fn read_metadata_at_returns_none_when_row_absent() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    // A freshly opened cartog DB has a metadata table but no last_commit row.
    let _db = Database::open(&db_path, 384).unwrap();
    assert_eq!(read_metadata_at(&db_path, "last_commit").unwrap(), None);
}

/// The three `EMBED_*_KEY` constants are `pub` so out-of-crate readers name
/// the same rows this crate writes. A typo in one is invisible to every
/// in-crate caller (they all use the constant), so pin each against the row
/// the store actually produces — reached the way an out-of-crate reader would,
/// via `read_metadata_at` on a closed file.
#[test]
fn embed_dimension_key_names_the_row_the_store_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    // Database::open writes the dimension via handle_embedding_dimension,
    // whose SQL inlines the literal rather than using the constant.
    let _db = Database::open(&db_path, 384).unwrap();

    assert_eq!(
        read_metadata_at(&db_path, EMBED_DIMENSION_KEY).unwrap(),
        Some("384".to_string()),
        "EMBED_DIMENSION_KEY must match the literal handle_embedding_dimension writes"
    );
}

#[test]
fn embed_provider_and_model_keys_name_the_rows_the_store_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.reconcile_embedding_fingerprint(&EmbeddingFingerprint {
            provider: "local".to_string(),
            model: "BAAI/bge-small-en-v1.5".to_string(),
            dimension: 384,
        })
        .unwrap();
    }

    assert_eq!(
        read_metadata_at(&db_path, EMBED_PROVIDER_KEY).unwrap(),
        Some("local".to_string())
    );
    assert_eq!(
        read_metadata_at(&db_path, EMBED_MODEL_KEY).unwrap(),
        Some("BAAI/bge-small-en-v1.5".to_string())
    );
}

#[test]
fn read_database_facts_at_reads_the_whole_fingerprint_in_one_call() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        db.reconcile_embedding_fingerprint(&EmbeddingFingerprint {
            provider: "local".to_string(),
            model: "bge-small".to_string(),
            dimension: 384,
        })
        .unwrap();
    }

    let facts = read_database_facts_at(&db_path);

    assert_eq!(facts.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(facts.embed_provider.as_deref(), Some("local"));
    assert_eq!(facts.embed_model.as_deref(), Some("bge-small"));
    assert_eq!(facts.embed_dim, Some(384));
}

#[test]
fn schema_version_key_names_the_row_the_store_writes() {
    // `SCHEMA_VERSION_KEY` has no in-crate callers — every writer inlines the
    // literal in its SQL — so a typo in the constant is invisible except here.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let _db = Database::open(&db_path, 384).unwrap();

    assert_eq!(
        read_metadata_at(&db_path, SCHEMA_VERSION_KEY).unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string()),
        "SCHEMA_VERSION_KEY must match the literal the store writes"
    );
}

#[test]
fn read_database_facts_at_reports_no_schema_version_for_a_foreign_file() {
    // `read_schema_version_at` returns Ok(0) for a non-cartog file; storing 0
    // would render as a real version and misleadingly flag `stale-schema`.
    let dir = tempfile::TempDir::new().unwrap();
    let foreign = dir.path().join("foreign.db");
    std::fs::write(&foreign, b"not a database").unwrap();

    let facts = read_database_facts_at(&foreign);

    assert_eq!(
        facts,
        DatabaseFacts::default(),
        "all-None for a foreign file"
    );
}

#[test]
fn read_database_facts_at_reports_no_embeddings_for_a_never_embedded_db() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let _db = Database::open(&db_path, 384).unwrap();

    let facts = read_database_facts_at(&db_path);

    assert_eq!(facts.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(facts.embed_provider, None);
    assert_eq!(facts.embed_model, None);
    // The dimension IS written at open, so it is known even with no vectors.
    assert_eq!(facts.embed_dim, Some(384));
}

#[test]
fn read_database_facts_at_never_fails_on_an_absent_file() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(
        read_database_facts_at(&dir.path().join("nope.db")),
        DatabaseFacts::default()
    );
}

#[test]
fn read_metadata_at_returns_none_for_non_cartog_sqlite() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("foreign.db");
    // A SQLite file with no `metadata` table is not a cartog DB; the helper
    // treats the missing table as an absent value, not an error.
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch("CREATE TABLE notes(content TEXT);")
        .unwrap();
    drop(conn);
    assert_eq!(read_metadata_at(&db_path, "last_commit").unwrap(), None);
}

#[test]
fn read_metadata_at_returns_none_for_null_value() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let db = Database::open(&db_path, 384).unwrap();
        // A corrupt/hand-edited NULL value must read as absent, not error.
        db.conn
            .execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('last_commit', NULL)",
                [],
            )
            .unwrap();
    }
    assert_eq!(read_metadata_at(&db_path, "last_commit").unwrap(), None);
}
