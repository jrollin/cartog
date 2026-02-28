use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use tracing::{debug, info, warn};

use crate::db::Database;
use crate::indexer::{self, is_ignored_dirname};
use crate::languages::detect_language;
use crate::rag;

/// Configuration for the watch loop.
pub struct WatchConfig {
    /// Root directory to watch.
    pub root: PathBuf,
    /// Debounce window for filesystem events.
    pub debounce: Duration,
    /// Whether to auto-embed after indexing.
    pub rag: bool,
    /// Delay after last index before embedding (only when `rag` is true).
    pub rag_delay: Duration,
}

impl WatchConfig {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            debounce: Duration::from_secs(2),
            rag: false,
            rag_delay: Duration::from_secs(30),
        }
    }
}

/// Handle returned by `spawn_watch`. Drop or call `stop()` to shut down the watcher.
pub struct WatchHandle {
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WatchHandle {
    /// Signal the watch loop to stop and wait for it to finish.
    pub fn stop(mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Don't join on drop — the thread will exit on next loop iteration.
    }
}

/// Spawn the watch loop on a background thread.
///
/// Returns a `WatchHandle` that can be used to stop the watcher.
/// The watcher opens its own `Database` connection (SQLite WAL allows concurrent readers).
pub fn spawn_watch(config: WatchConfig, db_path: &str) -> Result<WatchHandle> {
    let root = config
        .root
        .canonicalize()
        .context("cannot resolve watch root")?;

    if !root.is_dir() {
        anyhow::bail!("watch target is not a directory: {}", root.display());
    }

    let db_path = db_path.to_string();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);

    let thread = std::thread::Builder::new()
        .name("cartog-watch".into())
        .spawn(move || {
            if let Err(e) = watch_loop(config, &root, &db_path, &shutdown_clone) {
                warn!(error = %e, "watch loop exited with error");
            }
        })
        .context("failed to spawn watch thread")?;

    Ok(WatchHandle {
        shutdown,
        thread: Some(thread),
    })
}

/// Run the watch loop in the foreground (blocking).
///
/// Used by `cartog watch` CLI command.
pub fn run_watch(config: WatchConfig, db_path: &str) -> Result<()> {
    let root = config
        .root
        .canonicalize()
        .context("cannot resolve watch root")?;

    if !root.is_dir() {
        anyhow::bail!("watch target is not a directory: {}", root.display());
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);

    // Install Ctrl+C handler for graceful shutdown
    install_ctrlc_handler(&shutdown_clone);

    watch_loop(config, &root, db_path, &shutdown)
}

/// Install a Ctrl+C handler that sets the shutdown flag.
fn install_ctrlc_handler(flag: &Arc<AtomicBool>) {
    let flag = Arc::clone(flag);
    let _ = ctrlc::set_handler(move || {
        flag.store(true, Ordering::SeqCst);
    });
}

/// Core watch loop. Runs until `shutdown` is set.
fn watch_loop(
    config: WatchConfig,
    root: &Path,
    db_path: &str,
    shutdown: &AtomicBool,
) -> Result<()> {
    let db = Database::open(db_path).context("failed to open database for watcher")?;

    info!(
        path = %root.display(),
        debounce_ms = config.debounce.as_millis(),
        rag = config.rag,
        rag_delay_s = config.rag_delay.as_secs(),
        "starting watch"
    );

    // Initial incremental index to ensure DB is current
    match indexer::index_directory(&db, root, false) {
        Ok(r) => info!(
            files = r.files_indexed,
            skipped = r.files_skipped,
            removed = r.files_removed,
            symbols = r.symbols_added,
            "initial index complete"
        ),
        Err(e) => warn!(error = %e, "initial index failed"),
    }

    // Set up the debounced file watcher
    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer =
        new_debouncer(config.debounce, tx).context("failed to create file watcher")?;

    debouncer
        .watcher()
        .watch(root, notify::RecursiveMode::Recursive)
        .context("failed to start watching directory")?;

    info!("watching for changes (Ctrl+C to stop)");

    // RAG timer state: when we last indexed (to defer embedding)
    let mut rag_pending = false;
    let mut last_index_time: Option<Instant> = None;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // Wait for events with a timeout so we can check shutdown + RAG timer
        let poll_timeout = if config.rag && rag_pending {
            Duration::from_millis(500) // Poll frequently to check RAG timer
        } else {
            Duration::from_secs(1) // Idle poll for shutdown check
        };

        match rx.recv_timeout(poll_timeout) {
            Ok(Ok(events)) => {
                // Filter events to only supported source files in non-ignored dirs
                let relevant = events.iter().any(|event| {
                    event.kind == DebouncedEventKind::Any && is_relevant_path(&event.path, root)
                });

                if relevant {
                    debug!(
                        count = events.len(),
                        "file change events received, re-indexing"
                    );
                    match indexer::index_directory(&db, root, false) {
                        Ok(r) => {
                            if r.files_indexed > 0 || r.files_removed > 0 {
                                info!(
                                    files = r.files_indexed,
                                    skipped = r.files_skipped,
                                    removed = r.files_removed,
                                    symbols = r.symbols_added,
                                    "re-indexed"
                                );
                            }
                            // Check if RAG embedding is needed
                            if config.rag {
                                match db.symbols_needing_embeddings() {
                                    Ok(needing) if !needing.is_empty() => {
                                        debug!(
                                            pending = needing.len(),
                                            "symbols need embedding, starting RAG timer"
                                        );
                                        rag_pending = true;
                                        last_index_time = Some(Instant::now());
                                    }
                                    Ok(_) => {
                                        // No symbols need embedding
                                        rag_pending = false;
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "failed to check embedding status");
                                    }
                                }
                            }
                        }
                        Err(e) => warn!(error = %e, "re-index failed"),
                    }
                }
            }
            Ok(Err(error)) => {
                warn!(error = %error, "file watcher error");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Check RAG timer
                if config.rag && rag_pending {
                    if let Some(last) = last_index_time {
                        if last.elapsed() >= config.rag_delay {
                            info!("RAG delay elapsed, embedding pending symbols");
                            match rag::indexer::index_embeddings(&db, false) {
                                Ok(r) => {
                                    info!(
                                        embedded = r.symbols_embedded,
                                        skipped = r.symbols_skipped,
                                        "RAG embedding complete"
                                    );
                                }
                                Err(e) => {
                                    warn!(error = %e, "RAG embedding failed");
                                }
                            }
                            rag_pending = false;
                            last_index_time = None;
                        }
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                warn!("file watcher channel disconnected");
                break;
            }
        }
    }

    // Flush pending RAG embeddings on shutdown
    if config.rag && rag_pending {
        info!("flushing pending RAG embeddings before shutdown");
        match rag::indexer::index_embeddings(&db, false) {
            Ok(r) => info!(embedded = r.symbols_embedded, "final RAG flush complete"),
            Err(e) => warn!(error = %e, "final RAG flush failed"),
        }
    }

    info!("watch stopped");
    Ok(())
}

/// Check if a path is relevant for indexing: supported language + not in ignored directory.
///
/// Returns `false` for:
/// - Files with unsupported extensions (no tree-sitter extractor)
/// - Files outside the watched root (e.g., symlink escapes)
/// - Files under an ignored directory (`.git`, `node_modules`, etc.)
fn is_relevant_path(path: &Path, root: &Path) -> bool {
    // Must be a supported source file
    if detect_language(path).is_none() {
        return false;
    }

    // Must be under the watched root
    let relative = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return false,
    };

    // Check that no ancestor directory is ignored
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            if let std::path::Component::Normal(name) = component {
                if let Some(name_str) = name.to_str() {
                    if is_ignored_dirname(name_str) {
                        return false;
                    }
                }
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Language coverage: all supported extensions ──

    #[test]
    fn test_relevant_python_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/main.py"), &root));
    }

    #[test]
    fn test_relevant_python_stub() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/types.pyi"), &root));
    }

    #[test]
    fn test_relevant_typescript_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/app.ts"), &root));
    }

    #[test]
    fn test_relevant_tsx_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/App.tsx"), &root));
    }

    #[test]
    fn test_relevant_javascript_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/index.js"), &root));
    }

    #[test]
    fn test_relevant_jsx_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/App.jsx"), &root));
    }

    #[test]
    fn test_relevant_mjs_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/utils.mjs"), &root));
    }

    #[test]
    fn test_relevant_cjs_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/config.cjs"),
            &root
        ));
    }

    #[test]
    fn test_relevant_rust_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/src/lib.rs"), &root));
    }

    #[test]
    fn test_relevant_go_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/cmd/main.go"), &root));
    }

    #[test]
    fn test_relevant_ruby_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/lib/service.rb"),
            &root
        ));
    }

    // ── Irrelevant file types ──

    #[test]
    fn test_irrelevant_json_file() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(Path::new("/project/package.json"), &root));
    }

    #[test]
    fn test_irrelevant_markdown_file() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(Path::new("/project/README.md"), &root));
    }

    #[test]
    fn test_irrelevant_toml_file() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(Path::new("/project/Cargo.toml"), &root));
    }

    #[test]
    fn test_irrelevant_yaml_file() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.github/ci.yml"),
            &root
        ));
    }

    #[test]
    fn test_irrelevant_no_extension() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(Path::new("/project/Makefile"), &root));
    }

    // ── Ignored directories (all entries from is_ignored_dirname) ──

    #[test]
    fn test_ignored_node_modules() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/node_modules/pkg/index.js"),
            &root
        ));
    }

    #[test]
    fn test_ignored_git_dir() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.git/hooks/pre-commit.py"),
            &root
        ));
    }

    #[test]
    fn test_ignored_target_dir() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/target/debug/build.rs"),
            &root
        ));
    }

    #[test]
    fn test_ignored_pycache() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/src/__pycache__/mod.py"),
            &root
        ));
    }

    #[test]
    fn test_ignored_nested_vendor() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/lib/vendor/gem/lib.rb"),
            &root
        ));
    }

    #[test]
    fn test_ignored_venv() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.venv/lib/site.py"),
            &root
        ));
        assert!(!is_relevant_path(
            Path::new("/project/venv/lib/site.py"),
            &root
        ));
    }

    #[test]
    fn test_ignored_env() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.env/lib/site.py"),
            &root
        ));
        assert!(!is_relevant_path(
            Path::new("/project/env/lib/site.py"),
            &root
        ));
    }

    #[test]
    fn test_ignored_dist_build() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/dist/bundle.js"),
            &root
        ));
        assert!(!is_relevant_path(
            Path::new("/project/build/output.js"),
            &root
        ));
    }

    #[test]
    fn test_ignored_next_nuxt() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.next/server/app.js"),
            &root
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.nuxt/dist/app.js"),
            &root
        ));
    }

    #[test]
    fn test_ignored_mypy_pytest_tox() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.mypy_cache/3.11/mod.py"),
            &root
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.pytest_cache/v/test.py"),
            &root
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.tox/py311/lib.py"),
            &root
        ));
    }

    #[test]
    fn test_ignored_hg_svn() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.hg/store/data.py"),
            &root
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.svn/entries.py"),
            &root
        ));
    }

    // ── Path boundary conditions ──

    #[test]
    fn test_hidden_dir_ignored() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.hidden/script.py"),
            &root
        ));
    }

    #[test]
    fn test_root_level_file_allowed() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(Path::new("/project/setup.py"), &root));
    }

    #[test]
    fn test_deeply_nested_file_allowed() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/auth/tokens/validate.py"),
            &root
        ));
    }

    #[test]
    fn test_path_outside_root_rejected() {
        let root = PathBuf::from("/project");
        assert!(
            !is_relevant_path(Path::new("/other/project/main.py"), &root),
            "files outside root should be rejected"
        );
    }

    #[test]
    fn test_path_sibling_of_root_rejected() {
        let root = PathBuf::from("/workspace/project-a");
        assert!(
            !is_relevant_path(Path::new("/workspace/project-b/main.py"), &root),
            "files in sibling directory should be rejected"
        );
    }

    #[test]
    fn test_path_partial_prefix_rejected() {
        let root = PathBuf::from("/project");
        // "/project-b/main.py" starts with "/project" as a string but is not under /project/
        assert!(
            !is_relevant_path(Path::new("/project-b/main.py"), &root),
            "partial prefix match should be rejected (strip_prefix handles this correctly)"
        );
    }

    // ── WatchConfig ──

    #[test]
    fn test_config_defaults() {
        let config = WatchConfig::new(PathBuf::from("."));
        assert_eq!(config.debounce, Duration::from_secs(2));
        assert!(!config.rag);
        assert_eq!(config.rag_delay, Duration::from_secs(30));
    }

    #[test]
    fn test_config_custom_values() {
        let mut config = WatchConfig::new(PathBuf::from("/my/project"));
        config.debounce = Duration::from_secs(5);
        config.rag = true;
        config.rag_delay = Duration::from_secs(60);
        assert_eq!(config.root, PathBuf::from("/my/project"));
        assert_eq!(config.debounce, Duration::from_secs(5));
        assert!(config.rag);
        assert_eq!(config.rag_delay, Duration::from_secs(60));
    }

    // ── spawn_watch error paths ──

    #[test]
    fn test_spawn_watch_nonexistent_dir() {
        let config = WatchConfig::new(PathBuf::from("/nonexistent/path/xyz"));
        let result = spawn_watch(config, ":memory:");
        assert!(result.is_err(), "should fail for nonexistent directory");
    }

    #[test]
    fn test_spawn_watch_file_not_dir() {
        // Use Cargo.toml as a file that exists but is not a directory
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let config = WatchConfig::new(manifest);
        let result = spawn_watch(config, ":memory:");
        assert!(
            result.is_err(),
            "should fail when target is a file, not dir"
        );
    }

    // ── is_ignored_dirname direct tests ──

    #[test]
    fn test_is_ignored_dirname_known_dirs() {
        let ignored = [
            ".git",
            ".hg",
            ".svn",
            "node_modules",
            "__pycache__",
            ".mypy_cache",
            ".pytest_cache",
            ".tox",
            ".venv",
            "venv",
            ".env",
            "env",
            "target",
            "dist",
            "build",
            ".next",
            ".nuxt",
            "vendor",
        ];
        for name in &ignored {
            assert!(is_ignored_dirname(name), "{name} should be ignored");
        }
    }

    #[test]
    fn test_is_ignored_dirname_hidden_dirs() {
        assert!(is_ignored_dirname(".hidden"));
        assert!(is_ignored_dirname(".cache"));
        assert!(is_ignored_dirname(".config"));
    }

    #[test]
    fn test_is_ignored_dirname_allowed_dirs() {
        let allowed = [
            "src", "lib", "tests", "docs", "app", "cmd", "internal", "pkg",
        ];
        for name in &allowed {
            assert!(!is_ignored_dirname(name), "{name} should NOT be ignored");
        }
    }

    #[test]
    fn test_is_ignored_dirname_case_sensitive() {
        // "Target" != "target" — should NOT be ignored (case-sensitive match)
        assert!(!is_ignored_dirname("Target"));
        assert!(!is_ignored_dirname("NODE_MODULES"));
        assert!(!is_ignored_dirname("Vendor"));
    }

    // ── WatchHandle shutdown ──

    #[test]
    fn test_watch_handle_drop_signals_shutdown() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        let handle = WatchHandle {
            shutdown: shutdown_clone,
            thread: None,
        };
        assert!(!shutdown.load(Ordering::SeqCst));
        drop(handle);
        assert!(
            shutdown.load(Ordering::SeqCst),
            "drop should set shutdown flag"
        );
    }

    #[test]
    fn test_watch_handle_stop_signals_and_joins() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        let shutdown_for_thread = Arc::clone(&shutdown);

        let thread = std::thread::spawn(move || {
            // Simulate work loop that checks shutdown
            while !shutdown_for_thread.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let handle = WatchHandle {
            shutdown: shutdown_clone,
            thread: Some(thread),
        };
        handle.stop(); // Should set flag AND join thread
        assert!(shutdown.load(Ordering::SeqCst));
    }
}
