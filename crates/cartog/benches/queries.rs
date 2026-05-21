//! Criterion benchmarks for cartog query operations.
//!
//! Indexes the Python benchmark fixture once, then measures query latency
//! for search, refs, impact, outline, callees, hierarchy, deps, and stats.
//!
//! Note: separate from `benchmarks/` (shell-based integration suite measuring
//! token efficiency and recall). Both share `benchmarks/fixtures/`.
//!
//! Run with: `cargo bench --bench queries`

use criterion::{criterion_group, criterion_main, Criterion};
use std::path::Path;

use cartog::db::Database;
use cartog::indexer::index_directory;
use cartog::types::{EdgeKind, FileInfo};

/// Build an indexed database from the Python benchmark fixture.
fn setup_db() -> Database {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("benchmarks")
        .join("fixtures")
        .join("webapp_py");

    let db = Database::open_memory().expect("open in-memory DB");
    index_directory(&db, &fixture_dir, true, false, None, None).expect("index fixture");
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

fn setup_java_db() -> Database {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("benchmarks")
        .join("fixtures")
        .join("webapp_java");

    let db = Database::open_memory().expect("open in-memory DB");
    index_directory(&db, &fixture_dir, true, false, None, None).expect("index Java fixture");
    db
}

fn bench_java_search(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_search_token", |b| {
        b.iter(|| db.search("Token", None, None, 100).unwrap())
    });

    c.bench_function("java_search_validate", |b| {
        b.iter(|| db.search("validate", None, None, 100).unwrap())
    });

    c.bench_function("java_search_no_match", |b| {
        b.iter(|| {
            db.search("zzz_nonexistent_symbol", None, None, 100)
                .unwrap()
        })
    });
}

fn bench_java_refs(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_refs_validateToken_all", |b| {
        b.iter(|| db.refs("validateToken", None).unwrap())
    });

    c.bench_function("java_refs_validateToken_calls", |b| {
        b.iter(|| db.refs("validateToken", Some(EdgeKind::Calls)).unwrap())
    });

    c.bench_function("java_refs_TokenException_all", |b| {
        b.iter(|| db.refs("TokenException", None).unwrap())
    });

    c.bench_function("java_refs_AuthService", |b| {
        b.iter(|| db.refs("AuthService", None).unwrap())
    });
}

fn bench_java_impact(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_impact_AuthService_d3", |b| {
        b.iter(|| db.impact("AuthService", 3).unwrap())
    });

    c.bench_function("java_impact_DatabaseConnection_d3", |b| {
        b.iter(|| db.impact("DatabaseConnection", 3).unwrap())
    });

    c.bench_function("java_impact_validateToken_d3", |b| {
        b.iter(|| db.impact("validateToken", 3).unwrap())
    });
}

fn bench_java_outline(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_outline_token_service", |b| {
        b.iter(|| db.outline("auth/TokenService.java").unwrap())
    });

    c.bench_function("java_outline_auth_routes", |b| {
        b.iter(|| db.outline("routes/AuthRoutes.java").unwrap())
    });
}

fn bench_java_callees(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_callees_handleLogin", |b| {
        b.iter(|| db.callees("handleLogin").unwrap())
    });

    c.bench_function("java_callees_authenticate", |b| {
        b.iter(|| db.callees("authenticate").unwrap())
    });

    c.bench_function("java_callees_generateToken", |b| {
        b.iter(|| db.callees("generateToken").unwrap())
    });
}

fn bench_java_hierarchy(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_hierarchy_AppException", |b| {
        b.iter(|| db.hierarchy("AppException").unwrap())
    });

    c.bench_function("java_hierarchy_AuthService", |b| {
        b.iter(|| db.hierarchy("AuthService").unwrap())
    });
}

fn bench_java_deps(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_deps_auth_routes", |b| {
        b.iter(|| db.file_deps("routes/AuthRoutes.java").unwrap())
    });

    c.bench_function("java_deps_auth_service", |b| {
        b.iter(|| db.file_deps("services/AuthenticationService.java").unwrap())
    });
}

fn bench_java_stats(c: &mut Criterion) {
    let db = setup_java_db();

    c.bench_function("java_stats", |b| b.iter(|| db.stats().unwrap()));
}

// ── Indexing benchmarks ──

fn bench_indexing(c: &mut Criterion) {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("benchmarks")
        .join("fixtures")
        .join("webapp_py");

    // Full index (force=true): baseline
    c.bench_function("index_full_force", |b| {
        b.iter(|| {
            let db = Database::open_memory().unwrap();
            index_directory(&db, &fixture_dir, true, false, None, None).unwrap()
        })
    });

    // Incremental no-op: all files already indexed with matching hashes
    c.bench_function("index_incremental_noop", |b| {
        let db = Database::open_memory().unwrap();
        index_directory(&db, &fixture_dir, true, false, None, None).unwrap();
        b.iter(|| index_directory(&db, &fixture_dir, false, false, None, None).unwrap())
    });

    // Incremental one-file change: invalidate one file's hash to force re-parse + Merkle diff
    c.bench_function("index_incremental_one_file", |b| {
        let db = Database::open_memory().unwrap();
        index_directory(&db, &fixture_dir, true, false, None, None).unwrap();
        b.iter(|| {
            // Invalidate hash to simulate file change
            db.upsert_file(&FileInfo {
                path: "auth/service.py".to_string(),
                last_modified: 0.0,
                hash: "invalidated".to_string(),
                language: "python".to_string(),
                num_symbols: 0,
            })
            .unwrap();
            index_directory(&db, &fixture_dir, false, false, None, None).unwrap()
        })
    });
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
    bench_java_search,
    bench_java_refs,
    bench_java_impact,
    bench_java_outline,
    bench_java_callees,
    bench_java_hierarchy,
    bench_java_deps,
    bench_java_stats,
    bench_indexing,
);
criterion_main!(benches);
