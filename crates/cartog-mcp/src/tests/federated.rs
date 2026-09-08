//! Opt-in exposure of the two cross-project tools.
//!
//! `cartog_list_projects` and `cartog_search_all` are the only tools that read
//! other repositories' paths and README text into a session, so a freshly
//! constructed server hides them and `with_federated(true)` is the sole way to
//! expose them. Asserted over a real client/server pair, not the static
//! router: `#[tool_handler]` must read the per-instance field for the switch
//! to reach a client at all.

use super::test_provider;
use crate::*;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;

fn server(federated: bool) -> CartogServer {
    CartogServer::new_degraded_for_tests(
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
    )
    .expect("server constructs")
    .with_federated(federated)
}

/// In-process client over a duplex transport, so `tools/list` and
/// `tools/call` go through the handler exactly as a client's would.
async fn connect(server: CartogServer) -> RunningService<RoleClient, ()> {
    let (server_t, client_t) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_t).await {
            let _ = running.waiting().await;
        }
    });
    ().serve(client_t).await.expect("client connects")
}

async fn listed_tools(client: &RunningService<RoleClient, ()>) -> Vec<String> {
    client
        .list_tools(None)
        .await
        .expect("tools/list succeeds")
        .tools
        .into_iter()
        .map(|t| t.name.to_string())
        .collect()
}

#[tokio::test]
async fn a_client_sees_sixteen_tools_unless_federated() {
    let client = connect(server(false)).await;
    let names = listed_tools(&client).await;
    assert_eq!(names.len(), 16, "default surface is 16 tools: {names:?}");
    for hidden in FEDERATED_TOOLS {
        assert!(
            !names.iter().any(|n| n == hidden),
            "{hidden} must be hidden"
        );
    }
}

#[tokio::test]
async fn a_federated_client_sees_all_eighteen_tools() {
    let client = connect(server(true)).await;
    let names = listed_tools(&client).await;
    assert_eq!(names.len(), 18, "federated surface is 18 tools: {names:?}");
    for shown in FEDERATED_TOOLS {
        assert!(names.iter().any(|n| n == shown), "{shown} must be listed");
    }
}

/// The flag and the router are set together in `with_federated`, so nothing
/// else pins that they agree. A future edit that sets one and forgets the other
/// would make `get_info` advertise a tool the router dropped (or hide one it
/// kept), which no count assertion above would catch.
#[tokio::test]
async fn the_federated_flag_and_the_routed_surface_agree() {
    for federated in [false, true] {
        let server = server(federated);
        assert_eq!(
            server.is_federated(),
            federated,
            "with_federated({federated}) must set the flag"
        );

        let client = connect(server).await;
        let listed = listed_tools(&client).await;
        for name in FEDERATED_TOOLS {
            assert_eq!(
                listed.iter().any(|n| n == name),
                federated,
                "{name} routed must match the flag ({federated})"
            );
        }
    }
}

/// Hidden means not callable, not merely unlisted: a client that remembers the
/// name from a federated session gets an error, never another project's data.
///
/// Isolated + serialized even though the route is expected to be gone: the
/// assertion is that the handler never runs, and a regression that re-exposed
/// it would otherwise read the developer's own `projects.sqlite` while staying
/// green. The guard makes the failure mode a wrong-answer, not a data leak.
#[tokio::test]
#[serial_test::serial]
async fn a_hidden_cross_project_tool_is_not_callable() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let _env = super::RegistryEnv::isolated(home.path());
    let client = connect(server(false)).await;
    let err = client
        .call_tool(CallToolRequestParams::new("cartog_list_projects"))
        .await
        .expect_err("a hidden tool must not be callable");
    // rmcp answers an unrouted name with its generic "tool not found"; the
    // point is that no handler ran, so no foreign project data came back.
    assert!(
        format!("{err:?}").contains("tool not found"),
        "expected rmcp's unrouted-tool error, got: {err:?}"
    );
}

/// The instructions are the agent's first routing hint; naming a tool the
/// instance hides would send it straight into a "tool not found" error.
#[tokio::test]
async fn instructions_name_cartog_list_projects_only_when_federated() {
    let client = connect(server(false)).await;
    let text = client
        .peer_info()
        .and_then(|i| i.instructions.clone())
        .expect("instructions set");
    assert!(
        !text.contains("cartog_list_projects"),
        "non-federated instructions must not name a hidden tool: {text}"
    );

    let client = connect(server(true)).await;
    let text = client
        .peer_info()
        .and_then(|i| i.instructions.clone())
        .expect("instructions set");
    assert!(
        text.contains("cartog_list_projects"),
        "federated instructions name the cross-project entry point: {text}"
    );
}
