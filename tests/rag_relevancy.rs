//! RAG search relevancy benchmark.
//!
//! Indexes the Python benchmark fixture, runs a fixed set of queries with
//! expected results, and computes precision@k, recall@k, and NDCG@k.
//!
//! Run with: `cargo test --test rag_relevancy -- --nocapture`
//!
//! The output is a table of scores per query, plus aggregated means.
//! Use this to compare search quality before/after changes.

use std::path::Path;

use cartog::db::Database;
use cartog::indexer::index_directory;
use cartog::rag::search::hybrid_search;

/// A single relevancy test case.
struct QueryCase {
    /// Natural language query to run.
    query: &'static str,
    /// Symbol names that MUST appear in the top-k results, in ideal rank order.
    /// First = most relevant. Used for NDCG.
    expected: &'static [&'static str],
    /// k — how many results to evaluate.
    k: usize,
}

/// Precision@k: fraction of top-k results that are in the expected set.
fn precision_at_k(results: &[String], expected: &[&str], k: usize) -> f64 {
    let top_k: Vec<&str> = results.iter().take(k).map(|s| s.as_str()).collect();
    if top_k.is_empty() {
        return 0.0;
    }
    let hits = top_k.iter().filter(|r| expected.contains(r)).count();
    hits as f64 / top_k.len() as f64
}

/// Recall@k: fraction of expected items that appear in the top-k results.
fn recall_at_k(results: &[String], expected: &[&str], k: usize) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    let top_k: Vec<&str> = results.iter().take(k).map(|s| s.as_str()).collect();
    let hits = expected.iter().filter(|e| top_k.contains(e)).count();
    hits as f64 / expected.len() as f64
}

/// NDCG@k: Normalized Discounted Cumulative Gain.
///
/// `expected` defines the ideal relevance order: position 0 = rel score len,
/// position 1 = rel score len-1, etc. (graded relevance by expected rank).
fn ndcg_at_k(results: &[String], expected: &[&str], k: usize) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }

    // Assign relevance scores: expected[0] gets score len, expected[1] gets len-1, etc.
    let max_rel = expected.len();
    let relevance = |name: &str| -> f64 {
        expected
            .iter()
            .position(|e| *e == name)
            .map(|pos| (max_rel - pos) as f64)
            .unwrap_or(0.0)
    };

    // DCG@k
    let dcg: f64 = results
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, name)| relevance(name) / (2.0_f64 + i as f64).log2())
        .sum();

    // Ideal DCG@k (expected in perfect order)
    let idcg: f64 = expected
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, _)| (max_rel - i) as f64 / (2.0_f64 + i as f64).log2())
        .sum();

    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

fn setup_db() -> Database {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks")
        .join("fixtures")
        .join("webapp_py");

    let db = Database::open_memory().expect("open in-memory DB");
    index_directory(&db, &fixture_dir, true).expect("index fixture");
    db
}

#[test]
fn rag_relevancy_benchmark() {
    let db = setup_db();

    // Ground truth: expected symbol names come from `cartog search` on webapp_py.
    // Ordered by ideal relevance (best match first) for NDCG scoring.
    let cases = vec![
        QueryCase {
            query: "validate token",
            expected: &["validate_token", "verify_token", "extract_token"],
            k: 5,
        },
        QueryCase {
            query: "database connection",
            expected: &["DatabaseConnection", "ConnectionHandle", "get_connection"],
            k: 5,
        },
        QueryCase {
            query: "authenticate user",
            expected: &["authenticate", "login", "AuthService"],
            k: 5,
        },
        QueryCase {
            query: "send email",
            expected: &[
                "send_welcome_email",
                "send_password_reset_email",
                "EmailSender",
                "process_email_queue",
            ],
            k: 5,
        },
        QueryCase {
            query: "cache",
            expected: &["cache_get", "cache_set", "cache_invalidate", "BaseCache"],
            k: 5,
        },
        QueryCase {
            query: "error exception",
            expected: &["AppError", "TokenError", "DatabaseError", "ValidationError"],
            k: 10,
        },
        QueryCase {
            query: "middleware",
            expected: &[
                "auth_middleware",
                "logging_middleware",
                "rate_limit_middleware",
                "cors_middleware",
            ],
            k: 5,
        },
        QueryCase {
            query: "user",
            expected: &["User", "UserQueries", "get_current_user"],
            k: 5,
        },
        QueryCase {
            query: "login route",
            expected: &["login_route", "logout_route", "refresh_route"],
            k: 5,
        },
        QueryCase {
            query: "generate token",
            expected: &["generate_token"],
            k: 3,
        },
    ];

    // Try loading the cross-encoder to check availability.
    let reranker_active = cartog::rag::reranker::CrossEncoderEngine::load().is_ok();

    println!();
    println!(
        "  Re-ranker: {}",
        if reranker_active {
            "active"
        } else {
            "off (model not downloaded)"
        }
    );
    println!();
    println!(
        "  {:<35} {:>10} {:>10} {:>10} {:>6}",
        "Query", "P@k", "R@k", "NDCG@k", "k"
    );
    println!("  {}", "-".repeat(75));

    let mut total_p = 0.0;
    let mut total_r = 0.0;
    let mut total_ndcg = 0.0;
    let n = cases.len() as f64;

    for case in &cases {
        let result = hybrid_search(&db, case.query, case.k as u32, None)
            .unwrap_or_else(|e| panic!("search failed for '{}': {e}", case.query));

        let names: Vec<String> = result
            .results
            .iter()
            .map(|r| r.symbol.name.clone())
            .collect();

        let p = precision_at_k(&names, case.expected, case.k);
        let r = recall_at_k(&names, case.expected, case.k);
        let ndcg = ndcg_at_k(&names, case.expected, case.k);

        total_p += p;
        total_r += r;
        total_ndcg += ndcg;

        println!(
            "  {:<35} {:>9.1}% {:>9.1}% {:>9.3}  {:>5}",
            case.query,
            p * 100.0,
            r * 100.0,
            ndcg,
            case.k
        );

        // Print actual results for debugging
        let actual_str = if names.is_empty() {
            "(no results)".to_string()
        } else {
            names
                .iter()
                .take(case.k)
                .map(|n| {
                    if case.expected.contains(&n.as_str()) {
                        format!("[{n}]")
                    } else {
                        n.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!("    got: {actual_str}");
    }

    println!("  {}", "-".repeat(75));
    println!(
        "  {:<35} {:>9.1}% {:>9.1}% {:>9.3}",
        "MEAN",
        total_p / n * 100.0,
        total_r / n * 100.0,
        total_ndcg / n,
    );
    println!();
}
