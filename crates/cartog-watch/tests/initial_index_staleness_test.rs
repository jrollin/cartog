//! The initial index a watcher runs on startup must publish RAG-pending state
//! so the staleness banner fires for the initial batch, not only after a later
//! change-driven reindex.

use std::time::{Duration, Instant};

use cartog_db::Database;
use cartog_indexer as indexer;
use cartog_watch::{spawn_watch, StaleState, WatchConfig};

fn wait_for<F: FnMut() -> bool>(mut pred: F, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pred()
}

#[test]
fn initial_index_publishes_rag_pending_to_stale_state() {
    let workspace = tempfile::TempDir::new().unwrap();
    // The walker prunes dot-prefixed dirs, and TempDir names start with ".tmp";
    // index a named subdir so files aren't skipped.
    let root = &workspace.path().join("project");
    std::fs::create_dir(root).unwrap();
    // Bodies must clear the indexer's MIN_CONTENT_BYTES (~50) so symbol_content
    // is stored (and thus shows up as needing an embedding).
    let src = "def authenticate(user, password, mfa_token, remember_me):
    token = generate_token(user, password, mfa_token)
    return token


def generate_token(user, password, mfa_token):
    payload = {'user': user, 'mfa': mfa_token}
    return str(payload)
";
    std::fs::write(root.join("lib.py"), src).unwrap();
    let db_path = root.join("cartog.db");

    // Plain index first: writes symbol_content but NO embeddings, so
    // symbols_needing_embeddings() returns rows the watcher's initial index
    // will publish as RAG-pending.
    {
        let db = Database::open(&db_path, 384).unwrap();
        indexer::index_directory(
            &db,
            root,
            true,
            false,
            None,
            None,
            indexer::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
        )
        .expect("plain index");
        assert!(
            !db.symbols_needing_embeddings().unwrap().is_empty(),
            "content present, embeddings absent → pending rows exist"
        );
    }

    let stale = StaleState::new();
    let mut config = WatchConfig::new(root.to_path_buf());
    config.rag_override = Some(true);
    // Long delay so the deferred embed (which would need an ONNX model) never
    // fires during the test — we only assert the *initial* publish.
    config.rag_delay = Duration::from_secs(3600);
    config.stale = Some(std::sync::Arc::clone(&stale));

    let handle = spawn_watch(config, &db_path.to_string_lossy()).expect("spawn watch");

    // After the initial index, the watcher should have published the pending
    // embedding count — rag_stale() becomes true without any file change.
    let published = wait_for(|| stale.snapshot().rag_stale(), Duration::from_secs(10));
    handle.stop();

    assert!(
        published,
        "initial index must publish RAG-pending state (rag_stale) on startup"
    );
}
