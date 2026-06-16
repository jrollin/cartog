//! End-to-end test for MCP `notifications/progress` on the indexing tools.
//!
//! The unit tests in `progress.rs` exercise the forwarder with a stub
//! `Notifier`; this module closes the remaining gap by driving the real
//! server through an in-process rmcp client over a duplex transport, so the
//! actual `peer.notify_progress` path is covered end to end.
//!
//! Note on the gate: the server only emits progress when the request carries a
//! `progressToken` in `_meta` (`cartog_index` handler, `lib.rs`). The rmcp
//! client SDK injects a fresh token on *every* request
//! (`Peer::send_request_with_option`), so a tokenless request is unreachable
//! from a real client — exactly mirroring production, where Cursor and Copilot
//! always supply one. We therefore assert the positive contract (frames flow
//! and are scoped to the client's token) rather than a tokenless case a real
//! client can never produce.

#![cfg(test)]

use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, ClientRequest, ProgressNotificationParam, ProgressToken, Request,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RunningService};
use rmcp::{ClientHandler, RoleClient, ServiceExt};

use crate::indexer::{RedactionConfig, WalkFilter};
use crate::{rag, CartogServer, Role};

/// Client handler that records every progress notification it receives.
/// Dependency-free alternative to rmcp's `ProgressDispatcher` stream: we only
/// need to assert on the collected frames after the call returns.
#[derive(Clone, Default)]
struct CapturingClient {
    frames: Arc<Mutex<Vec<ProgressNotificationParam>>>,
}

impl ClientHandler for CapturingClient {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _ctx: NotificationContext<RoleClient>,
    ) {
        self.frames.lock().unwrap().push(params);
    }
}

/// Deterministic mock provider sized to the default embedding dimension, so
/// the test never loads the real ONNX model (absent in CI coverage runners).
fn test_provider() -> Box<dyn rag::provider::EmbeddingProvider> {
    Box::new(rag::provider::test_utils::MockEmbeddingProvider::new(
        rag::EMBEDDING_DIM,
    ))
}

/// A tiny source tree the indexer can walk fast. Created **under the server's
/// cwd** because `cartog_index` validates that the indexed path is contained
/// in cwd; an absolute tempdir elsewhere would be rejected. Returns the
/// `TempDir` (keep it alive) and the relative path to pass as the tool arg.
fn fixture_under_cwd() -> (tempfile::TempDir, String) {
    let cwd = std::env::current_dir().expect("cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir under cwd");
    std::fs::write(
        dir.path().join("a.py"),
        "def greet(name):\n    return f'hi {name}'\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.py"),
        "from a import greet\n\ndef main():\n    return greet('world')\n",
    )
    .unwrap();
    let rel = dir
        .path()
        .strip_prefix(&cwd)
        .expect("tempdir is under cwd")
        .to_string_lossy()
        .into_owned();
    (dir, rel)
}

/// Spin up an in-process client/server pair over a duplex transport. The
/// server is a writable primary backed by the mock provider. Returns the
/// running client service (its `service()` is the [`CapturingClient`]).
async fn connect(db_path: &std::path::Path) -> RunningService<RoleClient, CapturingClient> {
    let server = CartogServer::new_with_provider(
        db_path,
        test_provider(),
        RedactionConfig::disabled(),
        WalkFilter::unrestricted(),
        Role::Primary,
    )
    .expect("server constructs");

    let (server_t, client_t) = tokio::io::duplex(8 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_t).await {
            let _ = running.waiting().await;
        }
    });
    CapturingClient::default()
        .serve(client_t)
        .await
        .expect("client connects")
}

/// Build a `cartog_index` tool request for `rel_path` with `force = true`.
fn index_request(rel_path: &str) -> ClientRequest {
    let args = serde_json::json!({ "path": rel_path, "force": true })
        .as_object()
        .cloned()
        .expect("args object");
    ClientRequest::CallToolRequest(Request::new(
        CallToolRequestParams::new("cartog_index").with_arguments(args),
    ))
}

#[tokio::test]
async fn index_emits_progress_frames_scoped_to_the_client_token() {
    let db_dir = tempfile::TempDir::new().unwrap();
    let db_path = db_dir.path().join("test.db");
    let (_fixture, rel_path) = fixture_under_cwd();

    let client = connect(&db_path).await;

    // `send_cancellable_request` is the seam that exposes the SDK-assigned
    // progressToken; the server echoes exactly this token on every frame.
    let handle = client
        .send_cancellable_request(index_request(&rel_path), PeerRequestOptions::no_options())
        .await
        .expect("request sent");
    let token: ProgressToken = handle.progress_token.clone();
    handle.await_response().await.expect("index completes");

    let frames = client.service().frames.lock().unwrap().clone();
    assert!(
        !frames.is_empty(),
        "expected progress frames when a progressToken is supplied"
    );
    // Every frame is scoped to the token the client sent (the server's gate at
    // the `cartog_index` handler keys notifications to the request token).
    for f in &frames {
        assert_eq!(f.progress_token, token, "frame used a foreign token");
    }
    // Spec: progress strictly increases with each notification (the forwarder
    // drops duplicate/straggler frames that would tie the last emitted value).
    for w in frames.windows(2) {
        assert!(
            w[0].progress < w[1].progress,
            "progress did not strictly increase: {} -> {}",
            w[0].progress,
            w[1].progress
        );
    }
    // The first phase the indexer reports is the file scan.
    assert_eq!(frames[0].message.as_deref(), Some("scanning files"));
    // A parsing phase is reported for the fixture files.
    assert!(
        frames.iter().any(|f| f
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("parsing"))),
        "expected a parsing phase, got {:?}",
        frames
            .iter()
            .filter_map(|f| f.message.clone())
            .collect::<Vec<_>>()
    );

    let _ = client.cancel().await;
}

#[tokio::test]
async fn index_progress_messages_match_the_documented_phase_vocabulary() {
    // Guards the docs/usage.md contract: the client-facing phase messages are
    // `scanning files`, `parsing M/N files`, `storing M/N files` (and, when an
    // LSP server is present, `resolving M/N edges with LSP`). A counting phase
    // must also carry a determinate `total` so a client can render a bar.
    let db_dir = tempfile::TempDir::new().unwrap();
    let db_path = db_dir.path().join("test.db");
    let (_fixture, rel_path) = fixture_under_cwd();

    let client = connect(&db_path).await;
    let handle = client
        .send_cancellable_request(index_request(&rel_path), PeerRequestOptions::no_options())
        .await
        .expect("request sent");
    handle.await_response().await.expect("index completes");

    let messages: Vec<String> = client
        .service()
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter_map(|f| f.message.clone())
        .collect();
    assert!(
        messages.iter().any(|m| m == "scanning files"),
        "missing scan phase in {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.starts_with("storing")),
        "missing store phase in {messages:?}"
    );
    // A counting phase must expose a total so the client can show a bar.
    assert!(
        client
            .service()
            .frames
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.total.is_some()),
        "expected at least one frame with a determinate total"
    );

    let _ = client.cancel().await;
}
