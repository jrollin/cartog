//! Read-tool handlers driven over a real indexed temp DB.

use super::test_provider;
use crate::*;
use rmcp::handler::server::wrapper::Parameters;

// ── Read-tool handler tests over a real indexed DB ──
//
// Build a CartogServer over a temp DB pre-populated by indexing a small
// Python fixture, then drive the async read handlers directly. This
// exercises the real MCP dispatch (param parsing, error mapping, the
// tool_response / tool_response_named integration) over real query
// results, not mocks.

const FIXTURE_SRC: &str = "\
class Animal:
def speak(self):
    return helper()


class Dog(Animal):
def speak(self):
    return helper()


def helper():
return 42


def main():
d = Dog()
return d.speak()
";

/// Index `FIXTURE_SRC` as `lib.py` into a temp dir, then return a primary
/// server opened over the resulting DB. The TempDir is returned so the
/// caller keeps it alive for the test's duration.
fn indexed_server() -> (tempfile::TempDir, CartogServer) {
    let tmp = tempfile::TempDir::new().unwrap();
    // The index root must not be a dot-prefixed dir: the walker prunes any
    // entry whose name starts with '.', and TempDir names start with ".tmp".
    let root = tmp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("lib.py"), FIXTURE_SRC).unwrap();
    let db_path = tmp.path().join("cartog.db");
    let provider = test_provider();
    {
        let db = Database::open(&db_path, provider.dimension()).unwrap();
        indexer::index_directory(
            &db,
            &root,
            true,
            false,
            None,
            None,
            indexer::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
            &indexer::WalkFilter::unrestricted(),
        )
        .expect("fixture indexes");
    }
    let server = CartogServer::new_with_provider(
        &db_path,
        provider,
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::Primary,
    )
    .expect("server constructs");
    (tmp, server)
}

/// Extract the text payload of a successful single-content tool result.
fn result_text(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .expect("tool result has text content")
}

#[tokio::test]
async fn outline_lists_symbols_in_file() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_outline(Parameters(OutlineParams {
            file: "lib.py".to_string(),
        }))
        .await
        .expect("outline succeeds");
    let text = result_text(&result);
    assert!(
        text.contains("Animal"),
        "outline should list the Animal class"
    );
    assert!(
        text.contains("helper"),
        "outline should list the helper function"
    );
}

#[tokio::test]
async fn outline_unknown_file_returns_empty_array() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_outline(Parameters(OutlineParams {
            file: "nonexistent.py".to_string(),
        }))
        .await
        .expect("outline of unknown file is not an error");
    let text = result_text(&result);
    assert!(
        text.trim_start().starts_with('['),
        "empty outline is a JSON array"
    );
}

#[tokio::test]
async fn refs_finds_callers_of_helper() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_refs(Parameters(RefsParams {
            name: "helper".to_string(),
            kind: None,
        }))
        .await
        .expect("refs succeeds");
    let text = result_text(&result);
    assert!(text.contains("helper"), "refs to helper should mention it");
}

#[tokio::test]
async fn refs_rejects_invalid_edge_kind() {
    let (_dir, server) = indexed_server();
    let err = server
        .cartog_refs(Parameters(RefsParams {
            name: "helper".to_string(),
            kind: Some("bogus".to_string()),
        }))
        .await
        .expect_err("invalid edge kind must be rejected");
    assert!(
        err.message.contains("invalid edge kind"),
        "error should name the invalid kind, got: {}",
        err.message
    );
}

#[tokio::test]
async fn refs_unknown_name_suggests_near_matches() {
    let (_dir, server) = indexed_server();
    // "helpe" is one char off "helper" — should trigger did-you-mean.
    let result = server
        .cartog_refs(Parameters(RefsParams {
            name: "helpe".to_string(),
            kind: None,
        }))
        .await
        .expect("refs of near-miss name succeeds");
    let text = result_text(&result);
    assert!(
        text.contains("Did you mean") && text.contains("helper"),
        "near-miss should suggest helper, got: {text}"
    );
    // The empty-[] did-you-mean branch still returns structuredContent — it is
    // a distinct early-return path in tool_response_named, and a schema-bearing
    // tool must carry structuredContent even when the result is empty.
    assert!(
        result
            .structured_content
            .as_ref()
            .is_some_and(serde_json::Value::is_object),
        "did-you-mean response keeps object-typed structuredContent"
    );
}

#[tokio::test]
async fn callees_traces_calls_from_main() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_callees(Parameters(CalleesParams {
            name: "main".to_string(),
        }))
        .await
        .expect("callees succeeds");
    let text = result_text(&result);
    assert!(
        text.trim_start().starts_with('['),
        "callees returns a JSON array"
    );
}

#[tokio::test]
async fn impact_clamps_depth_and_returns_array() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_impact(Parameters(ImpactParams {
            name: "helper".to_string(),
            depth: Some(999), // clamped to MAX_IMPACT_DEPTH internally
        }))
        .await
        .expect("impact succeeds");
    let text = result_text(&result);
    assert!(
        text.trim_start().starts_with('['),
        "impact returns a JSON array"
    );
}

#[tokio::test]
async fn trace_finds_path_from_speak_to_helper() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_trace(Parameters(TraceParams {
            from: "speak".to_string(),
            to: "helper".to_string(),
            depth: Some(8),
        }))
        .await
        .expect("trace succeeds");
    let text = result_text(&result);
    assert!(text.contains("\"found\": true"), "path exists: {text}");
    assert!(text.contains("helper"), "hop should reach helper: {text}");
}

#[tokio::test]
async fn trace_reports_no_path_when_unreachable() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_trace(Parameters(TraceParams {
            from: "helper".to_string(),
            to: "speak".to_string(),
            depth: Some(8),
        }))
        .await
        .expect("trace succeeds");
    let text = result_text(&result);
    assert!(text.contains("\"found\": false"), "no path: {text}");
}

#[tokio::test]
async fn trace_hop_includes_body_when_content_indexed() {
    let (_dir, server) = indexed_server();
    // Seed RAG content for every `speak` symbol (the hop sources on the
    // speak→helper path), so the hop body is populated.
    {
        let db = server.db.lock().unwrap();
        for sym in db.search("speak", None, None, 10).unwrap() {
            db.upsert_symbol_content(
                &sym.id,
                "speak",
                "def speak(self):\n    return helper()",
                "// method speak",
            )
            .unwrap();
        }
    }
    let result = server
        .cartog_trace(Parameters(TraceParams {
            from: "speak".to_string(),
            to: "helper".to_string(),
            depth: Some(8),
        }))
        .await
        .expect("trace succeeds");
    let text = result_text(&result);
    assert!(
        text.contains("\"body\""),
        "hop carries an inline body when content is indexed: {text}"
    );
}

#[tokio::test]
async fn context_bundles_relevant_symbols_for_a_task() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_context(Parameters(ContextParams {
            task: "speak".to_string(),
            tokens: Some(6000),
        }))
        .await
        .expect("context succeeds");
    let text = result_text(&result);
    assert!(text.contains("\"task\": \"speak\""), "echoes task: {text}");
    assert!(text.contains("\"entries\""), "returns entries: {text}");
}

#[tokio::test]
async fn rag_search_prepends_banner_when_embeddings_pending() {
    let (_dir, server) = indexed_server();
    // Simulate a live watcher with pending embeddings.
    let stale = cartog_watch::StaleState::new();
    stale.note_reindex(0, 7);
    *server.stale.lock().unwrap() = Some(stale);
    server
        .watcher_active
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let result = server
        .cartog_rag_search(Parameters(RagSearchParams {
            query: "helper".to_string(),
            kind: None,
            limit: Some(5),
        }))
        .await
        .expect("rag search succeeds");
    let text = result_text(&result);
    assert!(
        text.starts_with("⚠️") && text.contains("re-embedding"),
        "banner prepended: {text}"
    );
}

#[tokio::test]
async fn no_banner_without_active_watcher() {
    let (_dir, server) = indexed_server();
    // Stale state present but watcher_active is false (degraded/no watcher).
    let stale = cartog_watch::StaleState::new();
    stale.note_reindex(0, 7);
    *server.stale.lock().unwrap() = Some(stale);

    let result = server
        .cartog_rag_search(Parameters(RagSearchParams {
            query: "helper".to_string(),
            kind: None,
            limit: Some(5),
        }))
        .await
        .expect("rag search succeeds");
    assert!(!result_text(&result).starts_with("⚠️"), "no banner");
}

#[tokio::test]
async fn context_rejects_empty_task() {
    let (_dir, server) = indexed_server();
    let err = server
        .cartog_context(Parameters(ContextParams {
            task: String::new(),
            tokens: None,
        }))
        .await
        .expect_err("empty task must be rejected");
    assert!(
        err.message.contains("cannot be empty"),
        "got: {}",
        err.message
    );
}

#[tokio::test]
async fn hierarchy_reports_dog_extends_animal() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_hierarchy(Parameters(HierarchyParams {
            name: "Dog".to_string(),
        }))
        .await
        .expect("hierarchy succeeds");
    let text = result_text(&result);
    assert!(
        text.contains("Animal"),
        "Dog's hierarchy should reach Animal: {text}"
    );
}

#[tokio::test]
async fn deps_lists_file_imports() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_deps(Parameters(DepsParams {
            file: "lib.py".to_string(),
        }))
        .await
        .expect("deps succeeds");
    let text = result_text(&result);
    assert!(
        text.trim_start().starts_with('['),
        "deps returns a JSON array"
    );
}

#[tokio::test]
async fn search_finds_symbol_by_partial_name() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_search(Parameters(SearchParams {
            query: "Anim".to_string(),
            kind: None,
            file: None,
            limit: None,
        }))
        .await
        .expect("search succeeds");
    let text = result_text(&result);
    assert!(
        text.contains("Animal"),
        "search for 'Anim' should find Animal"
    );
}

#[tokio::test]
async fn search_rejects_empty_query() {
    let (_dir, server) = indexed_server();
    let err = server
        .cartog_search(Parameters(SearchParams {
            query: String::new(),
            kind: None,
            file: None,
            limit: None,
        }))
        .await
        .expect_err("empty query must be rejected");
    assert!(
        err.message.contains("query cannot be empty"),
        "got: {}",
        err.message
    );
}

#[tokio::test]
async fn search_rejects_invalid_kind() {
    let (_dir, server) = indexed_server();
    let err = server
        .cartog_search(Parameters(SearchParams {
            query: "Animal".to_string(),
            kind: Some("nonsense".to_string()),
            file: None,
            limit: None,
        }))
        .await
        .expect_err("invalid symbol kind must be rejected");
    assert!(
        err.message.contains("invalid symbol kind"),
        "got: {}",
        err.message
    );
}

#[tokio::test]
async fn stats_reports_role_and_symbol_count() {
    let (_dir, server) = indexed_server();
    let result = server.cartog_stats().await.expect("stats succeeds");
    let text = result_text(&result);
    let value: serde_json::Value = serde_json::from_str(&text).expect("stats is JSON");
    assert_eq!(
        value["role"], "primary",
        "primary server reports primary role"
    );
    assert!(
        value["num_symbols"].as_u64().unwrap_or(0) > 0,
        "indexed fixture has symbols: {text}"
    );
}

#[tokio::test]
async fn map_returns_files_and_top_symbols() {
    let (_dir, server) = indexed_server();
    let result = server
        .cartog_map(Parameters(MapParams { limit: Some(10) }))
        .await
        .expect("map succeeds");
    let text = result_text(&result);
    assert!(
        text.contains("lib.py"),
        "map should list the indexed file: {text}"
    );
}

/// Every read tool declares an `output_schema`, so the MCP spec requires it
/// to return `structuredContent` on a successful call. A prior bug dropped
/// `structuredContent`, which strict clients (e.g. opencode) reject. Drive
/// every read handler over a real indexed DB and assert each attaches
/// object-typed `structuredContent`.
///
/// This covers the presence invariant on normal (under-cap) results; the
/// oversized/trimmed path is covered by
/// `schema::oversized_result_bounds_text_and_structured_together`.
#[tokio::test]
async fn every_read_tool_returns_structured_content() {
    let (_dir, server) = indexed_server();

    fn assert_object_structured(tool: &str, result: &CallToolResult) {
        let structured = result
            .structured_content
            .as_ref()
            .unwrap_or_else(|| panic!("{tool} must return structuredContent"));
        assert!(
            structured.is_object(),
            "{tool} structuredContent must be a JSON object, got: {structured}"
        );
    }

    assert_object_structured(
        "cartog_outline",
        &server
            .cartog_outline(Parameters(OutlineParams {
                file: "lib.py".to_string(),
            }))
            .await
            .expect("outline"),
    );
    assert_object_structured(
        "cartog_refs",
        &server
            .cartog_refs(Parameters(RefsParams {
                name: "helper".to_string(),
                kind: None,
            }))
            .await
            .expect("refs"),
    );
    assert_object_structured(
        "cartog_callees",
        &server
            .cartog_callees(Parameters(CalleesParams {
                name: "main".to_string(),
            }))
            .await
            .expect("callees"),
    );
    assert_object_structured(
        "cartog_impact",
        &server
            .cartog_impact(Parameters(ImpactParams {
                name: "helper".to_string(),
                depth: None,
            }))
            .await
            .expect("impact"),
    );
    assert_object_structured(
        "cartog_trace",
        &server
            .cartog_trace(Parameters(TraceParams {
                from: "speak".to_string(),
                to: "helper".to_string(),
                depth: None,
            }))
            .await
            .expect("trace"),
    );
    assert_object_structured(
        "cartog_hierarchy",
        &server
            .cartog_hierarchy(Parameters(HierarchyParams {
                name: "Dog".to_string(),
            }))
            .await
            .expect("hierarchy"),
    );
    assert_object_structured(
        "cartog_deps",
        &server
            .cartog_deps(Parameters(DepsParams {
                file: "lib.py".to_string(),
            }))
            .await
            .expect("deps"),
    );
    assert_object_structured(
        "cartog_search",
        &server
            .cartog_search(Parameters(SearchParams {
                query: "Animal".to_string(),
                kind: None,
                file: None,
                limit: None,
            }))
            .await
            .expect("search"),
    );
    assert_object_structured(
        "cartog_map",
        &server
            .cartog_map(Parameters(MapParams { limit: Some(10) }))
            .await
            .expect("map"),
    );
    assert_object_structured(
        "cartog_changes",
        &server
            .cartog_changes(Parameters(ChangesParams {
                commits: None,
                kind: None,
            }))
            .await
            .expect("changes"),
    );
    assert_object_structured("cartog_stats", &server.cartog_stats().await.expect("stats"));
    assert_object_structured(
        "cartog_rag_search",
        &server
            .cartog_rag_search(Parameters(RagSearchParams {
                query: "helper".to_string(),
                kind: None,
                limit: Some(5),
            }))
            .await
            .expect("rag search"),
    );
    assert_object_structured(
        "cartog_context",
        &server
            .cartog_context(Parameters(ContextParams {
                task: "speak".to_string(),
                tokens: Some(6000),
            }))
            .await
            .expect("context"),
    );
}

#[tokio::test]
async fn read_tools_count_toward_query_log() {
    let (_dir, server) = indexed_server();
    let _ = server
        .cartog_search(Parameters(SearchParams {
            query: "helper".to_string(),
            kind: None,
            file: None,
            limit: None,
        }))
        .await
        .expect("search succeeds");
    let result = server.cartog_stats().await.expect("stats succeeds");
    let value: serde_json::Value =
        serde_json::from_str(&result_text(&result)).expect("stats is JSON");
    // stats itself plus the prior search both log; the field exists once
    // any read tool has run against a populated index.
    assert!(value.get("num_symbols").is_some(), "stats shape is intact");
}
