//! Consent-gate (degraded-start) behavior of the MCP server.
//!
//! A `cartog serve` started on a config-less, un-indexed repo with no
//! `CARTOG_AUTO_INIT` runs **degraded**: it created no `.cartog/`, holds an
//! empty in-memory DB, refuses the 2 write tools with a "run `cartog init`"
//! message, and reports the degraded state in `cartog_stats`. Read tools work
//! against the empty DB and naturally return empty results.

use super::test_provider;
use crate::*;

fn degraded_server() -> CartogServer {
    CartogServer::new_degraded_for_tests(
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
    )
    .expect("degraded server constructs")
}

#[test]
fn degraded_server_refuses_write_tools_with_init_hint() {
    let server = degraded_server();
    assert!(server.is_degraded(), "constructed degraded");

    for tool in ["cartog_index", "cartog_rag_index"] {
        let err = server
            .refuse_if_degraded(tool)
            .unwrap_or_else(|| panic!("degraded must refuse {tool}"));
        let msg = format!("{err:?}");
        assert!(msg.contains(tool), "refusal names the tool: {msg}");
        assert!(
            msg.contains("cartog init"),
            "refusal must point at `cartog init`: {msg}"
        );
        // Distinct from the read-only-secondary refusal.
        assert!(
            !msg.contains("read-only"),
            "degraded refusal must not be the read-only message: {msg}"
        );
    }
}

#[test]
fn non_degraded_primary_does_not_refuse() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let server = CartogServer::new_with_provider(
        &db_path,
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::Primary,
    )
    .expect("primary constructs");
    assert!(!server.is_degraded());
    assert!(
        server.refuse_if_degraded("cartog_index").is_none(),
        "a normally-opened primary must not refuse on the degraded gate"
    );
}

#[tokio::test]
async fn degraded_stats_reports_degraded_state() {
    let server = degraded_server();
    let result = server.cartog_stats().await.expect("stats succeeds");
    let text = result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .expect("stats has text");
    // Human banner names the degraded state and the fix.
    assert!(
        text.contains("no index yet") && text.contains("cartog init"),
        "degraded stats must lead with the no-index banner: {text}"
    );
    // Structured payload carries the flag.
    let structured = result
        .structured_content
        .expect("stats has structured content");
    assert_eq!(
        structured.get("degraded").and_then(|v| v.as_bool()),
        Some(true),
        "structured stats must set degraded=true"
    );
}

#[test]
fn read_only_secondary_against_absent_db_starts_degraded_not_error() {
    // A 2nd serve peer that loses election while the primary is degraded (no DB
    // on disk) must start degraded too, not fail open_readonly and exit — else
    // the serve-for-all-clients flow leaves the 2nd client with no MCP server.
    let dir = tempfile::TempDir::new().unwrap();
    let absent = dir.path().join(".cartog").join("db.sqlite");
    let server = CartogServer::new_with_provider(
        &absent,
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::ReadOnly,
    )
    .expect("read-only attach against absent DB must not error");
    assert!(server.is_degraded(), "absent DB → degraded secondary");
    assert!(
        !absent.parent().unwrap().exists(),
        "the degraded secondary must not create .cartog/"
    );
    assert!(
        server.refuse_if_degraded("cartog_index").is_some(),
        "degraded secondary still refuses write tools"
    );
}

#[test]
fn open_existing_drives_new_degraded_decision_without_creating_dir() {
    // `CartogServer::new(allow_create=false)` keys its degraded branch on
    // open_existing returning NotFound (it can't run here — `new` loads ONNX;
    // the full degraded startup is covered by
    // `read_only_secondary_against_absent_db_starts_degraded_not_error` via the
    // injectable provider). This pins the decision input: NotFound + no dir.
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join(".cartog").join("db.sqlite");

    assert!(matches!(
        Database::open_existing(&db_path, rag::EMBEDDING_DIM),
        Err(cartog_db::DbError::NotFound { .. })
    ));
    assert!(
        !db_path.parent().unwrap().exists(),
        "the consent gate must not materialize .cartog/ for an un-opted repo"
    );
}
