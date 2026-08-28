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

/// No degraded surface may claim the config is *absent*. A present-but-rejected
/// `.cartog.toml` lands in the same degraded state, so "this project has no
/// .cartog.toml" tells a user looking straight at one that it isn't there — the
/// exact failure the consent-gate fix exists to remove. The stats banner kept
/// that wording after its two siblings were corrected.
#[tokio::test]
async fn no_degraded_message_claims_the_config_is_absent() {
    let server = degraded_server();
    let stats = server.cartog_stats().await.expect("stats succeeds");
    let banner = stats
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .expect("stats has text");
    let refusal = format!("{:?}", server.refuse_if_degraded("cartog_index").unwrap());

    for (surface, msg) in [("stats banner", &banner), ("write refusal", &refusal)] {
        assert!(
            !msg.contains("has no .cartog.toml") && !msg.contains("no .cartog.toml in"),
            "{surface} claims the config is absent; say \"no usable .cartog.toml\" \
             so a rejected-config user isn't told their file doesn't exist: {msg}"
        );
        assert!(
            msg.contains("no usable .cartog.toml"),
            "{surface} must use the agreed \"no usable .cartog.toml\" wording: {msg}"
        );
    }
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

/// Constructing a server must not build the cross-encoder. That deferral is the
/// entire ~162 MB idle-memory win, and its only other guard (`make bench-memory`)
/// runs on macOS alone and in no CI job — so without this assertion a change that
/// re-eagers the build ships green everywhere.
#[test]
fn server_construction_does_not_build_the_reranker() {
    let dir = tempfile::TempDir::new().unwrap();
    let server = CartogServer::new_with_provider(
        &dir.path().join("test.db"),
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::Primary,
    )
    .expect("primary constructs");
    assert!(
        !server.reranker_is_loaded(),
        "the cross-encoder must stay unbuilt until the first semantic query"
    );
    assert!(
        !degraded_server().reranker_is_loaded(),
        "a degraded server must not build the cross-encoder either"
    );
}

/// The production wiring must be lazy too — the test constructors inject a
/// no-op reranker, so they cannot catch `lazy_reranker` itself being changed to
/// build at construction.
#[test]
fn production_reranker_wiring_is_lazy_at_construction() {
    let lazy = crate::lazy_provider::lazy_reranker(rag::EmbeddingProviderConfig::default());
    assert!(
        !lazy.is_loaded(),
        "lazy_reranker must not build until first use — this is the wiring the \
         real server uses, unlike the test harness's no_reranker()"
    );
}
