//! Criterion benchmarks for cartog query operations.
//!
//! Indexes the Python benchmark fixture once, then measures query latency
//! for search, refs, impact, outline, callees, hierarchy, deps, and stats.
//!
//! Run with: `cargo bench --bench queries`

use criterion::{criterion_group, criterion_main, Criterion};
use std::path::Path;

use cartog::db::Database;
use cartog::indexer::index_directory;
use cartog::types::EdgeKind;

/// Build an indexed database from the Python benchmark fixture.
fn setup_db() -> Database {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks")
        .join("fixtures")
        .join("webapp_py");

    let db = Database::open_memory().expect("open in-memory DB");
    index_directory(&db, &fixture_dir, true).expect("index fixture");
    db
}

fn bench_search(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("search_token", |b| {
        b.iter(|| db.search("token", None, None, 100).unwrap())
    });

    c.bench_function("search_validate", |b| {
        b.iter(|| db.search("validate", None, None, 100).unwrap())
    });

    c.bench_function("search_no_match", |b| {
        b.iter(|| {
            db.search("zzz_nonexistent_symbol", None, None, 100)
                .unwrap()
        })
    });
}

fn bench_refs(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("refs_validate_token_all", |b| {
        b.iter(|| db.refs("validate_token", None).unwrap())
    });

    c.bench_function("refs_validate_token_calls", |b| {
        b.iter(|| db.refs("validate_token", Some(EdgeKind::Calls)).unwrap())
    });

    c.bench_function("refs_get_logger_all", |b| {
        b.iter(|| db.refs("get_logger", None).unwrap())
    });

    c.bench_function("refs_AuthService", |b| {
        b.iter(|| db.refs("AuthService", None).unwrap())
    });
}

fn bench_impact(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("impact_AuthService_d3", |b| {
        b.iter(|| db.impact("AuthService", 3).unwrap())
    });

    c.bench_function("impact_DatabaseConnection_d5", |b| {
        b.iter(|| db.impact("DatabaseConnection", 5).unwrap())
    });

    c.bench_function("impact_validate_token_d3", |b| {
        b.iter(|| db.impact("validate_token", 3).unwrap())
    });
}

fn bench_outline(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("outline_auth_service", |b| {
        b.iter(|| db.outline("auth/service.py").unwrap())
    });

    c.bench_function("outline_routes_auth", |b| {
        b.iter(|| db.outline("routes/auth.py").unwrap())
    });
}

fn bench_callees(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("callees_login_route", |b| {
        b.iter(|| db.callees("login_route").unwrap())
    });

    c.bench_function("callees_login", |b| b.iter(|| db.callees("login").unwrap()));

    c.bench_function("callees_generate_token", |b| {
        b.iter(|| db.callees("generate_token").unwrap())
    });
}

fn bench_hierarchy(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("hierarchy_BaseService", |b| {
        b.iter(|| db.hierarchy("BaseService").unwrap())
    });

    c.bench_function("hierarchy_AppError", |b| {
        b.iter(|| db.hierarchy("AppError").unwrap())
    });
}

fn bench_deps(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("deps_routes_auth", |b| {
        b.iter(|| db.file_deps("routes/auth.py").unwrap())
    });

    c.bench_function("deps_auth_service", |b| {
        b.iter(|| db.file_deps("auth/service.py").unwrap())
    });
}

fn bench_stats(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("stats", |b| b.iter(|| db.stats().unwrap()));
}

criterion_group!(
    benches,
    bench_search,
    bench_refs,
    bench_impact,
    bench_outline,
    bench_callees,
    bench_hierarchy,
    bench_deps,
    bench_stats,
);
criterion_main!(benches);
