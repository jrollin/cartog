//! Tests for incremental re-index: LSP-state seals, embedding invalidation, rollback.

use crate::*;

#[test]
fn test_index_directory_force() {
    use cartog_db::Database;

    let db = Database::open_memory().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/auth");

    if fixtures.exists() {
        // First index
        let r1 = index_directory(
            &db,
            &fixtures,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
        )
        .unwrap();
        assert!(r1.files_indexed > 0);
        assert!(r1.dirty_files > 0);

        // Second index without force — should skip all files (no-op)
        let r2 = index_directory(
            &db,
            &fixtures,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
        )
        .unwrap();
        assert_eq!(r2.files_indexed, 0);
        assert!(r2.files_skipped > 0);
        assert_eq!(
            r2.dirty_files, 0,
            "no-op reindex must report zero dirty files — gates the LSP pass"
        );
        assert_eq!(
            r2.edges_lsp_resolved, 0,
            "no-op reindex must not run LSP resolution"
        );

        // Force re-index — dirty_files matches files_indexed
        let r3 = index_directory(
            &db,
            &fixtures,
            true,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
        )
        .unwrap();
        assert_eq!(r3.files_indexed, r1.files_indexed);
        assert_eq!(r3.files_skipped, 0);
        assert_eq!(r3.dirty_files, r3.files_indexed);
    }
}

#[cfg(feature = "lsp")]
#[test]
fn test_noop_reindex_does_not_run_lsp() {
    // Regression guard: no dirty files → no LSP pass.
    use cartog_db::Database;

    let db = Database::open_memory().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/auth");
    if !fixtures.exists() {
        return;
    }

    // Prime, then re-run. Don't assert LSP found anything (depends on
    // whether pyright is on PATH in CI) — only that the second pass skips it.
    let _ = index_directory(
        &db,
        &fixtures,
        false,
        true,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let r2 = index_directory(
        &db,
        &fixtures,
        false,
        true,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert_eq!(r2.dirty_files, 0);
    assert_eq!(r2.edges_lsp_resolved, 0);
}

#[test]
fn no_lsp_index_seals_unresolved_edges_at_state_four() {
    // Watch / --no-lsp runs (lsp=false) have no LSP pass to retry the
    // heuristic's leftovers, so a state=0 edge would be re-walked on every
    // re-index (#109 amplification). The indexer must seal it at state=4.
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.py"), "def caller():\n    nowhere('x')\n").unwrap();

    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &root,
        false,
        false, // lsp off — the only resolver is the heuristic
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    // The unresolvable `nowhere` call is sealed, so it no longer surfaces
    // to the unresolved set the next pass would scan.
    let names: Vec<String> = db
        .unresolved_edges()
        .unwrap()
        .into_iter()
        .map(|e| e.target_name)
        .collect();
    assert!(
        !names.contains(&"nowhere".to_string()),
        "no-lsp index must seal the unresolvable edge at state=4, got {names:?}"
    );
}

#[test]
fn test_added_symbol_reopens_unresolvable_edges() {
    // Name-keyed reset: a new symbol whose name matches a state=2 edge
    // returns the edge to state=0 (or state=1 if the heuristic resolves it).
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    // Rust's tempfile creates `.tmpXXXX` directories on macOS — the leading
    // dot makes is_ignored() reject the walk root. Nest a non-dotted child.
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.py"), "def caller():\n    find_user('x')\n").unwrap();

    let db = Database::open_memory().unwrap();
    let r1 = index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert!(
        r1.files_indexed >= 1,
        "expected a.py to index, got {:?}",
        r1
    );

    // The no-lsp index seals leftovers at state=4; reopen so the rest of
    // the test exercises the state=0 → marker lifecycle as before.
    db.reopen_heuristic_exhausted().unwrap();
    let before = db.unresolved_edges().unwrap();
    let find_user = before
        .iter()
        .find(|e| e.target_name == "find_user")
        .expect("find_user edge should exist as unresolved after first index");
    let edge_id = find_user.edge_id;
    db.mark_edge_unresolvable(edge_id).unwrap();
    assert!(db.is_edge_unresolvable(edge_id).unwrap());

    // Adding b.py with find_user definition should reopen the marker.
    std::fs::write(root.join("b.py"), "def find_user(name):\n    return None\n").unwrap();
    index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    assert!(
        !db.is_edge_unresolvable(edge_id).unwrap(),
        "edge must not stay state=2 after a matching target appears"
    );
}

#[test]
fn reindex_invalidates_embedding_of_modified_symbol() {
    // Drift regression: a symbol whose body changes keeps its stable id, so
    // its old embedding must be dropped on re-index — otherwise
    // symbols_needing_embeddings() skips it and the vector stays stale.
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    // Body must exceed MIN_CONTENT_BYTES (50) so content is extracted.
    std::fs::write(
        root.join("a.py"),
        "def greet(name):\n    message = 'hello there ' + name\n    return message\n",
    )
    .unwrap();

    let db = Database::open_memory().unwrap();
    let idx = |db: &Database| {
        index_directory(
            db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
        )
        .unwrap()
    };
    idx(&db);

    // Simulate a prior `rag index`: embed the only content symbol.
    let needing = db.symbols_needing_embeddings().unwrap();
    assert_eq!(needing.len(), 1, "expected greet() to need embedding");
    let greet_id = needing[0].clone();
    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let eid = db.get_or_create_embedding_id(&greet_id).unwrap();
    db.upsert_embedding(eid, &bytes).unwrap();
    assert!(db.symbols_needing_embeddings().unwrap().is_empty());

    // Edit the body (same name/kind/file → same stable id) and re-index.
    std::fs::write(
        root.join("a.py"),
        "def greet(name):\n    message = 'goodbye and farewell ' + name\n    return message\n",
    )
    .unwrap();
    idx(&db);

    // The drifted embedding is gone and the symbol re-enters the queue.
    assert!(
        !db.has_embedding(&greet_id).unwrap(),
        "modified symbol's stale embedding must be cleared"
    );
    assert_eq!(
        db.symbols_needing_embeddings().unwrap(),
        vec![greet_id],
        "modified symbol must re-enter the needs-embedding set"
    );
}

#[test]
fn reindex_keeps_embedding_of_unchanged_sibling() {
    // The drift-clear is scoped to modified symbols: a sibling whose own
    // content is untouched keeps its embedding even when the file is dirty.
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    let greet_v1 = "def greet(name):\n    message = 'hello there ' + name\n    return message\n";
    let farewell =
        "def farewell(name):\n    message = 'goodbye and take care ' + name\n    return message\n";
    std::fs::write(root.join("a.py"), format!("{greet_v1}\n\n{farewell}")).unwrap();

    let db = Database::open_memory().unwrap();
    let idx = |db: &Database| {
        index_directory(
            db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
        )
        .unwrap()
    };
    idx(&db);

    let bytes: Vec<u8> = vec![0.0f32; 384]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    for id in db.symbols_needing_embeddings().unwrap() {
        let eid = db.get_or_create_embedding_id(&id).unwrap();
        db.upsert_embedding(eid, &bytes).unwrap();
    }
    let farewell_id = db
        .all_content_symbol_ids()
        .unwrap()
        .into_iter()
        .find(|id| id.contains("farewell"))
        .expect("farewell symbol id");

    // Change only greet(); farewell()'s own content is identical.
    let greet_v2 =
        "def greet(name):\n    message = 'goodbye and farewell ' + name\n    return message\n";
    std::fs::write(root.join("a.py"), format!("{greet_v2}\n\n{farewell}")).unwrap();
    idx(&db);

    assert!(
        db.has_embedding(&farewell_id).unwrap(),
        "unchanged sibling must keep its embedding"
    );
}

#[test]
fn reindex_drops_stale_content_when_modified_body_shrinks_below_threshold() {
    // A modified symbol whose new body no longer yields content must not keep
    // its pre-edit content row (else it re-embeds stale text forever).
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    // v1 body is well over MIN_CONTENT_BYTES (50) so content is stored.
    std::fs::write(
        root.join("a.py"),
        "def greet(name):\n    message = 'a long enough greeting for ' + name\n    return message\n",
    )
    .unwrap();

    let db = Database::open_memory().unwrap();
    let idx = |db: &Database| {
        index_directory(
            db,
            &root,
            false,
            false,
            None,
            None,
            crate::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
            &crate::WalkFilter::unrestricted(),
        )
        .unwrap()
    };
    idx(&db);
    let greet_id = db.symbols_needing_embeddings().unwrap()[0].clone();
    assert!(db.get_symbol_content(&greet_id).unwrap().is_some());

    // Shrink the body below the content threshold; same stable id (same name).
    std::fs::write(root.join("a.py"), "def greet(name):\n    pass\n").unwrap();
    idx(&db);

    assert!(
        db.get_symbol_content(&greet_id).unwrap().is_none(),
        "stale content row must be deleted when the modified body loses content"
    );
    assert!(
        !db.symbols_needing_embeddings().unwrap().contains(&greet_id),
        "a symbol with no content must not be queued for embedding"
    );
}

#[test]
fn test_force_reindex_does_not_inherit_sticky_markers() {
    // End-to-end contract: --force is the documented escape hatch for
    // "retry everything". Under --force, every file is re-parsed and
    // `clear_edges_for_file` / `clear_file_data_in_tx` wipe the edge
    // rows before `resolve_edges` runs — so the post-force edges have
    // fresh auto-increment IDs at default state=0 regardless of what
    // state the pre-force edges held. This test exercises that path
    // through real indexing; the targeted unit test for the SQL filter
    // (`IN (2, 3)`) lives in cartog-db
    // (test_reset_all_unresolvable_resets_state_two_and_three).
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("a.py"),
        "def caller():\n    find_x()\n    find_ext()\n",
    )
    .unwrap();

    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    // The no-lsp index seals leftovers at state=4; reopen so we can mark
    // them {2, 3} and test that --force clears those sticky markers.
    db.reopen_heuristic_exhausted().unwrap();
    let pre = db.unresolved_edges().unwrap();
    let find_x_id = pre
        .iter()
        .find(|e| e.target_name == "find_x")
        .expect("find_x edge should exist")
        .edge_id;
    let find_ext_id = pre
        .iter()
        .find(|e| e.target_name == "find_ext")
        .expect("find_ext edge should exist")
        .edge_id;
    db.mark_edge_unresolvable(find_x_id).unwrap();
    db.mark_edge_external(find_ext_id).unwrap();

    // --force = true: rebuilds edges with fresh IDs, must NOT inherit state {2, 3}.
    index_directory(
        &db,
        &root,
        true,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    // --force clears the sticky LSP markers (reset_all_unresolvable), then
    // this no-lsp run re-seals the still-unresolvable edges at state=4 — a
    // fresh heuristic-exhaustion verdict, not the inherited {2, 3} markers.
    // Reopen state=4 to confirm both edges came back to the resolvable set.
    db.reopen_heuristic_exhausted().unwrap();
    let post = db.unresolved_edges().unwrap();
    assert!(
        post.iter().any(|e| e.target_name == "find_x"),
        "after --force, find_x must not stay at an inherited {{2, 3}} marker"
    );
    assert!(
        post.iter().any(|e| e.target_name == "find_ext"),
        "after --force, find_ext must not stay at an inherited {{2, 3}} marker"
    );
}

#[test]
fn test_name_keyed_reset_reopens_external_edges() {
    // If an edge was marked state=3 (target outside the indexed root) and
    // the user then vendors that target in-tree, indexing the new file
    // must reopen the external marker so LSP retries it.
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.py"), "def caller():\n    vendored_helper()\n").unwrap();

    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    // The no-lsp index seals leftovers at state=4; reopen so we can mark
    // the edge external and test the vendored-in-tree reopen path.
    db.reopen_heuristic_exhausted().unwrap();
    let unresolved = db.unresolved_edges().unwrap();
    let edge_id = unresolved
        .iter()
        .find(|e| e.target_name == "vendored_helper")
        .expect("vendored_helper edge should exist")
        .edge_id;
    db.mark_edge_external(edge_id).unwrap();
    assert_eq!(db.edge_resolution_state(edge_id).unwrap(), 3);

    // Vendor the dep in-tree.
    std::fs::write(
        root.join("vendor.py"),
        "def vendored_helper():\n    return 1\n",
    )
    .unwrap();
    index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    // After the name-keyed reset, the edge is reopened (state=0) and the
    // heuristic resolver runs in the same indexing pass — `vendored_helper`
    // is now defined in the same directory, so the same-dir heuristic
    // resolves the edge to state=1. Asserting state=1 (not just "not 3")
    // catches a future regression that breaks the reset-then-resolve
    // pipeline (e.g. silently re-marking as state=2).
    assert_eq!(
        db.edge_resolution_state(edge_id).unwrap(),
        1,
        "vendored target should reopen the external marker AND be resolved by the heuristic"
    );
}

#[test]
fn test_noop_reindex_preserves_unresolvable_and_external_markers() {
    // Defensive: a no-op reindex must not touch state {2, 3} markers — no
    // spurious resets (would burn the gate), no spurious re-marks.
    use cartog_db::Database;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("a.py"),
        "def caller():\n    find_x()\n    find_y()\n",
    )
    .unwrap();

    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    // The no-lsp index seals leftovers at state=4; reopen so we can mark
    // them {2, 3} and test that a no-op reindex preserves those markers.
    db.reopen_heuristic_exhausted().unwrap();
    let unresolved = db.unresolved_edges().unwrap();
    let burned = unresolved
        .iter()
        .find(|e| e.target_name == "find_x")
        .expect("find_x edge should exist");
    let ext = unresolved
        .iter()
        .find(|e| e.target_name == "find_y")
        .expect("find_y edge should exist");
    db.mark_edge_unresolvable(burned.edge_id).unwrap();
    db.mark_edge_external(ext.edge_id).unwrap();

    // No file changes → reindex is a no-op → markers survive.
    let r = index_directory(
        &db,
        &root,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();
    assert_eq!(r.dirty_files, 0);

    assert_eq!(
        db.edge_resolution_state(burned.edge_id).unwrap(),
        2,
        "no-op reindex must not reset state=2"
    );
    assert_eq!(
        db.edge_resolution_state(ext.edge_id).unwrap(),
        3,
        "no-op reindex must not reset state=3"
    );
}

#[test]
fn test_markdown_indexing_end_to_end() {
    use cartog_db::Database;

    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();

    let md_file = dir.join("design.md");
    std::fs::write(
        &md_file,
        r#"# Architecture

This document describes the system architecture.

## Authentication

Users authenticate via JWT tokens. The server validates
the token signature and checks expiration before granting access.

## Database

We use PostgreSQL with connection pooling via pgbouncer.
"#,
    )
    .unwrap();

    let db = Database::open_memory().unwrap();
    let result = index_directory(
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

    assert_eq!(result.files_indexed, 1);
    assert!(result.symbols_added >= 3, "should have at least 3 sections");

    // Verify file entry
    let file = db.get_file("design.md").unwrap();
    assert!(file.is_some());
    let file = file.unwrap();
    assert_eq!(file.language, "markdown");

    // Verify Document symbols exist
    let outline = db.outline("design.md").unwrap();
    assert!(
        outline.len() >= 3,
        "should have Architecture, Authentication, Database sections"
    );

    let names: Vec<&str> = outline.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"Architecture"),
        "missing Architecture section"
    );
    assert!(
        names.contains(&"Authentication"),
        "missing Authentication section"
    );
    assert!(names.contains(&"Database"), "missing Database section");

    for sym in &outline {
        assert_eq!(sym.kind, cartog_core::SymbolKind::Document);
    }

    // Verify symbol_content is populated
    let auth_sym = outline.iter().find(|s| s.name == "Authentication").unwrap();
    let content = db.get_symbol_content(&auth_sym.id).unwrap();
    assert!(
        content.is_some(),
        "symbol_content should exist for document section"
    );
    let (text, header) = content.unwrap();
    assert!(
        text.contains("JWT tokens"),
        "content should include section body"
    );
    assert!(
        header.contains("Authentication"),
        "header should include section name"
    );
}

#[test]
fn test_index_directory_rolls_back_on_disk_full() {
    use cartog_core::SymbolKind;
    use cartog_db::Database;

    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();

    // Seed file: get into a known indexed state under a generous page budget.
    std::fs::write(dir.join("seed.py"), "def keep_me():\n    return 1\n").unwrap();

    let db = Database::open_memory().unwrap();
    index_directory(
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
    .expect("seed index should succeed");

    // Snapshot the seed state so we can assert it is preserved across the
    // failed run.
    let seed_outline = db.outline("seed.py").unwrap();
    let seed_keep_me = seed_outline
        .iter()
        .find(|s| s.name == "keep_me")
        .expect("seed symbol must be present after the first index");
    let seed_keep_me_id = seed_keep_me.id.clone();

    // Add a second file that the next `index_directory` call will try to
    // ingest. Combined with a tight page cap, the new symbol/edge/content
    // writes will hit `SQLITE_FULL` somewhere inside Phase 3.
    std::fs::write(
        dir.join("big.py"),
        // Lots of small symbols: many independent INSERTs, so the page
        // budget runs out partway through and the outer tx must roll back.
        (0..200)
            .map(|i| format!("def fn_{i}():\n    return {i}\n\n"))
            .collect::<String>(),
    )
    .unwrap();

    // Cap the DB at a page count that holds the seed comfortably but
    // cannot fit the second file's worth of symbol/content rows.
    // The exact value is empirical; on macOS APFS with the default 4 KiB
    // page size, ~30 pages is enough to seed but not enough to ingest 200
    // new functions through Phase 3.
    db.set_max_page_count_for_tests(30).unwrap();

    let result = index_directory(
        &db,
        &dir,
        false,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    );
    assert!(
        result.is_err(),
        "Phase 3 must fail when SQLite runs out of pages; got Ok({result:?})"
    );

    // Lift the cap so post-mortem queries can run.
    db.set_max_page_count_for_tests(1_000_000).unwrap();

    // Rollback assertions:
    //
    // 1. The seed symbol must still be there (a regression that wiped
    //    pre-existing data is the worst flavor of the original bug).
    // 2. None of the symbols from the failed file may have leaked through:
    //    Phase 3 was wrapped in a single transaction, so partial writes
    //    are rolled back atomically.
    let seed_outline_after = db.outline("seed.py").unwrap();
    assert!(
        seed_outline_after.iter().any(|s| s.id == seed_keep_me_id),
        "seed symbol must survive the rolled-back run"
    );
    let big_outline_after = db.outline("big.py").unwrap();
    assert!(
        big_outline_after.is_empty(),
        "no symbols from the failed Phase 3 may persist; big.py outline: {:?}",
        big_outline_after
            .iter()
            .map(|s| &s.name)
            .collect::<Vec<_>>()
    );

    // The seed symbol is a function — a quick sanity check that the kind
    // wasn't corrupted by partial writes either.
    let kept = seed_outline_after
        .iter()
        .find(|s| s.id == seed_keep_me_id)
        .unwrap();
    assert_eq!(kept.kind, SymbolKind::Function);
}
