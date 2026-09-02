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

/// RAII override of `CARTOG_REGISTRY`, restoring the previous value on drop.
///
/// Mandatory for every test here: the registry is **user-global**, so a test
/// that sets the var without restoring it — or that runs in parallel with one
/// that does — reads and writes the developer's own registry. `#[serial]` on
/// each test closes the parallel half; this guard closes the leak half,
/// including on panic.
struct RegistryEnv(Option<std::ffi::OsString>);

impl RegistryEnv {
    fn set(value: &std::ffi::OsStr) -> Self {
        let prev = std::env::var_os(cartog_registry::REGISTRY_ENV);
        std::env::set_var(cartog_registry::REGISTRY_ENV, value);
        Self(prev)
    }

    /// Point at a fresh registry inside `dir`.
    fn isolated(dir: &std::path::Path) -> Self {
        Self::set(dir.join("projects.sqlite").as_os_str())
    }
}

impl Drop for RegistryEnv {
    fn drop(&mut self) {
        match self.0.take() {
            Some(v) => std::env::set_var(cartog_registry::REGISTRY_ENV, v),
            None => std::env::remove_var(cartog_registry::REGISTRY_ENV),
        }
    }
}

/// `cartog_list_projects` must work on a degraded server. A degraded server has
/// no index for *this* project, which is exactly when knowing what else is
/// indexed on the machine is most useful — refusing would make the tool useless
/// precisely when it matters.
///
/// This is the one test here that must call the real tool end-to-end (the claim
/// is that it does not *refuse*), so it is also the only one that touches
/// `CARTOG_REGISTRY`. The rest drive `build_result` directly — see the note on
/// `a_degraded_server_flags_no_project_as_current`.
#[tokio::test]
#[serial_test::serial]
async fn degraded_server_still_lists_projects() {
    let home = tempfile::TempDir::new().unwrap();
    let _env = RegistryEnv::isolated(home.path());

    let server = degraded_server();
    let result = server
        .cartog_list_projects()
        .await
        .expect("a degraded server must still answer list_projects, not refuse");

    // An outputSchema tool must always return structuredContent.
    assert!(
        result.structured_content.is_some(),
        "the tool declares an output schema, so structured content is mandatory"
    );
}

/// A degraded server has no on-disk database, so it has no registry identity —
/// nothing it lists can be flagged `current`.
///
/// Drives the tool's pure conversion directly rather than through
/// `CARTOG_REGISTRY`: this crate has two independent test-serialization
/// mechanisms (`#[serial]` and the tokio `SERIAL` mutex), so a test that mutates
/// a process-global env var cannot be reliably isolated from the other set.
#[test]
fn a_degraded_server_flags_no_project_as_current() {
    let listing = cartog_registry::Listing {
        projects: vec![fake_row("other", "/w/other/.cartog/db.sqlite")],
        available: true,
    };

    // A degraded server's db_path is empty (its DB is in memory).
    let result = crate::tools::projects::build_result(&listing, std::path::Path::new(""));

    assert_eq!(result.projects.len(), 1, "the row must still list");
    assert!(
        !result.projects[0].current,
        "a degraded server serves no project, so nothing is `current`"
    );
}

/// The serving project is flagged `current` so an agent does not re-route to
/// itself, and matching is by slot so an equivalent path still matches.
#[test]
fn the_served_project_is_flagged_current_even_via_an_equivalent_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("mine");
    std::fs::create_dir_all(root.join(".cartog")).unwrap();
    let db = root.join(".cartog").join("db.sqlite");
    std::fs::write(&db, b"").unwrap();

    let listing = cartog_registry::Listing {
        projects: vec![
            fake_row("mine", &db.to_string_lossy()),
            fake_row("other", "/w/other/.cartog/db.sqlite"),
        ],
        available: true,
    };

    // Serve the same physical DB through a path with a redundant component.
    let equivalent = root.join(".").join(".cartog").join("db.sqlite");
    let result = crate::tools::projects::build_result(&listing, &equivalent);

    let mine = result.projects.iter().find(|p| p.name == "mine").unwrap();
    let other = result.projects.iter().find(|p| p.name == "other").unwrap();
    assert!(
        mine.current,
        "an equivalent path to the served DB must still flag `current`"
    );
    assert!(!other.current);
}

/// An unavailable registry is reported as such, so an agent cannot misread it
/// as "no other projects exist".
#[test]
fn an_unavailable_registry_is_reported_not_silently_empty() {
    let result = crate::tools::projects::build_result(
        &cartog_registry::Listing::unavailable(),
        std::path::Path::new(""),
    );
    assert!(!result.registry_available);
    assert!(result.projects.is_empty());
}

/// The tool must never open a foreign project's database.
///
/// Asserted by observable effect: a row whose `db_path` points at a file that is
/// not valid SQLite must still convert and list. If the conversion opened
/// project databases, this row would error or vanish.
#[test]
fn list_projects_opens_no_foreign_database() {
    let dir = tempfile::TempDir::new().unwrap();
    let fake_db = dir.path().join("not-a-database.sqlite");
    std::fs::write(&fake_db, b"definitely not sqlite").unwrap();

    let listing = cartog_registry::Listing {
        projects: vec![fake_row("bogus", &fake_db.to_string_lossy())],
        available: true,
    };

    let result = crate::tools::projects::build_result(&listing, std::path::Path::new(""));

    assert_eq!(result.projects.len(), 1);
    assert_eq!(result.projects[0].name, "bogus");
}

/// A registry row with the shape the reader produces.
fn fake_row(name: &str, db_path: &str) -> cartog_registry::ProjectRow {
    cartog_registry::ProjectRow {
        id: cartog_registry::slot_for_db("serve", std::path::Path::new(db_path)),
        db_path: db_path.into(),
        root: format!("/w/{name}").into(),
        name: name.to_string(),
        languages: vec![("rust".to_string(), 10)],
        schema_version: Some(cartog_db::CURRENT_SCHEMA_VERSION),
        file_count: Some(10),
        symbol_count: Some(100),
        edge_count: Some(200),
        resolved_count: Some(150),
        embedding_count: Some(100),
        embed_provider: None,
        embed_model: None,
        embed_dim: None,
        last_indexed: Some(1_700_000_000),
        last_seen: 1_700_000_100,
        markers: cartog_registry::Markers::default(),
    }
}

/// A registry holding many projects must not blow the response budget.
///
/// Both halves matter: the text block AND `structuredContent`. Trimming only
/// the text would leave the structured half unbounded, which is the defect
/// PR #151 fixed for the other outputSchema tools.
#[test]
fn a_large_project_list_is_trimmed_and_says_so() {
    let listing = cartog_registry::Listing {
        projects: (0..400)
            .map(|i| {
                fake_row(
                    &format!("project-{i}"),
                    &format!("/w/p{i}/.cartog/db.sqlite"),
                )
            })
            .collect(),
        available: true,
    };

    let result = crate::tools::projects::build_result(&listing, std::path::Path::new(""));
    let total = result.projects.len();
    assert_eq!(total, 400, "precondition: all rows convert");

    // Call the handler's own trim, so removing it from the handler fails here.
    let mut trimmed = result;
    let omitted = crate::tools::projects::trim_to_budget(&mut trimmed);

    assert!(omitted > 0, "400 projects must exceed the list budget");
    assert_eq!(
        trimmed.projects.len() + omitted,
        total,
        "nothing may be lost silently"
    );

    // The structured half is bounded by the same trim, so it cannot diverge —
    // and it is never re-clamped downstream, so this is its only bound.
    let structured = serde_json::to_string_pretty(&trimmed).unwrap();
    assert!(
        structured.len() <= crate::mcp_max_bytes(),
        "structuredContent must stay under the response cap, got {} bytes",
        structured.len()
    );
}

/// The MCP surface is 17 tools. Pinned so a router that silently loses a block
/// (a `mod` line dropped, a `+ Self::x_router()` removed) fails here rather
/// than by a client mysteriously not seeing a tool.
#[test]
fn the_tool_router_exposes_seventeen_tools() {
    let tools = CartogServer::tool_router().list_all();
    assert_eq!(
        tools.len(),
        17,
        "expected 17 MCP tools, got {}: {:?}",
        tools.len(),
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>()
    );
    assert!(
        tools.iter().any(|t| t.name == "cartog_list_projects"),
        "cartog_list_projects must be routed"
    );
}
