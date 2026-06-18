//! Tests for the progress callback and Ctrl-C cancel probe.

use crate::*;

fn tiny_python_project() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.py"), "def f():\n    pass\n").unwrap();
    std::fs::write(root.join("b.py"), "def g():\n    pass\n").unwrap();
    (tmp, root)
}

#[test]
fn progress_callback_fires_in_phase_order() {
    use cartog_db::Database;
    use std::sync::Mutex;

    let (_tmp, root) = tiny_python_project();
    let db = Database::open_memory().unwrap();

    let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
    let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
    let result = index_directory(
        &db,
        &root,
        true,
        false,
        Some(&cb),
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    assert!(result.files_indexed >= 2);
    let events = events.into_inner().unwrap();
    assert_eq!(events[0], ProgressUpdate::Walking);
    // Each phase opens with done == 0 and later reports done == total.
    assert!(events
        .iter()
        .any(|e| matches!(e, ProgressUpdate::Parsing { done: 0, total } if *total >= 2)));
    assert!(events.iter().any(
        |e| matches!(e, ProgressUpdate::Parsing { done, total } if done == total && *total >= 2)
    ));
    assert!(events
        .iter()
        .any(|e| matches!(e, ProgressUpdate::Storing { done: 0, total } if *total >= 2)));
    assert!(events.iter().any(
        |e| matches!(e, ProgressUpdate::Storing { done, total } if done == total && *total >= 2)
    ));
    // Phase order: last Walking < first Parsing < first Storing.
    let pos = |pred: fn(&ProgressUpdate) -> bool| events.iter().position(pred).unwrap();
    let walking = pos(|e| matches!(e, ProgressUpdate::Walking));
    let parsing = pos(|e| matches!(e, ProgressUpdate::Parsing { .. }));
    let storing = pos(|e| matches!(e, ProgressUpdate::Storing { .. }));
    assert!(walking < parsing && parsing < storing);
}

#[test]
fn progress_callback_none_matches_some_for_result() {
    use cartog_db::Database;

    let (_t1, root1) = tiny_python_project();
    let db1 = Database::open_memory().unwrap();
    let r_none = index_directory(
        &db1,
        &root1,
        true,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let (_t2, root2) = tiny_python_project();
    let db2 = Database::open_memory().unwrap();
    let cb = |_: ProgressUpdate| {};
    let r_some = index_directory(
        &db2,
        &root2,
        true,
        false,
        Some(&cb),
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    // Different temp dirs → different file modified-times can shift, but the
    // count-based fields of IndexResult are deterministic on a fresh DB.
    assert_eq!(r_none.files_indexed, r_some.files_indexed);
    assert_eq!(r_none.symbols_added, r_some.symbols_added);
    assert_eq!(r_none.edges_added, r_some.edges_added);
}

#[test]
fn progress_callback_emits_walking_then_parsing_and_storing() {
    use cartog_db::Database;
    use std::sync::Mutex;

    let (_tmp, root) = tiny_python_project();
    let db = Database::open_memory().unwrap();
    let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
    let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
    index_directory(
        &db,
        &root,
        true,
        false,
        Some(&cb),
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let events = events.into_inner().unwrap();
    assert!(
        matches!(events.first(), Some(ProgressUpdate::Walking)),
        "first progress event must be Walking, got {:?}",
        events.first()
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProgressUpdate::Parsing { total, .. } if *total > 0)),
        "must emit a Parsing event with a positive total"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProgressUpdate::Storing { total, .. } if *total > 0)),
        "must emit a Storing event with a positive total"
    );
}

#[test]
fn progress_counter_climbs_mid_phase_for_large_repo() {
    use cartog_db::Database;
    use std::sync::Mutex;

    // More than PROGRESS_STRIDE files so the in-loop stride emit fires at
    // least one intermediate `done` (0 < done < total) — the path the small
    // 2-file fixtures never exercise.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("project");
    std::fs::create_dir(&root).unwrap();
    let n = PROGRESS_STRIDE * 3 + 5; // 197 files
    for i in 0..n {
        std::fs::write(
            root.join(format!("m{i}.py")),
            format!("def f{i}():\n    return {i}\n"),
        )
        .unwrap();
    }

    let db = Database::open_memory().unwrap();
    let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
    let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
    index_directory(
        &db,
        &root,
        true,
        false,
        Some(&cb),
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let events = events.into_inner().unwrap();
    // A climbing parse event with done strictly between 0 and total.
    assert!(
        events.iter().any(|e| matches!(
            e,
            ProgressUpdate::Parsing { done, total } if *done > 0 && *done < *total
        )),
        "expected a mid-climb Parsing event, got {events:?}"
    );
    // Same for storing.
    assert!(events.iter().any(|e| matches!(
        e,
        ProgressUpdate::Storing { done, total } if *done > 0 && *done < *total
    )));
    // Emitted parse `done` values never decrease (out-of-order rayon clamp).
    let parse_dones: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            ProgressUpdate::Parsing { done, .. } => Some(*done),
            _ => None,
        })
        .collect();
    assert!(
        parse_dones.windows(2).all(|w| w[0] <= w[1]),
        "parse done must be non-decreasing, got {parse_dones:?}"
    );
}

#[test]
fn cancel_probe_returning_true_aborts_with_cancelled_error() {
    use cartog_db::Database;

    let (_tmp, root) = tiny_python_project();
    let db = Database::open_memory().unwrap();

    let probe = || true;
    let err = index_directory(
        &db,
        &root,
        true,
        false,
        None,
        Some(&probe),
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .expect_err("index must abort when probe trips at first phase boundary");
    assert!(
        err.to_string().contains("cancelled"),
        "error must mention cancellation, got: {err}"
    );
}

#[test]
fn cancel_probe_returning_false_runs_to_completion() {
    use cartog_db::Database;

    let (_tmp, root) = tiny_python_project();
    let db = Database::open_memory().unwrap();

    let probe = || false;
    let result = index_directory(
        &db,
        &root,
        true,
        false,
        None,
        Some(&probe),
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .expect("non-cancelling probe must not affect normal indexing");
    assert!(result.files_indexed >= 2);
}

#[test]
fn rerun_after_cancellation_completes_normally() {
    use cartog_db::Database;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_tmp, root) = tiny_python_project();
    let db = Database::open_memory().unwrap();

    let flag = AtomicBool::new(true);
    let probe = || flag.load(Ordering::SeqCst);
    let _ = index_directory(
        &db,
        &root,
        true,
        false,
        None,
        Some(&probe),
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .expect_err("first run cancels");

    // Flip the probe off — second run must complete and produce a real result.
    flag.store(false, Ordering::SeqCst);
    let result = index_directory(
        &db,
        &root,
        true,
        false,
        None,
        Some(&probe),
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .expect("re-run after cancellation must succeed");
    assert!(result.files_indexed >= 2);
}
