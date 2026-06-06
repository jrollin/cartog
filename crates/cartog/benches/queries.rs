//! Criterion benchmarks for cartog query operations.
//!
//! Indexes the Python benchmark fixture once, then measures query latency
//! for search, refs, impact, outline, callees, hierarchy, deps, and stats.
//!
//! Note: separate from `benchmarks/` (shell-based integration suite measuring
//! token efficiency and recall). Both share `benchmarks/fixtures/`.
//!
//! Inputs and results are wrapped in [`std::hint::black_box`] so the compiler
//! cannot constant-fold the literal queries or eliminate the (otherwise
//! unused) results — without it these microsecond-scale benches risk
//! measuring nothing.
//!
//! Indexing throughput is measured separately by `cartog-indexer`'s `indexing`
//! bench (ONNX-free); this target covers query latency only.
//!
//! Run with: `cargo bench -p cartog --bench queries`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

use cartog::db::Database;
use cartog::indexer::{bench_support, index_directory};
use cartog::types::EdgeKind;

/// Build an indexed database from the Python benchmark fixture.
fn setup_db() -> Database {
    let db = Database::open_memory().expect("open in-memory DB");
    index_directory(
        &db,
        &bench_support::fixture_path(),
        true,
        false,
        None,
        None,
        cartog::indexer::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
    )
    .expect("index fixture");
    db
}

fn bench_search(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("search_token", |b| {
        b.iter(|| black_box(db.search(black_box("token"), None, None, 100).unwrap()))
    });

    c.bench_function("search_validate", |b| {
        b.iter(|| black_box(db.search(black_box("validate"), None, None, 100).unwrap()))
    });

    c.bench_function("search_no_match", |b| {
        b.iter(|| {
            black_box(
                db.search(black_box("zzz_nonexistent_symbol"), None, None, 100)
                    .unwrap(),
            )
        })
    });
}

fn bench_refs(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("refs_validate_token_all", |b| {
        b.iter(|| black_box(db.refs(black_box("validate_token"), None).unwrap()))
    });

    c.bench_function("refs_validate_token_calls", |b| {
        b.iter(|| {
            black_box(
                db.refs(black_box("validate_token"), Some(EdgeKind::Calls))
                    .unwrap(),
            )
        })
    });

    c.bench_function("refs_get_logger_all", |b| {
        b.iter(|| black_box(db.refs(black_box("get_logger"), None).unwrap()))
    });

    c.bench_function("refs_AuthService", |b| {
        b.iter(|| black_box(db.refs(black_box("AuthService"), None).unwrap()))
    });
}

fn bench_impact(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("impact_AuthService_d3", |b| {
        b.iter(|| black_box(db.impact(black_box("AuthService"), 3).unwrap()))
    });

    c.bench_function("impact_DatabaseConnection_d5", |b| {
        b.iter(|| black_box(db.impact(black_box("DatabaseConnection"), 5).unwrap()))
    });

    c.bench_function("impact_validate_token_d3", |b| {
        b.iter(|| black_box(db.impact(black_box("validate_token"), 3).unwrap()))
    });
}

fn bench_outline(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("outline_auth_service", |b| {
        b.iter(|| black_box(db.outline(black_box("auth/service.py")).unwrap()))
    });

    c.bench_function("outline_routes_auth", |b| {
        b.iter(|| black_box(db.outline(black_box("routes/auth.py")).unwrap()))
    });
}

fn bench_callees(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("callees_login_route", |b| {
        b.iter(|| black_box(db.callees(black_box("login_route")).unwrap()))
    });

    c.bench_function("callees_login", |b| {
        b.iter(|| black_box(db.callees(black_box("login")).unwrap()))
    });

    c.bench_function("callees_generate_token", |b| {
        b.iter(|| black_box(db.callees(black_box("generate_token")).unwrap()))
    });
}

fn bench_hierarchy(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("hierarchy_BaseService", |b| {
        b.iter(|| black_box(db.hierarchy(black_box("BaseService")).unwrap()))
    });

    c.bench_function("hierarchy_AppError", |b| {
        b.iter(|| black_box(db.hierarchy(black_box("AppError")).unwrap()))
    });
}

fn bench_deps(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("deps_routes_auth", |b| {
        b.iter(|| black_box(db.file_deps(black_box("routes/auth.py")).unwrap()))
    });

    c.bench_function("deps_auth_service", |b| {
        b.iter(|| black_box(db.file_deps(black_box("auth/service.py")).unwrap()))
    });
}

fn bench_stats(c: &mut Criterion) {
    let db = setup_db();

    c.bench_function("stats", |b| b.iter(|| black_box(db.stats().unwrap())));
}

fn setup_java_db() -> Database {
    let fixture_dir = bench_support::fixture_path()
        .parent()
        .expect("fixtures dir")
        .join("webapp_java");

    let db = Database::open_memory().expect("open in-memory DB");
    index_directory(
        &db,
        &fixture_dir,
        true,
        false,
        None,
        None,
        cartog::indexer::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
    )
    .expect("index Java fixture");
    db
}

fn bench_java_search(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_search_token", |b| {
        b.iter(|| black_box(db.search(black_box("Token"), None, None, 100).unwrap()))
    });

    c.bench_function("java_search_validate", |b| {
        b.iter(|| black_box(db.search(black_box("validate"), None, None, 100).unwrap()))
    });

    c.bench_function("java_search_no_match", |b| {
        b.iter(|| {
            black_box(
                db.search(black_box("zzz_nonexistent_symbol"), None, None, 100)
                    .unwrap(),
            )
        })
    });
}

fn bench_java_refs(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_refs_validateToken_all", |b| {
        b.iter(|| black_box(db.refs(black_box("validateToken"), None).unwrap()))
    });

    c.bench_function("java_refs_validateToken_calls", |b| {
        b.iter(|| {
            black_box(
                db.refs(black_box("validateToken"), Some(EdgeKind::Calls))
                    .unwrap(),
            )
        })
    });

    c.bench_function("java_refs_TokenException_all", |b| {
        b.iter(|| black_box(db.refs(black_box("TokenException"), None).unwrap()))
    });

    c.bench_function("java_refs_AuthService", |b| {
        b.iter(|| black_box(db.refs(black_box("AuthService"), None).unwrap()))
    });
}

fn bench_java_impact(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_impact_AuthService_d3", |b| {
        b.iter(|| black_box(db.impact(black_box("AuthService"), 3).unwrap()))
    });

    c.bench_function("java_impact_DatabaseConnection_d3", |b| {
        b.iter(|| black_box(db.impact(black_box("DatabaseConnection"), 3).unwrap()))
    });

    c.bench_function("java_impact_validateToken_d3", |b| {
        b.iter(|| black_box(db.impact(black_box("validateToken"), 3).unwrap()))
    });
}

fn bench_java_outline(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_outline_token_service", |b| {
        b.iter(|| black_box(db.outline(black_box("auth/TokenService.java")).unwrap()))
    });

    c.bench_function("java_outline_auth_routes", |b| {
        b.iter(|| black_box(db.outline(black_box("routes/AuthRoutes.java")).unwrap()))
    });
}

fn bench_java_callees(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_callees_handleLogin", |b| {
        b.iter(|| black_box(db.callees(black_box("handleLogin")).unwrap()))
    });

    c.bench_function("java_callees_authenticate", |b| {
        b.iter(|| black_box(db.callees(black_box("authenticate")).unwrap()))
    });

    c.bench_function("java_callees_generateToken", |b| {
        b.iter(|| black_box(db.callees(black_box("generateToken")).unwrap()))
    });
}

fn bench_java_hierarchy(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_hierarchy_AppException", |b| {
        b.iter(|| black_box(db.hierarchy(black_box("AppException")).unwrap()))
    });

    c.bench_function("java_hierarchy_AuthService", |b| {
        b.iter(|| black_box(db.hierarchy(black_box("AuthService")).unwrap()))
    });
}

fn bench_java_deps(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_deps_auth_routes", |b| {
        b.iter(|| black_box(db.file_deps(black_box("routes/AuthRoutes.java")).unwrap()))
    });

    c.bench_function("java_deps_auth_service", |b| {
        b.iter(|| {
            black_box(
                db.file_deps(black_box("services/AuthenticationService.java"))
                    .unwrap(),
            )
        })
    });
}

fn bench_java_stats(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_stats", |b| b.iter(|| black_box(db.stats().unwrap())));
}

// Indexing benchmarks live in `cartog-indexer/benches/indexing.rs` (its own
// `[[bench]]` target). They run without the ONNX build chain, so CI can
// measure them directly without linking `cartog-rag`. This bench is
// query-only.

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
    bench_java_search,
    bench_java_refs,
    bench_java_impact,
    bench_java_outline,
    bench_java_callees,
    bench_java_hierarchy,
    bench_java_deps,
    bench_java_stats,
);
criterion_main!(benches);
