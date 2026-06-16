//! File system watcher with auto-reindexing for cartog.
//!
//! Watches a directory for source file changes using debounced filesystem events,
//! triggers incremental re-indexing, and optionally defers RAG embedding to batch
//! changed symbols after a configurable quiet period.
#![doc = ""]
#![doc = include_str!("../README.md")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use serde::Serialize;
use tracing::{debug, info, warn};

use cartog_core::detect_language;
use cartog_db::Database;
use cartog_indexer as indexer;
use cartog_indexer::is_ignored_dirname;
use cartog_rag as rag;

mod stale;
pub use stale::{StaleSnapshot, StaleState};

/// Configuration for the watch loop.
pub struct WatchConfig {
    /// Root directory to watch.
    pub root: PathBuf,
    /// Debounce window for filesystem events.
    pub debounce: Duration,
    /// Auto-embed override. `Some(true)`/`Some(false)` force on/off; `None`
    /// auto-detects (embed only if the DB already has embeddings). Resolved once
    /// at startup against the live DB — see [`run_watch`].
    pub rag_override: Option<bool>,
    /// Delay after last index before embedding (only when auto-embed is on).
    pub rag_delay: Duration,
    /// RAG provider configuration (embedding + reranker).
    pub rag_config: rag::EmbeddingProviderConfig,
    /// Secret-redaction policy applied to each re-index pass.
    pub redact: indexer::RedactionConfig,
    /// Walk filter (`[index] exclude` globs + gitignore policy), honored by
    /// each re-index so watch and a manual `cartog index` agree on scope. The
    /// relevance filter consults only its `exclude` field (gitignore is the
    /// walker's job, not the event filter's).
    pub walk_filter: indexer::WalkFilter,
    /// Emit newline-delimited JSON events on stdout. When false, the loop
    /// only produces tracing logs on stderr (existing behavior).
    pub json_events: bool,
    /// Directory for the watcher's PID file (written on startup, removed on
    /// graceful exit). `None` disables PID-file tracking. Consulted by
    /// `cartog self update` to detect a running watcher.
    pub pid_lock_dir: Option<PathBuf>,
    /// Slot name used when acquiring the watch PID file. Required when
    /// `pid_lock_dir` is set — [`run_watch`]/[`spawn_watch`] hard-fail
    /// if a directory is configured without a slot, to prevent a
    /// global-slot watcher from silently colliding with DB-scoped peers
    /// in multi-project setups. `None` is only valid when `pid_lock_dir`
    /// is also `None` (untracked mode used by tests).
    ///
    /// In the cartog binary the slot is derived via
    /// `cartog::state::slot_for_db("watch", db_path)`. Library
    /// embedders should follow the same shape: `<prefix>-<16 hex chars>`
    /// where the hex is a SHA-256 prefix of the canonicalized DB path.
    pub pid_lock_slot: Option<String>,
    /// Open the on-disk DB via `Database::open_existing_rw` instead of
    /// `Database::open`. Used by the Phase 5 promoter to attach without
    /// re-running schema migrations (the promoter validated the schema
    /// when it pinned `PinnedAttach`; running them again would re-trigger
    /// the SQLITE_BUSY race the election prevents).
    pub skip_migrations: bool,
    /// Shared staleness state for the MCP server to read. `None` (the default,
    /// e.g. standalone `cartog watch`) disables staleness publishing.
    pub stale: Option<Arc<StaleState>>,
}

impl WatchConfig {
    /// Build a [`WatchConfig`] rooted at `root` with these defaults:
    /// `debounce = 5s`, `rag_override = None` (auto-detect), `rag_delay = 30s`,
    /// `json_events = false`, both `pid_lock_*` = `None` (untracked
    /// mode), `skip_migrations = false`. Callers wanting PID-lock
    /// tracking must set BOTH `pid_lock_dir` and `pid_lock_slot` after
    /// construction — see [`WatchConfig::pid_lock_slot`].
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            // 5s default: long enough to collapse bursts from `git pull` /
            // `npm install` / branch switches into a single re-index, short
            // enough to feel live for normal save-on-type editing.
            debounce: Duration::from_secs(5),
            rag_override: None,
            rag_delay: Duration::from_secs(30),
            rag_config: rag::EmbeddingProviderConfig::default(),
            redact: indexer::RedactionConfig::default(),
            walk_filter: indexer::WalkFilter::unrestricted(),
            json_events: false,
            pid_lock_dir: None,
            pid_lock_slot: None,
            skip_migrations: false,
            stale: None,
        }
    }
}

/// Legacy fallback slot used in untracked mode (`pid_lock_dir = None`,
/// `pid_lock_slot = None`). When `pid_lock_dir` is set, callers must
/// provide a DB-scoped slot via
/// `cartog::state::slot_for_db("watch", db_path)` — see
/// [`WatchConfig::pid_lock_slot`].
pub const WATCH_LOCK_SLOT: &str = "watch";

/// Resolve whether the watcher auto-embeds: an explicit `override` wins, else
/// auto-detect — embed only if the repo already has embeddings (opted into RAG).
fn resolve_watch_rag(override_: Option<bool>, embedding_count: u32) -> bool {
    override_.unwrap_or(embedding_count > 0)
}

/// A single event emitted by the watch loop when `json_events` is enabled.
///
/// Serialized to stdout as one compact JSON object per line (NDJSON) so
/// downstream tooling can parse events as they arrive.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum WatchEvent<'a> {
    /// Emitted once when the watcher begins observing the tree.
    Started {
        root: &'a str,
        debounce_ms: u128,
        rag: bool,
        rag_delay_s: u64,
    },
    /// A debounced re-index pass completed successfully.
    Reindex {
        files_indexed: u32,
        files_skipped: u32,
        files_removed: u32,
        symbols_added: u32,
        edges_added: u32,
        edges_resolved: u32,
        duration_ms: u128,
    },
    /// A debounced re-index pass failed; the loop keeps running.
    ReindexFailed { error: String },
    /// RAG deferred-embedding pass completed.
    RagEmbedded {
        symbols_embedded: u32,
        symbols_skipped: u32,
        total_content_symbols: u32,
        duration_ms: u128,
    },
    /// RAG deferred-embedding pass failed; the loop keeps running.
    RagFailed { error: String },
    /// Emitted once when the watcher has stopped (Ctrl+C, drop, etc.).
    Shutdown,
}

/// Write one NDJSON line to stdout, flushing immediately so consumers see
/// events in real time rather than when the pipe buffer flushes.
///
/// Deliberately fire-and-forget: the only realistic failure modes are a
/// closed stdout pipe (the consumer went away — nothing to report to) or a
/// serde error on an entirely statically-typed struct (impossible in
/// practice). Propagating would force every call site to decide whether to
/// abort the watch loop over a transient stdout hiccup, which is worse
/// behavior than missing one event line.
fn emit_event(event: &WatchEvent<'_>) {
    if let Ok(line) = serde_json::to_string(event) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
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
        // Best-effort join with a bounded deadline. Without this, the
        // watcher thread keeps holding the PID `ProcessLock` until its
        // next `recv_timeout` (~1s for idle, up to `config.debounce`
        // worst case), so a fresh `cartog watch` started right after Drop
        // would observe AcquireError::Held — confusing the user who saw
        // the previous process exit. We try briefly (~1.5s) then return:
        // we'd rather leak the thread than block shutdown for several
        // seconds on a hung debouncer. Callers wanting deterministic
        // cleanup should call `stop()` explicitly (it joins unbounded).
        if let Some(handle) = self.thread.take() {
            let deadline = std::time::Instant::now() + Duration::from_millis(1500);
            while !handle.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
            // else: leak the JoinHandle (process is shutting down anyway).
        }
    }
}

/// Validate that the PID-lock configuration on a [`WatchConfig`] is
/// internally consistent. The two dangerous half-configured states are
/// `(Some(dir), None)` — a global slot would collide with DB-scoped
/// peers — and `(None, Some(slot))` — the slot is silently ignored and
/// the caller's intent is dropped. Called synchronously by both
/// [`spawn_watch`] and [`run_watch`] so a misconfigured embedder never
/// gets an `Ok(WatchHandle)` for a watcher that died on the spot.
fn validate_pid_lock_config(config: &WatchConfig) -> Result<()> {
    match (
        config.pid_lock_dir.is_some(),
        config.pid_lock_slot.is_some(),
    ) {
        (true, false) => anyhow::bail!(
            "WatchConfig::pid_lock_dir is set but pid_lock_slot is None; \
             refusing to claim the global watch slot — pass a DB-scoped slot \
             (e.g. `cartog::state::slot_for_db(\"watch\", db_path)`)"
        ),
        (false, true) => anyhow::bail!(
            "WatchConfig::pid_lock_slot is set but pid_lock_dir is None; \
             a slot without a directory is silently ignored — either set \
             both fields or clear both to run in untracked mode"
        ),
        _ => Ok(()),
    }
}

/// Spawn the watch loop on a background thread.
///
/// Returns a `WatchHandle` that can be used to stop the watcher. The
/// watcher opens its own `Database` connection (SQLite WAL allows
/// concurrent readers).
///
/// Static misconfiguration of [`WatchConfig`] (e.g. `pid_lock_dir` set
/// without `pid_lock_slot`, or vice versa) is checked synchronously
/// before the thread is spawned and surfaced as `Err`, so a
/// misconfigured embedder never gets a `WatchHandle` whose thread is
/// already dead. Errors that emerge later (PID-lock contention with a
/// live peer, filesystem I/O failures) are logged inside the thread;
/// the caller still receives `Ok(WatchHandle)`. Use [`run_watch`] when
/// ALL failures must propagate synchronously.
pub fn spawn_watch(config: WatchConfig, db_path: &str) -> Result<WatchHandle> {
    let root = config
        .root
        .canonicalize()
        .context("cannot resolve watch root")?;

    if !root.is_dir() {
        anyhow::bail!("watch target is not a directory: {}", root.display());
    }
    validate_pid_lock_config(&config)?;

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
    validate_pid_lock_config(&config)?;
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
    // Acquire first so an election loss aborts before opening DB / watcher.
    // Unlike `cartog serve`, the watcher does NOT attach read-only — if
    // another writer already owns the slot, we refuse to start and let the
    // user stop the running process. This applies uniformly: the Phase 5
    // promoter also acquires `watch.pid` so a separately-running
    // `cartog watch` from a terminal correctly sees Held and aborts. Two
    // watchers writing to the same DB would re-index the same files in
    // parallel; the watch slot is the only thing preventing that.
    //
    // The (Some(dir), None) hard-fail is enforced synchronously by
    // `validate_pid_lock_config` in spawn_watch/run_watch BEFORE this
    // function runs; reaching this point with a misconfigured pair
    // means a caller bypassed those entry points.
    validate_pid_lock_config(&config)?;
    let watch_slot: Option<&str> = config.pid_lock_slot.as_deref();
    let _lock: Option<cartog_process_lock::ProcessLock> =
        match (config.pid_lock_dir.as_deref(), watch_slot) {
            (Some(dir), Some(slot)) => match cartog_process_lock::ProcessLock::acquire(dir, slot) {
                Ok(lock) => Some(lock),
                Err(cartog_process_lock::AcquireError::Held(held)) => {
                    anyhow::bail!(
                        "another cartog process holds the watch lock at {} (slot {}, PID {}); \
                         stop it before running `cartog watch`",
                        dir.display(),
                        held.slot,
                        held.pid,
                    );
                }
                Err(cartog_process_lock::AcquireError::Io(e)) => {
                    return Err(e).with_context(|| {
                        format!("failed to acquire watch PID lock at {}", dir.display())
                    });
                }
            },
            _ => None,
        };

    let db = if config.skip_migrations {
        Database::open_existing_rw(db_path)
            .context("failed to open database for watcher (existing-rw)")?
    } else {
        Database::open(db_path, config.rag_config.resolved_dimension())
            .context("failed to open database for watcher")?
    };

    // Re-resolve auto-embed on every consultation, not once at startup: an
    // explicit override wins, else auto-detect against the LIVE embedding count.
    // This way a repo that runs its first `rag index` AFTER the watcher started
    // (the common MCP flow) begins auto-embedding without a restart.
    let rag_override = config.rag_override;
    let rag_enabled = |db: &Database| -> bool {
        resolve_watch_rag(
            rag_override,
            db.embedding_count().unwrap_or_else(|e| {
                warn!(error = %e, "failed to read embedding count; auto-embed off");
                0
            }),
        )
    };

    info!(
        path = %root.display(),
        debounce_ms = config.debounce.as_millis(),
        rag = rag_enabled(&db),
        rag_delay_s = config.rag_delay.as_secs(),
        "starting watch"
    );
    if config.json_events {
        emit_event(&WatchEvent::Started {
            root: &root.to_string_lossy(),
            debounce_ms: config.debounce.as_millis(),
            rag: rag_enabled(&db),
            rag_delay_s: config.rag_delay.as_secs(),
        });
    }

    // Initial incremental index to ensure DB is current. Symbols left needing
    // embedding here arm the RAG timer + staleness state below, so the initial
    // batch is embedded on the same deferred schedule as change-driven reindexes
    // (and shows the staleness banner meanwhile) rather than only on shutdown.
    let mut initial_pending = 0u32;
    let initial_start = Instant::now();
    // Watch never runs the LSP pass (lsp = false), so the override map is inert.
    match indexer::index_directory(
        &db,
        root,
        false,
        false,
        None,
        None,
        config.redact,
        &std::collections::HashMap::new(),
        &config.walk_filter,
    ) {
        Ok(r) => {
            info!(
                files = r.files_indexed,
                skipped = r.files_skipped,
                removed = r.files_removed,
                symbols = r.symbols_added,
                "initial index complete"
            );
            if config.json_events {
                emit_event(&WatchEvent::Reindex {
                    files_indexed: r.files_indexed,
                    files_skipped: r.files_skipped,
                    files_removed: r.files_removed,
                    symbols_added: r.symbols_added,
                    edges_added: r.edges_added,
                    edges_resolved: r.edges_resolved,
                    duration_ms: initial_start.elapsed().as_millis(),
                });
            }
            if rag_enabled(&db) {
                match db.symbols_needing_embeddings() {
                    Ok(needing) => initial_pending = needing.len() as u32,
                    Err(e) => warn!(error = %e, "failed to check embedding status"),
                }
                // A pending format upgrade re-embeds all symbols; count it so the
                // staleness banner reflects the queued re-embed.
                if initial_pending == 0
                    && rag::indexer::embedding_format_upgrade_pending(&db).unwrap_or(false)
                {
                    initial_pending = db.symbol_content_count().unwrap_or(1).max(1);
                }
            }
            // Publish the post-initial-index state (no changes observed yet, so
            // change_seq is 0; caught up to 0).
            if let Some(s) = &config.stale {
                s.note_reindex(s.change_seq(), initial_pending);
            }
        }
        Err(e) => {
            warn!(error = %e, "initial index failed");
            if config.json_events {
                emit_event(&WatchEvent::ReindexFailed {
                    error: e.to_string(),
                });
            }
        }
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

    // Create the embedding provider once (lazy, on first RAG use). On
    // first creation we also reconcile the on-disk fingerprint so a
    // provider/model swap (even at the same dimension) clears the now-stale
    // vector index instead of returning garbage similarity scores.
    let mut rag_provider: Option<Box<dyn rag::provider::EmbeddingProvider>> = None;
    let ensure_provider =
        |provider: &mut Option<Box<dyn rag::provider::EmbeddingProvider>>| -> bool {
            if provider.is_none() {
                match rag::create_embedding_provider(&config.rag_config) {
                    Ok(p) => {
                        if let Err(e) =
                            db.reconcile_embedding_fingerprint(&rag::fingerprint_of(p.as_ref()))
                        {
                            warn!(error = %e, "failed to reconcile embedding fingerprint");
                            return false;
                        }
                        *provider = Some(p);
                        true
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to create embedding provider");
                        false
                    }
                }
            } else {
                true
            }
        };

    // RAG timer seed. `initial_pending` already folds in a pending format upgrade.
    let mut rag_pending = initial_pending > 0;
    let mut last_index_time: Option<Instant> = rag_pending.then(Instant::now);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // Wait for events with a timeout so we can check shutdown + RAG timer
        let poll_timeout = if rag_pending {
            Duration::from_millis(500) // Poll frequently to check RAG timer
        } else {
            Duration::from_secs(1) // Idle poll for shutdown check
        };

        match rx.recv_timeout(poll_timeout) {
            Ok(Ok(events)) => {
                // Filter events to only supported source files in non-ignored dirs
                let relevant = events.iter().any(|event| {
                    event.kind == DebouncedEventKind::Any
                        && is_relevant_path(&event.path, root, &config.walk_filter.exclude)
                });

                if relevant {
                    debug!(
                        count = events.len(),
                        "file change events received, re-indexing"
                    );
                    // Capture the change count BEFORE reindexing; a change that
                    // arrives while the reindex runs bumps the seq past this and
                    // stays flagged stale.
                    let caught_up_to = config.stale.as_ref().map(|s| {
                        s.note_change();
                        s.change_seq()
                    });
                    let reindex_start = Instant::now();
                    match indexer::index_directory(
                        &db,
                        root,
                        false,
                        false,
                        None,
                        None,
                        config.redact,
                        &std::collections::HashMap::new(),
                        &config.walk_filter,
                    ) {
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
                            if config.json_events && (r.files_indexed > 0 || r.files_removed > 0) {
                                emit_event(&WatchEvent::Reindex {
                                    files_indexed: r.files_indexed,
                                    files_skipped: r.files_skipped,
                                    files_removed: r.files_removed,
                                    symbols_added: r.symbols_added,
                                    edges_added: r.edges_added,
                                    edges_resolved: r.edges_resolved,
                                    duration_ms: reindex_start.elapsed().as_millis(),
                                });
                            }
                            // Check if RAG embedding is needed
                            let mut pending_count = 0u32;
                            if rag_enabled(&db) {
                                match db.symbols_needing_embeddings() {
                                    Ok(needing) if !needing.is_empty() => {
                                        debug!(
                                            pending = needing.len(),
                                            "symbols need embedding, starting RAG timer"
                                        );
                                        pending_count = needing.len() as u32;
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
                            if let (Some(s), Some(seq)) = (&config.stale, caught_up_to) {
                                s.note_reindex(seq, pending_count);
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "re-index failed");
                            if config.json_events {
                                emit_event(&WatchEvent::ReindexFailed {
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                warn!(error = %error, "file watcher error");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Check RAG timer (rag_pending is only set when auto-embed is on)
                if rag_pending {
                    if let Some(last) = last_index_time {
                        if last.elapsed() >= config.rag_delay {
                            info!("RAG delay elapsed, embedding pending symbols");
                            if !ensure_provider(&mut rag_provider) {
                                rag_pending = false;
                                last_index_time = None;
                                continue;
                            }
                            if let Some(ref mut provider) = rag_provider {
                                let embed_start = Instant::now();
                                match rag::indexer::index_embeddings(
                                    &db,
                                    provider.as_mut(),
                                    false,
                                    None,
                                    None,
                                ) {
                                    Ok(r) => {
                                        info!(
                                            embedded = r.symbols_embedded,
                                            skipped = r.symbols_skipped,
                                            "RAG embedding complete"
                                        );
                                        if config.json_events {
                                            emit_event(&WatchEvent::RagEmbedded {
                                                symbols_embedded: r.symbols_embedded,
                                                symbols_skipped: r.symbols_skipped,
                                                total_content_symbols: r.total_content_symbols,
                                                duration_ms: embed_start.elapsed().as_millis(),
                                            });
                                        }
                                        // Embeddings are current — clear the stale signal.
                                        if let Some(s) = &config.stale {
                                            s.clear_rag_pending();
                                        }
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "RAG embedding failed");
                                        if config.json_events {
                                            emit_event(&WatchEvent::RagFailed {
                                                error: e.to_string(),
                                            });
                                        }
                                        // Leave the stale signal set: embeddings did NOT
                                        // catch up, so callers must still be warned.
                                    }
                                }
                            }
                            // Disarm the local retry timer regardless (avoid a tight
                            // re-embed loop); a later file change re-arms it.
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

    // Flush pending RAG embeddings on shutdown (rag_pending implies enabled)
    if rag_pending {
        info!("flushing pending RAG embeddings before shutdown");
        ensure_provider(&mut rag_provider);
        if let Some(ref mut provider) = rag_provider {
            let embed_start = Instant::now();
            match rag::indexer::index_embeddings(&db, provider.as_mut(), false, None, None) {
                Ok(r) => {
                    info!(embedded = r.symbols_embedded, "final RAG flush complete");
                    if config.json_events {
                        emit_event(&WatchEvent::RagEmbedded {
                            symbols_embedded: r.symbols_embedded,
                            symbols_skipped: r.symbols_skipped,
                            total_content_symbols: r.total_content_symbols,
                            duration_ms: embed_start.elapsed().as_millis(),
                        });
                    }
                }
                Err(e) => {
                    warn!(error = %e, "final RAG flush failed");
                    if config.json_events {
                        emit_event(&WatchEvent::RagFailed {
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
    }

    info!("watch stopped");
    if config.json_events {
        emit_event(&WatchEvent::Shutdown);
    }
    Ok(())
}

/// Check if a path is relevant for indexing: supported language + not in ignored directory.
///
/// Returns `false` for:
/// - Files with unsupported extensions (no tree-sitter extractor)
/// - Files outside the watched root (e.g., symlink escapes)
/// - Files under an ignored directory (`.git`, `node_modules`, etc.)
/// - Files matching a `[index] exclude` glob (keeps watch scope = index scope)
fn is_relevant_path(path: &Path, root: &Path, exclude: &indexer::ExcludeGlobs) -> bool {
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

    // The file path, then each ancestor as a directory: a bare-name or `dir/*`
    // exclude matches the directory, not the deep file, so checking ancestors
    // keeps the watcher from re-indexing files the index walk already prunes.
    if exclude.is_excluded(relative) {
        return false;
    }
    let mut ancestor = relative.parent();
    while let Some(dir) = ancestor {
        if dir.as_os_str().is_empty() {
            break;
        }
        if exclude.is_excluded_with_dir(dir, true) {
            return false;
        }
        ancestor = dir.parent();
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
        assert!(is_relevant_path(
            Path::new("/project/src/main.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_python_stub() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/types.pyi"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_typescript_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/app.ts"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_tsx_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/App.tsx"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_javascript_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/index.js"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_jsx_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/App.jsx"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_mjs_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/utils.mjs"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_cjs_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/config.cjs"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_rust_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/lib.rs"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_go_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/cmd/main.go"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_ruby_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/lib/service.rb"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_java_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/UserService.java"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    // ── Irrelevant file types ──

    #[test]
    fn test_irrelevant_json_file() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/package.json"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_relevant_markdown_file() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/README.md"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(is_relevant_path(
            Path::new("/project/docs/design.md"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_irrelevant_toml_file() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/Cargo.toml"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_irrelevant_yaml_file() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.github/ci.yml"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_irrelevant_no_extension() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/Makefile"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    // ── Ignored directories (all entries from is_ignored_dirname) ──

    #[test]
    fn test_ignored_node_modules() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/node_modules/pkg/index.js"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn is_relevant_path_respects_exclude_globs() {
        let root = PathBuf::from("/project");
        let exclude =
            indexer::ExcludeGlobs::from_globs(&["mobile/ios/Pods/**".to_string()]).unwrap();
        // Excluded by the glob even though it's a supported (.swift) source file.
        assert!(!is_relevant_path(
            Path::new("/project/mobile/ios/Pods/Firebase/Core.swift"),
            &root,
            &exclude
        ));
        // A sibling source file outside the glob stays relevant.
        assert!(is_relevant_path(
            Path::new("/project/mobile/lib/main.dart"),
            &root,
            &exclude
        ));
    }

    #[test]
    fn is_relevant_path_respects_bare_dir_exclude_via_ancestors() {
        // A bare-name / `dir/*` exclude matches the directory, not the deep
        // file — the watcher must still treat files under it as irrelevant
        // (else it re-indexes paths the walk already prunes).
        let root = PathBuf::from("/project");
        let exclude = indexer::ExcludeGlobs::from_globs(&["vendor".to_string()]).unwrap();
        assert!(!is_relevant_path(
            Path::new("/project/vendor/sub/lib.py"),
            &root,
            &exclude
        ));
        assert!(is_relevant_path(
            Path::new("/project/src/main.py"),
            &root,
            &exclude
        ));
    }

    #[test]
    fn is_relevant_path_empty_exclude_is_noop() {
        let root = PathBuf::from("/project");
        let none = indexer::ExcludeGlobs::empty();
        assert!(is_relevant_path(
            Path::new("/project/src/main.py"),
            &root,
            &none
        ));
        assert!(!is_relevant_path(
            Path::new("/project/node_modules/pkg/index.js"),
            &root,
            &none
        ));
    }

    #[test]
    fn test_ignored_git_dir() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.git/hooks/pre-commit.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_target_dir() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/target/debug/build.rs"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_pycache() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/src/__pycache__/mod.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_nested_vendor() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/lib/vendor/gem/lib.rb"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_venv() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.venv/lib/site.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(!is_relevant_path(
            Path::new("/project/venv/lib/site.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_env() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.env/lib/site.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(!is_relevant_path(
            Path::new("/project/env/lib/site.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_dist_build() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/dist/bundle.js"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(!is_relevant_path(
            Path::new("/project/build/output.js"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_next_nuxt() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.next/server/app.js"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.nuxt/dist/app.js"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_mypy_pytest_tox() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.mypy_cache/3.11/mod.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.pytest_cache/v/test.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.tox/py311/lib.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_ignored_hg_svn() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.hg/store/data.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
        assert!(!is_relevant_path(
            Path::new("/project/.svn/entries.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    // ── Path boundary conditions ──

    #[test]
    fn test_hidden_dir_ignored() {
        let root = PathBuf::from("/project");
        assert!(!is_relevant_path(
            Path::new("/project/.hidden/script.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_root_level_file_allowed() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/setup.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_deeply_nested_file_allowed() {
        let root = PathBuf::from("/project");
        assert!(is_relevant_path(
            Path::new("/project/src/auth/tokens/validate.py"),
            &root,
            &indexer::ExcludeGlobs::empty()
        ));
    }

    #[test]
    fn test_path_outside_root_rejected() {
        let root = PathBuf::from("/project");
        assert!(
            !is_relevant_path(
                Path::new("/other/project/main.py"),
                &root,
                &indexer::ExcludeGlobs::empty()
            ),
            "files outside root should be rejected"
        );
    }

    #[test]
    fn test_path_sibling_of_root_rejected() {
        let root = PathBuf::from("/workspace/project-a");
        assert!(
            !is_relevant_path(
                Path::new("/workspace/project-b/main.py"),
                &root,
                &indexer::ExcludeGlobs::empty()
            ),
            "files in sibling directory should be rejected"
        );
    }

    #[test]
    fn test_path_partial_prefix_rejected() {
        let root = PathBuf::from("/project");
        // "/project-b/main.py" starts with "/project" as a string but is not under /project/
        assert!(
            !is_relevant_path(
                Path::new("/project-b/main.py"),
                &root,
                &indexer::ExcludeGlobs::empty()
            ),
            "partial prefix match should be rejected (strip_prefix handles this correctly)"
        );
    }

    // ── WatchConfig ──

    #[test]
    fn test_config_defaults() {
        let config = WatchConfig::new(PathBuf::from("."));
        assert_eq!(config.debounce, Duration::from_secs(5));
        assert_eq!(config.rag_override, None);
        assert_eq!(config.rag_delay, Duration::from_secs(30));
        assert!(!config.json_events);
    }

    #[test]
    fn auto_detect_embeds_only_when_repo_has_embeddings() {
        assert!(resolve_watch_rag(None, 5));
        assert!(!resolve_watch_rag(None, 0));
    }

    #[test]
    fn explicit_override_beats_embedding_count() {
        assert!(!resolve_watch_rag(Some(false), 100));
        assert!(resolve_watch_rag(Some(true), 0));
    }

    // ── NDJSON event serialization ──
    //
    // Lock in the wire format of the events `cartog watch --json` produces:
    // downstream tools parse these, so a field rename would be a breaking
    // change that should show up in a diff review.

    #[test]
    fn test_watch_event_started_shape() {
        let e = WatchEvent::Started {
            root: "/proj",
            debounce_ms: 5000,
            rag: true,
            rag_delay_s: 30,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"event\":\"started\""));
        assert!(s.contains("\"root\":\"/proj\""));
        assert!(s.contains("\"debounce_ms\":5000"));
        assert!(s.contains("\"rag\":true"));
        assert!(s.contains("\"rag_delay_s\":30"));
    }

    #[test]
    fn test_watch_event_reindex_shape() {
        let e = WatchEvent::Reindex {
            files_indexed: 1,
            files_skipped: 2,
            files_removed: 0,
            symbols_added: 10,
            edges_added: 4,
            edges_resolved: 3,
            duration_ms: 42,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"event\":\"reindex\""));
        assert!(s.contains("\"files_indexed\":1"));
        assert!(s.contains("\"duration_ms\":42"));
    }

    #[test]
    fn test_watch_event_shutdown_shape() {
        let s = serde_json::to_string(&WatchEvent::Shutdown).unwrap();
        assert_eq!(s, "{\"event\":\"shutdown\"}");
    }

    #[test]
    fn test_config_custom_values() {
        let mut config = WatchConfig::new(PathBuf::from("/my/project"));
        config.debounce = Duration::from_secs(5);
        config.rag_override = Some(true);
        config.rag_delay = Duration::from_secs(60);
        assert_eq!(config.root, PathBuf::from("/my/project"));
        assert_eq!(config.debounce, Duration::from_secs(5));
        assert_eq!(config.rag_override, Some(true));
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

    // ── validate_pid_lock_config branches ──

    #[test]
    fn validate_pid_lock_accepts_both_none() {
        let config = WatchConfig::new(PathBuf::from("."));
        assert!(
            validate_pid_lock_config(&config).is_ok(),
            "untracked mode (both None) is valid"
        );
    }

    #[test]
    fn validate_pid_lock_accepts_both_set() {
        let mut config = WatchConfig::new(PathBuf::from("."));
        config.pid_lock_dir = Some(PathBuf::from("/tmp/cartog-locks"));
        config.pid_lock_slot = Some("watch-0123456789abcdef".to_string());
        assert!(
            validate_pid_lock_config(&config).is_ok(),
            "both fields set is valid"
        );
    }

    #[test]
    fn validate_pid_lock_rejects_dir_without_slot() {
        let mut config = WatchConfig::new(PathBuf::from("."));
        config.pid_lock_dir = Some(PathBuf::from("/tmp/cartog-locks"));
        let err = validate_pid_lock_config(&config).expect_err("dir without slot must fail");
        assert!(
            err.to_string().contains("pid_lock_slot is None"),
            "error names the missing slot: {err}"
        );
    }

    #[test]
    fn validate_pid_lock_rejects_slot_without_dir() {
        let mut config = WatchConfig::new(PathBuf::from("."));
        config.pid_lock_slot = Some("watch-0123456789abcdef".to_string());
        let err = validate_pid_lock_config(&config).expect_err("slot without dir must fail");
        assert!(
            err.to_string().contains("pid_lock_dir is None"),
            "error names the missing directory: {err}"
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
