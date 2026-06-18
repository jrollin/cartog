//! Tests for the --jobs parse-pool cap and pool reuse.

use super::visible_dir;
use crate::*;

#[test]
fn clamp_jobs_resolves_zero_to_available_and_bounds_the_rest() {
    // 0 = auto, but capped at 64 even on a >64-core host.
    let auto = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
        .min(64);
    assert_eq!(clamp_jobs(0), auto, "0 = auto, clamped to the upper bound");
    assert_eq!(clamp_jobs(1), 1);
    assert_eq!(clamp_jobs(8), 8);
    assert_eq!(clamp_jobs(64), 64);
    assert_eq!(clamp_jobs(1000), 64, "clamped to the upper bound");
}

#[test]
fn parse_pool_is_sized_to_jobs_and_reused() {
    // A sized pool actually has that many threads, and the same size hands
    // back the cached instance (warm threads survive re-index).
    let p3 = parse_pool(3).expect("pool builds");
    assert_eq!(p3.current_num_threads(), 3);
    let p3_again = parse_pool(3).expect("pool builds");
    assert!(
        Arc::ptr_eq(&p3, &p3_again),
        "same size reuses the cached pool"
    );
    let p1 = parse_pool(1).expect("pool builds");
    assert_eq!(p1.current_num_threads(), 1);
}

fn index_dir_with_jobs(db: &cartog_db::Database, root: &Path, jobs: usize) -> IndexResult {
    let filter = WalkFilter {
        jobs,
        ..Default::default()
    };
    index_directory(
        db,
        root,
        false,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &filter,
    )
    .unwrap()
}

fn symbol_names(db: &cartog_db::Database) -> Vec<String> {
    let files = db.all_files().unwrap();
    db.symbols_for_files(&files, None)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect()
}

#[test]
fn index_output_is_identical_across_pool_sizes() {
    // The dedicated parse pool applies per call, so two pool sizes are
    // comparable in one process: parallel parsing must not change results.
    let make = || {
        let tmp = tempfile::TempDir::new().unwrap();
        let proj = visible_dir(&tmp, "proj");
        for i in 0..12 {
            std::fs::write(
                proj.join(format!("m{i}.py")),
                format!("def f{i}():\n    return {i}\n"),
            )
            .unwrap();
        }
        (tmp, proj)
    };

    let db1 = cartog_db::Database::open_memory().unwrap();
    let (_t1, p1) = make();
    let r1 = index_dir_with_jobs(&db1, &p1, 1);

    let db8 = cartog_db::Database::open_memory().unwrap();
    let (_t8, p8) = make();
    let r8 = index_dir_with_jobs(&db8, &p8, 8);

    assert_eq!(r1.files_indexed, 12);
    assert_eq!(r1.files_indexed, r8.files_indexed);
    assert_eq!(r1.symbols_added, r8.symbols_added);

    let mut names1 = symbol_names(&db1);
    let mut names8 = symbol_names(&db8);
    names1.sort();
    names8.sort();
    assert_eq!(names1, names8, "symbol set is pool-size independent");
}
