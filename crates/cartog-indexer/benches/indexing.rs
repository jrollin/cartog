//! ONNX-free indexing benchmarks for `cartog-indexer`.
//!
//! Mirrors the `index_*` benches in `crates/cartog/benches/queries.rs`, but
//! lives in `cartog-indexer` so it can be run without the cartog-rag /
//! ONNX runtime build chain. Useful for quick local perf checks and CI
//! environments without the native ONNX library installed.
//!
//! Run with: `cargo bench --bench indexing`
//!
//! **Keep in sync with `crates/cartog/benches/queries.rs::bench_indexing`.**
//! When you adjust scenarios here (fixture path, page-cap, fixture file
//! invalidated for the single-file edit case), apply the same change there
//! so the two surfaces measure the same thing. Duplication is intentional —
//! the cartog-binary bench depends on `cartog-rag` which depends on the
//! ONNX native library; this crate has no such constraint.

use std::path::Path;

use cartog_core::FileInfo;
use cartog_db::Database;
use cartog_indexer::index_directory;
use criterion::{criterion_group, criterion_main, Criterion};

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("benchmarks")
        .join("fixtures")
        .join("webapp_py")
}

fn bench_indexing(c: &mut Criterion) {
    let fixture = fixture_dir();
    assert!(
        fixture.exists(),
        "expected fixture at {fixture:?}; run from a checkout that includes benchmarks/"
    );

    // Full index (force=true): baseline.
    c.bench_function("index_full_force", |b| {
        b.iter(|| {
            let db = Database::open_memory().unwrap();
            index_directory(&db, &fixture, true, false, None).unwrap();
        });
    });

    // No-op re-index: every file's stored hash matches; everything is skipped
    // before parsing.
    c.bench_function("index_incremental_noop", |b| {
        let db = Database::open_memory().unwrap();
        index_directory(&db, &fixture, true, false, None).unwrap();
        b.iter(|| {
            index_directory(&db, &fixture, false, false, None).unwrap();
        });
    });

    // Single-file change: invalidate one file's stored hash so it re-parses
    // and exercises the Merkle-diff path inside Phase 3.
    c.bench_function("index_incremental_one_file", |b| {
        let db = Database::open_memory().unwrap();
        index_directory(&db, &fixture, true, false, None).unwrap();
        b.iter(|| {
            db.upsert_file(&FileInfo {
                path: "auth/service.py".to_string(),
                last_modified: 0.0,
                hash: "invalidated".to_string(),
                language: "python".to_string(),
                num_symbols: 0,
            })
            .unwrap();
            index_directory(&db, &fixture, false, false, None).unwrap();
        });
    });
}

criterion_group!(benches, bench_indexing);
criterion_main!(benches);
