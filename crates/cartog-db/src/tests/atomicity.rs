use crate::*;

// ── Phase 3 atomicity: indexing transaction primitive ──

/// Build a minimal valid Symbol for transactional tests.
fn tx_test_symbol(id: &str, file: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        name: id.to_string(),
        kind: SymbolKind::Function,
        file_path: file.to_string(),
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 0,
        parent_id: None,
        signature: None,
        visibility: Visibility::Public,
        is_async: false,
        is_test: false,
        docstring: None,
        in_degree: 0,
        content_hash: Some("h".to_string()),
        subtree_hash: Some("s".to_string()),
    }
}

#[test]
fn test_indexing_tx_commit_persists_writes() {
    // Sanity: writes through *_in_tx variants under begin_indexing_tx
    // must persist after commit().
    let db = Database::open_memory().unwrap();
    let sym = tx_test_symbol("a.py:function:foo", "a.py");

    let tx = db.begin_indexing_tx().unwrap();
    db.insert_symbols_in_tx(std::slice::from_ref(&sym)).unwrap();
    tx.commit().unwrap();

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "committed write must persist");
}

#[test]
fn test_indexing_tx_rollback_drops_writes() {
    // Phase 3 atomicity: writes through *_in_tx variants must roll back
    // when the transaction is dropped without commit() — e.g. an `?`
    // bubbled up an error mid-pipeline, or a panic unwound the stack.
    let db = Database::open_memory().unwrap();
    let sym = tx_test_symbol("a.py:function:foo", "a.py");

    {
        let _tx = db.begin_indexing_tx().unwrap();
        db.insert_symbols_in_tx(std::slice::from_ref(&sym)).unwrap();
        // _tx dropped here without commit() — must roll back.
    }

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "writes must roll back when the indexing transaction is dropped without commit"
    );
}

#[test]
fn test_indexing_tx_partial_failure_rolls_back_full_pipeline() {
    // Phase 3 atomicity, end-to-end shape: simulate a multi-step pipeline
    // where step N fails after steps 1..N-1 already wrote. Without an
    // outer transaction, the prior writes would persist (the original
    // bug). With begin_indexing_tx wrapping the sequence, dropping `tx`
    // on the error path rolls every prior write back.
    let db = Database::open_memory().unwrap();

    // Seed one pre-existing symbol so we can verify it survives the
    // rollback path (a regression here would also wipe pre-existing
    // data, which is the worst flavor of the bug).
    let pre = tx_test_symbol("pre.py:function:keep", "pre.py");
    db.insert_symbols(std::slice::from_ref(&pre)).unwrap();

    // Run a "Phase 3 lookalike" that fails mid-way. The early `bail!`
    // means tx.commit() is unreachable; dropping `tx` on the error
    // path is exactly what we want to exercise.
    let result: Result<()> = (|| {
        let _tx = db.begin_indexing_tx()?;
        // Write a first batch.
        let batch1 = vec![tx_test_symbol("a.py:function:foo", "a.py")];
        db.insert_symbols_in_tx(&batch1)?;

        // Simulate a downstream failure after a successful early write.
        anyhow::bail!("simulated mid-pipeline failure");
    })();
    assert!(result.is_err(), "the pipeline must propagate its error");

    // The seed survives, the partial write does not.
    let names: Vec<String> = db
        .conn
        .prepare("SELECT id FROM symbols ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["pre.py:function:keep"],
        "pre-existing rows must survive; the partial write must roll back"
    );
}

#[test]
fn test_public_wrapper_still_self_commits() {
    // The public, non-`_in_tx` API must remain usable on its own —
    // existing callers (mcp server, watch, search, etc.) don't open
    // transactions and must keep working unchanged.
    let db = Database::open_memory().unwrap();
    let sym = tx_test_symbol("a.py:function:foo", "a.py");

    // No outer transaction; the wrapper opens and commits its own.
    db.insert_symbols(std::slice::from_ref(&sym)).unwrap();

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "public wrapper must persist without an outer tx");
}

#[test]
fn test_partial_pipeline_without_outer_tx_persists_writes() {
    // Discriminator test: documents the *old* behavior. Without an
    // outer transaction, an error after a successful self-committing
    // write leaves that write persisted. This is exactly the bug the
    // outer transaction in `index_directory` fixes. If this assertion
    // ever flips, it means someone changed the public wrapper's
    // semantics — and `test_indexing_tx_partial_failure_rolls_back_full_pipeline`
    // would no longer be discriminating between buggy and fixed states.
    let db = Database::open_memory().unwrap();

    let result: Result<()> = (|| {
        // Each call commits independently.
        let batch1 = vec![tx_test_symbol("a.py:function:foo", "a.py")];
        db.insert_symbols(&batch1)?;
        anyhow::bail!("simulated mid-pipeline failure");
    })();
    assert!(result.is_err());

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "without an outer transaction, an early write persists despite a later error"
    );
}
