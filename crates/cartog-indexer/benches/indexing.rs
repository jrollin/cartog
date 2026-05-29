//! ONNX-free indexing benchmarks for `cartog-indexer`.
//!
//! This is the canonical home for indexing-throughput benchmarks: it lives in
//! `cartog-indexer`, which has no `cartog-rag`/ONNX dependency, so CI can run
//! it without the native ONNX build chain. The sibling `cartog --bench
//! queries` target deliberately does *not* duplicate these.
//!
//! Run with: `cargo bench -p cartog-indexer --bench indexing`
//!
//! Scenarios:
//! - `index_full_force/<lang>` — full re-index of each language fixture. Each
//!   exercises a distinct tree-sitter grammar + extractor, which is where
//!   indexing cost actually varies, so this is parameterized over all 8
//!   fixtures.
//! - `index_incremental_noop` / `index_incremental_one_file` — the incremental
//!   skip/diff paths. These are language-agnostic (hash compare + Merkle diff
//!   in `cartog-db`/`cartog-indexer`, not in any grammar), so they run on the
//!   dense Python fixture only.
//!
//! The timed bodies live in [`cartog_indexer::bench_support`] so they stay
//! identical to anything that reuses them.

use std::hint::black_box;

use cartog_indexer::bench_support;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

fn bench_full_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_full_force");
    for (lang, fixture) in bench_support::all_fixtures() {
        group.bench_with_input(BenchmarkId::from_parameter(lang), &fixture, |b, fixture| {
            b.iter(|| black_box(bench_support::full_force(black_box(fixture))));
        });
    }
    group.finish();
}

fn bench_incremental(c: &mut Criterion) {
    let fixture = bench_support::fixture_path();

    // No-op re-index: every file's stored hash matches; everything is skipped
    // before parsing.
    c.bench_function("index_incremental_noop", |b| {
        let db = bench_support::seed(&fixture);
        b.iter(|| black_box(bench_support::noop(black_box(&db), black_box(&fixture))));
    });

    // Single-file change: invalidate one file's stored hash so it re-parses
    // and exercises the Merkle-diff path inside Phase 3.
    c.bench_function("index_incremental_one_file", |b| {
        let db = bench_support::seed(&fixture);
        b.iter(|| black_box(bench_support::one_file(black_box(&db), black_box(&fixture))));
    });
}

criterion_group!(benches, bench_full_index, bench_incremental);
criterion_main!(benches);
