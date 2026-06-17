//! In-memory-DB tool-handler, response-serialization and stale-banner tests.

use super::snap;
use crate::*;

// ── Tool handler tests (using in-memory DB) ──

// These test the underlying DB operations that the MCP handlers call.
// We cannot easily construct MCP tool calls in unit tests without a full
// server, so we test the DB layer directly with the same patterns.

#[test]
fn empty_db_outline_returns_empty() {
    let db = Database::open_memory().expect("in-memory DB");
    let result = db.outline("nonexistent.py").expect("query");
    assert!(result.is_empty());
}

#[test]
fn empty_db_refs_returns_empty() {
    let db = Database::open_memory().expect("in-memory DB");
    let result = db.refs("nonexistent", None).expect("query");
    assert!(result.is_empty());
}

#[test]
fn empty_db_callees_returns_empty() {
    let db = Database::open_memory().expect("in-memory DB");
    let result = db.callees("nonexistent").expect("query");
    assert!(result.is_empty());
}

#[test]
fn empty_db_impact_returns_empty() {
    let db = Database::open_memory().expect("in-memory DB");
    let result = db.impact("nonexistent", 3).expect("query");
    assert!(result.is_empty());
}

#[test]
fn empty_db_hierarchy_returns_empty() {
    let db = Database::open_memory().expect("in-memory DB");
    let result = db.hierarchy("nonexistent").expect("query");
    assert!(result.is_empty());
}

#[test]
fn empty_db_deps_returns_empty() {
    let db = Database::open_memory().expect("in-memory DB");
    let result = db.file_deps("nonexistent.py").expect("query");
    assert!(result.is_empty());
}

#[test]
fn empty_db_search_returns_empty() {
    let db = Database::open_memory().expect("in-memory DB");
    let result = db.search("foo", None, None, 20).expect("query");
    assert!(result.is_empty());
}

#[test]
fn did_you_mean_suffix_lists_candidates() {
    let cands = vec!["ReviewResult".to_string(), "ReviewComment".to_string()];
    let suffix = did_you_mean_suffix("Revie", &cands).expect("suffix");
    assert!(suffix.contains("Did you mean: ReviewResult, ReviewComment"));
    assert!(suffix.contains("cartog_search"));
}

#[test]
fn did_you_mean_suffix_none_on_exact_match() {
    let cands = vec!["ReviewResult".to_string()];
    assert!(did_you_mean_suffix("ReviewResult", &cands).is_none());
}

#[test]
fn did_you_mean_suffix_none_without_candidates() {
    assert!(did_you_mean_suffix("Whatever", &[]).is_none());
}

#[test]
fn stale_banner_none_when_not_stale() {
    assert!(stale_banner(Some(snap(0, 10, 10)), "cartog_rag_search").is_none());
    assert!(stale_banner(None, "cartog_rag_search").is_none());
}

#[test]
fn stale_banner_rag_only_warns_semantic_tools() {
    // Pending embeddings warn rag_search/context...
    assert!(stale_banner(Some(snap(3, 10, 10)), "cartog_rag_search").is_some());
    assert!(stale_banner(Some(snap(3, 10, 10)), "cartog_context").is_some());
    // ...but not a structural tool (no debounce gap here).
    assert!(stale_banner(Some(snap(3, 10, 10)), "cartog_refs").is_none());
}

#[test]
fn stale_banner_structural_warns_every_read_tool() {
    // A change after the last reindex warns refs and rag_search alike.
    assert!(stale_banner(Some(snap(0, 20, 10)), "cartog_refs").is_some());
    assert!(stale_banner(Some(snap(0, 20, 10)), "cartog_rag_search").is_some());
}

#[test]
fn search_limit_is_capped() {
    assert_eq!(999u32.min(MAX_SEARCH_LIMIT), MAX_SEARCH_LIMIT);
    assert_eq!(30u32.min(MAX_SEARCH_LIMIT), 30);
}

#[test]
fn empty_db_stats_returns_zeros() {
    let db = Database::open_memory().expect("in-memory DB");
    let stats = db.stats().expect("query");
    assert_eq!(stats.num_files, 0);
    assert_eq!(stats.num_symbols, 0);
    assert_eq!(stats.num_edges, 0);
    assert_eq!(stats.num_resolved, 0);
}

// ── Response serialization tests ──

#[test]
fn ref_entry_serializes() {
    let entry = RefEntry {
        edge: cartog_core::Edge::new("src:foo:1", "bar", EdgeKind::Calls, "src/main.py", 10),
        source: None,
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    assert!(json.contains("\"bar\""));
    assert!(json.contains("\"calls\""));
}

#[test]
fn impact_entry_serializes() {
    let entry = ImpactEntry {
        edge: cartog_core::Edge::new("src:foo:1", "bar", EdgeKind::Calls, "src/main.py", 10),
        depth: 2,
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    assert!(json.contains("\"depth\":2"));
}

#[test]
fn hierarchy_entry_serializes() {
    let entry = HierarchyEntry {
        child: "Dog".to_string(),
        parent: "Animal".to_string(),
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    assert!(json.contains("\"Dog\""));
    assert!(json.contains("\"Animal\""));
}
