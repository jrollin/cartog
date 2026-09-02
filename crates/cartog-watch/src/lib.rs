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

/// Verdict on whether a `.cartog.toml` at the given path is usable.
///
/// Supplied by the binary, which owns the schema; see
/// [`WatchConfig::config_usable`].
pub type ConfigUsable = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

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
    /// `cartog_registry::slot_for_db("watch", db_path)`. Library
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
    /// Consent gate: may the watcher create a fresh `.cartog/` on its first
    /// index? `true` (the default) preserves the historical behavior. When
    /// `false` (a `cartog serve --watch` started degraded on a config-less,
    /// un-indexed repo), the watcher stays degraded and keeps watching the root
    /// for a `.cartog.toml` to appear — then it pre-builds the index so the
    /// next MCP relaunch finds it ready. Re-evaluated against the live config +
    /// DB each pass, so an existing DB or a newly-set `CARTOG_AUTO_INIT` also
    /// flips it.
    pub allow_create: bool,
    /// True when the `.cartog.toml` seen **at startup** existed but could not be
    /// parsed, so its `[database] path` is unknown.
    ///
    /// Without this, a broken config would grant consent and pre-build an index
    /// at the default location the user may have configured away from. The
    /// binary already parsed the file, so it passes the verdict down rather than
    /// making this crate re-derive it — `cartog_toml_at_or_above` therefore
    /// reports *presence* only.
    ///
    /// This is a startup snapshot. A config that appears or changes mid-session
    /// is judged by [`WatchConfig::config_usable`] instead.
    pub config_unparseable: bool,
    /// Live usability check for a `.cartog.toml` that appears or changes after
    /// startup, given its path. `None` (the default) trusts presence alone.
    ///
    /// The binary injects its real loader here. `config_unparseable` only covers
    /// the file present at startup, so a serve that began with *no* config has it
    /// `false` — and a broken config written mid-session would otherwise be
    /// consented to on presence alone. This crate must not re-derive the verdict
    /// itself: a local syntax check is blind to the schema, provider and
    /// credential validation `main` applies, which is exactly how the two answers
    /// drifted apart.
    pub config_usable: Option<ConfigUsable>,
}

impl WatchConfig {
    /// Build a [`WatchConfig`] rooted at `root` with these defaults:
    /// `debounce = 5s`, `rag_override = None` (auto-detect), `rag_delay = 30s`,
    /// `json_events = false`, both `pid_lock_*` = `None` (untracked
    /// mode), `skip_migrations = false`, `allow_create = true`. Callers wanting
    /// PID-lock tracking must set BOTH `pid_lock_dir` and `pid_lock_slot` after
    /// construction — see [`WatchConfig::pid_lock_slot`].
    #[must_use]
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
            // Default permissive: standalone `cartog watch` is consent-gated by
            // the CLI before it reaches here; the degraded `serve --watch` path
            // sets this to false explicitly.
            allow_create: true,
            config_unparseable: false,
            config_usable: None,
        }
    }
}

/// Legacy fallback slot used in untracked mode (`pid_lock_dir = None`,
/// `pid_lock_slot = None`). When `pid_lock_dir` is set, callers must
/// provide a DB-scoped slot via
/// `cartog_registry::slot_for_db("watch", db_path)` — see
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
             (e.g. `cartog_registry::slot_for_db(\"watch\", db_path)`)"
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

    // Consent gate: when the watcher may NOT create a fresh `.cartog/` and none
    // exists yet (a `cartog serve --watch` started degraded on a config-less,
    // un-indexed repo), stay degraded — watch the root for a `.cartog.toml` to
    // appear (or an existing DB / `CARTOG_AUTO_INIT`) before opening the DB.
    // Returns `true` once consent is granted, `false` if shutdown fired first.
    if !watcher_consents(&config, root, db_path) {
        info!(
            path = %root.display(),
            "watcher degraded: no .cartog.toml and no index yet — watching for `cartog init` \
             (run it to opt in; the index pre-builds and loads on the next Claude Code launch)"
        );
        if !wait_for_consent(&config, db_path, root, shutdown)? {
            info!("watch stopped before consent was granted");
            return Ok(());
        }
        info!("consent granted (config or index appeared); building the initial index");
    }

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
    // Debounce for machine-local registry writes; see REGISTRY_WRITE_DEBOUNCE.
    let mut last_registry_write: Option<Instant> = None;

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
                            // Refresh the machine-local registry, but not on
                            // every keystroke-driven pass: a watcher can
                            // re-index many times a minute, and each
                            // registration pays a `stats()`. Debounced, and
                            // only when the pass actually changed the graph.
                            if (r.files_indexed > 0 || r.files_removed > 0)
                                && registry_debounce_elapsed(last_registry_write)
                            {
                                record_watched_project(&db, db_path, root);
                                last_registry_write = Some(Instant::now());
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

/// Environment variable that opts a config-less project into indexing with
/// defaults. Must match the binary's `CARTOG_AUTO_INIT` (the watcher crate
/// can't import the binary's config module). Re-checked live so a var set
/// after the watcher started still flips consent.
const AUTO_INIT_ENV: &str = "CARTOG_AUTO_INIT";

/// True when the watcher may build/refresh the index. Consent is granted by
/// the threaded `allow_create` flag (config present / DB existed / AUTO_INIT at
/// startup) OR — re-evaluated live each pass — a *usable* `.cartog.toml` now
/// exists at or above the watched `root` (the `cartog init` mid-session signal)
/// OR the main DB file now exists OR `CARTOG_AUTO_INIT` is now set. Keyed on the
/// main DB file, so a stray `-wal`/`-shm` without it does not count.
fn watcher_consents(config: &WatchConfig, root: &Path, db_path: &str) -> bool {
    // An existing DB or AUTO_INIT still consents: the location is settled, or
    // the user asked for defaults explicitly. Only the "a `.cartog.toml`
    // appeared" signal is suppressed, since that file is the unreadable one.
    config.allow_create
        || (!config.config_unparseable && config_appeared_and_is_usable(config, root))
        || Path::new(db_path).exists()
        || std::env::var(AUTO_INIT_ENV)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
}

/// The mid-session "a `.cartog.toml` appeared" signal, gated on the binary's
/// usability verdict when one was injected.
///
/// `config_unparseable` is a startup snapshot, so a serve that began with no
/// config carries `false` and cannot speak for a file written afterwards. Absent
/// an injected predicate, presence alone consents — the historical behavior.
fn config_appeared_and_is_usable(config: &WatchConfig, root: &Path) -> bool {
    let Some(path) = cartog_toml_path_at_or_above(root) else {
        return false;
    };
    match &config.config_usable {
        Some(is_usable) => is_usable(&path),
        None => true,
    }
}

/// The `.cartog.toml` at `root` or the nearest ancestor up to (and including)
/// the git root, if any. Mirrors the binary's `local_config_path` walk-up so
/// a `cartog serve --watch` launched from a subdirectory still sees a `.cartog.toml`
/// written at the git root by `cartog init` — without this, the watcher (rooted
/// at the subdir) would miss it and only the next relaunch would un-degrade.
///
/// **Presence only.** Whether the file is *usable* is the binary's verdict,
/// threaded in as [`WatchConfig::config_unparseable`]; re-deriving it here meant
/// two answers to one question, and the weaker of the two (a raw syntax check,
/// blind to the schema and credential validation `main` applies) decided the
/// mid-session case. See `watcher_consents`.
fn cartog_toml_path_at_or_above(root: &Path) -> Option<PathBuf> {
    let mut dir = root;
    loop {
        let candidate = dir.join(".cartog.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        // Stop at the git root: don't escape the project into ancestors / $HOME.
        if dir.join(".git").exists() {
            return None;
        }
        dir = dir.parent()?;
    }
}

/// Install a best-effort non-recursive watcher on `root` for the degraded
/// consent wait. Returns `None` (caller falls back to polling) if the
/// debouncer or the `.watch()` call fails. The returned debouncer must be held
/// alive by the caller; dropping it stops watching.
fn install_root_watcher(
    tx: std::sync::mpsc::Sender<notify_debouncer_mini::DebounceEventResult>,
    root: &Path,
) -> Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    let mut debouncer = match new_debouncer(Duration::from_millis(500), tx) {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "consent-wait watcher failed to start; polling only");
            return None;
        }
    };
    if let Err(e) = debouncer
        .watcher()
        .watch(root, notify::RecursiveMode::NonRecursive)
    {
        warn!(error = %e, "consent-wait watcher failed to install; polling only");
        return None;
    }
    Some(debouncer)
}

/// Degraded-watch loop: block until [`watcher_consents`] turns true or
/// `shutdown` is set. Watches the repo root so a `.cartog.toml` create event
/// wakes us promptly; a 1s poll covers shutdown, a DB appearing out-of-band,
/// `CARTOG_AUTO_INIT` being exported mid-session, and a `.cartog.toml` written
/// at a git-root *above* the watched root (which the non-recursive root watcher
/// won't event on, but the walk-up in [`watcher_consents`] catches on the next
/// poll). Opens no DB and creates no `.cartog/`. Returns `Ok(true)` when consent
/// is granted, `Ok(false)` when shutdown fired first.
fn wait_for_consent(
    config: &WatchConfig,
    db_path: &str,
    root: &Path,
    shutdown: &AtomicBool,
) -> Result<bool> {
    // A best-effort watcher on the root: any filesystem event (notably a
    // `.cartog.toml` create) wakes the recv early so we re-check consent
    // without waiting out the full poll interval. Held for its Drop (which
    // stops watching). If it fails to install we fall back to pure polling —
    // correctness doesn't depend on the events, only latency.
    let (tx, rx) = std::sync::mpsc::channel();
    let _debouncer = install_root_watcher(tx, root);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if watcher_consents(config, root, db_path) {
            return Ok(true);
        }
        // Block up to 1s for an event; the timeout doubles as the poll interval
        // (covers shutdown, an out-of-band DB, and AUTO_INIT set mid-session).
        // A disconnected channel just means no live watcher — keep polling.
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
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

/// Minimum gap between registry writes from a watcher.
///
/// A watcher can re-index many times a minute while a person edits, and each
/// registration pays a `stats()` (five scans). The registry is a coarse
/// "what is on this machine" view, so a minute of staleness costs nothing.
const REGISTRY_WRITE_DEBOUNCE: Duration = Duration::from_secs(60);

/// Whether enough time has passed since the last registry write.
///
/// `None` (no write yet this session) always passes: the first pass after a
/// watcher starts is exactly when the registry is most likely to be stale.
fn registry_debounce_elapsed(last: Option<Instant>) -> bool {
    // `map_or`, not `is_none_or`: the latter is stable only from Rust 1.82 and
    // this workspace's MSRV is 1.80.
    last.map_or(true, |t| t.elapsed() >= REGISTRY_WRITE_DEBOUNCE)
}

/// Record the watched project in the machine-local registry.
///
/// Mirrors `cartog index`'s registration: full counts plus a `last_indexed`,
/// because a watcher pass *is* an indexing pass. Never fails the watcher — the
/// registry's write path logs and returns.
fn record_watched_project(db: &Database, db_path: &str, root: &Path) {
    let path = Path::new(db_path);
    let mut facts = cartog_registry::ProjectFacts::identity_only(path, root);
    if let Ok(stats) = db.stats() {
        facts.file_count = Some(stats.num_files);
        facts.symbol_count = Some(stats.num_symbols);
        facts.edge_count = Some(stats.num_edges);
        facts.resolved_count = Some(stats.num_resolved);
        facts.languages = Some(stats.languages);
    }
    facts.embedding_count = db.embedding_count().ok();
    facts.schema_version = cartog_db::read_schema_version_at(path)
        .ok()
        .filter(|v| *v > 0);
    facts.embed_provider = cartog_db::read_metadata_at(path, cartog_db::EMBED_PROVIDER_KEY)
        .ok()
        .flatten();
    facts.embed_model = cartog_db::read_metadata_at(path, cartog_db::EMBED_MODEL_KEY)
        .ok()
        .flatten();
    facts.embed_dim = cartog_db::read_metadata_at(path, cartog_db::EMBED_DIMENSION_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    facts.last_indexed = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64),
    );
    cartog_registry::record_project(&facts);
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

    // ── consent gate (allow_create) ──

    /// Restore CARTOG_AUTO_INIT on drop so a set/remove in one test can't leak.
    fn auto_init_guard() -> impl Drop {
        struct Restore(Option<String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match &self.0 {
                    Some(v) => std::env::set_var(AUTO_INIT_ENV, v),
                    None => std::env::remove_var(AUTO_INIT_ENV),
                }
            }
        }
        Restore(std::env::var(AUTO_INIT_ENV).ok())
    }

    #[test]
    fn watcher_consents_when_allow_create() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = true;
        assert!(watcher_consents(&config, tmp.path(), "/no/such/db.sqlite"));
    }

    /// Reject every `.cartog.toml` the binary would reject.
    fn rejecting_verdict() -> Option<ConfigUsable> {
        Some(Arc::new(|_: &Path| false))
    }

    #[test]
    #[serial_test::serial]
    fn watcher_withholds_consent_for_an_unparseable_cartog_toml() {
        // A `.cartog.toml` that appears mid-session but cannot be parsed may name
        // a `[database] path` we would then ignore — pre-building an index at the
        // default location the user configured away from. Existence alone is not
        // the signal; the binary's verdict is.
        let _g = auto_init_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".cartog.toml"), "[database\npath = \"x\"\n").unwrap();
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = false;
        config.config_usable = rejecting_verdict();
        assert!(
            !watcher_consents(&config, tmp.path(), "/no/such/db.sqlite"),
            "a broken config must not grant the mid-session init signal"
        );
    }

    /// The gap that closed: `config_unparseable` is a *startup* snapshot, so a
    /// serve that began with no config carries `false` and cannot speak for a
    /// file written afterwards. The crate used to fall back to its own raw-syntax
    /// check, which is blind to the schema, provider and credential validation
    /// the binary applies — so a syntactically-valid but schema-rejected config
    /// appearing mid-session granted consent `main` would have refused.
    #[test]
    #[serial_test::serial]
    fn schema_rejected_config_appearing_mid_session_is_refused() {
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        // Valid TOML — a syntax-only check says yes; the binary says no.
        std::fs::write(
            tmp.path().join(".cartog.toml"),
            "[remote]\nendpoint = \"https://x.example.com\"\nbucket = \"b\"\nsecret_key = \"AKIA\"\n",
        )
        .unwrap();
        assert!(
            toml::from_str::<toml::value::Table>(
                &std::fs::read_to_string(tmp.path().join(".cartog.toml")).unwrap()
            )
            .is_ok(),
            "fixture must be syntactically valid, or it wouldn't cover the gap"
        );
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        // A serve that started with no config: the startup flag says nothing.
        config.allow_create = false;
        config.config_unparseable = false;
        config.config_usable = rejecting_verdict();
        assert!(
            !watcher_consents(&config, tmp.path(), "/no/such/db.sqlite"),
            "the binary's verdict must decide, not a local syntax check"
        );
    }

    #[test]
    #[serial_test::serial]
    fn usable_config_appearing_mid_session_still_consents() {
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".cartog.toml"), b"[database]\n").unwrap();
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = false;
        config.config_usable = Some(Arc::new(|_: &Path| true));
        assert!(
            watcher_consents(&config, tmp.path(), "/no/such/db.sqlite"),
            "a good config written mid-session is still the `cartog init` signal"
        );
    }

    #[test]
    #[serial_test::serial]
    fn watcher_consents_when_an_existing_db_outlives_a_broken_config() {
        // Contrast: the db location is already settled, so a broken config is
        // irrelevant — steady-state re-indexing must keep working.
        let _g = auto_init_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".cartog.toml"), "[database\n").unwrap();
        let db = tmp.path().join("db.sqlite");
        std::fs::write(&db, b"").unwrap();
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = false;
        assert!(watcher_consents(&config, tmp.path(), db.to_str().unwrap()));
    }

    #[test]
    #[serial_test::serial]
    fn watcher_consents_when_cartog_toml_appears() {
        // The `cartog init` mid-session signal: a `.cartog.toml` at the watched
        // root grants consent even with allow_create=false and no DB.
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join(".cartog").join("db.sqlite");
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = false;
        assert!(
            !watcher_consents(&config, tmp.path(), db.to_str().unwrap()),
            "no config yet → no consent"
        );
        std::fs::write(tmp.path().join(".cartog.toml"), b"[database]\n").unwrap();
        assert!(
            watcher_consents(&config, tmp.path(), db.to_str().unwrap()),
            "a .cartog.toml appearing at the root grants consent"
        );
    }

    #[test]
    #[serial_test::serial]
    fn watcher_consents_when_cartog_toml_at_git_root_above_watch_root() {
        // serve --watch launched from a subdir: the watched root is the subdir,
        // but `cartog init` may write .cartog.toml at the git root above it. The
        // walk-up must find it (stopping at the git root, not escaping further).
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        let git_root = tmp.path();
        std::fs::create_dir(git_root.join(".git")).unwrap();
        let subdir = git_root.join("crates").join("inner");
        std::fs::create_dir_all(&subdir).unwrap();
        let db = git_root.join(".cartog").join("db.sqlite");

        let mut config = WatchConfig::new(subdir.clone());
        config.allow_create = false;
        assert!(
            !watcher_consents(&config, &subdir, db.to_str().unwrap()),
            "no config anywhere up to git root → no consent"
        );
        // init writes at the git root, above the subdir watch root.
        std::fs::write(git_root.join(".cartog.toml"), b"[database]\n").unwrap();
        assert!(
            watcher_consents(&config, &subdir, db.to_str().unwrap()),
            "a git-root .cartog.toml above the watch root grants consent (walk-up)"
        );
    }

    #[test]
    fn cartog_toml_walk_up_stops_at_git_root() {
        // A .cartog.toml ABOVE the git root must NOT count — the walk-up stops
        // at the git boundary so it can't escape into an ancestor or $HOME.
        let tmp = tempfile::TempDir::new().unwrap();
        let outer = tmp.path();
        std::fs::write(outer.join(".cartog.toml"), b"[database]\n").unwrap();
        let git_root = outer.join("project");
        std::fs::create_dir_all(git_root.join(".git")).unwrap();
        let subdir = git_root.join("src");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(
            cartog_toml_path_at_or_above(&subdir).is_none(),
            "walk-up must stop at the git root, not reach the outer .cartog.toml"
        );
    }

    #[test]
    #[serial_test::serial]
    fn watcher_consents_when_db_exists() {
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("db.sqlite");
        std::fs::write(&db, b"").unwrap();
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = false;
        assert!(watcher_consents(&config, tmp.path(), db.to_str().unwrap()));
    }

    #[test]
    #[serial_test::serial]
    fn watcher_refuses_without_any_signal() {
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join(".cartog").join("db.sqlite");
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = false;
        assert!(
            !watcher_consents(&config, tmp.path(), db.to_str().unwrap()),
            "no allow_create, no config, no DB, no AUTO_INIT → no consent"
        );
    }

    #[test]
    #[serial_test::serial]
    fn watcher_consents_with_auto_init_env() {
        let _g = auto_init_guard();
        std::env::set_var(AUTO_INIT_ENV, "1");
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join(".cartog").join("db.sqlite");
        let mut config = WatchConfig::new(tmp.path().to_path_buf());
        config.allow_create = false;
        assert!(watcher_consents(&config, tmp.path(), db.to_str().unwrap()));
    }

    #[test]
    #[serial_test::serial]
    fn degraded_watch_loop_creates_nothing_then_stops() {
        // allow_create=false + no DB: the loop stays in the degraded wait,
        // creating no `.cartog/`. Setting shutdown returns it cleanly.
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let db_path = root.join(".cartog").join("db.sqlite");

        let mut config = WatchConfig::new(root.clone());
        config.allow_create = false;

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        let db_str = db_path.to_string_lossy().into_owned();
        let root_for_thread = root.clone();
        let handle = std::thread::spawn(move || {
            watch_loop(config, &root_for_thread, &db_str, &shutdown_clone)
        });

        // Give the degraded wait a moment, then confirm nothing was created.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !db_path.parent().unwrap().exists(),
            "degraded watcher must not create .cartog/ while waiting for consent"
        );

        shutdown.store(true, Ordering::SeqCst);
        let result = handle.join().expect("watch thread joins");
        assert!(result.is_ok(), "degraded watch_loop exits Ok on shutdown");
        assert!(
            !db_path.parent().unwrap().exists(),
            "no .cartog/ after a shutdown-before-consent run"
        );
    }

    #[test]
    #[serial_test::serial]
    fn watcher_builds_index_once_db_appears() {
        // Consent granted out-of-band (DB created): the watcher leaves the
        // degraded wait and indexes the root, populating the DB.
        let _g = auto_init_guard();
        std::env::remove_var(AUTO_INIT_ENV);
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("a.py"), "def f():\n    return 1\n").unwrap();
        let db_path = root.join(".cartog").join("db.sqlite");

        let mut config = WatchConfig::new(root.clone());
        config.allow_create = false;

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        let db_str = db_path.to_string_lossy().into_owned();
        let root_for_thread = root.clone();
        let handle = std::thread::spawn(move || {
            watch_loop(config, &root_for_thread, &db_str, &shutdown_clone)
        });

        // Grant consent: create the DB the way `cartog index` would, so the
        // degraded loop's `db_path.exists()` check flips true.
        std::thread::sleep(Duration::from_millis(200));
        Database::open(&db_path, cartog_db::DEFAULT_EMBEDDING_DIM).expect("create DB out of band");

        // Wait for the watcher to perform its initial index against the DB.
        let mut indexed = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(db) = Database::open_existing_rw(&db_path) {
                if !db.is_empty().unwrap_or(true) {
                    indexed = true;
                    break;
                }
            }
        }
        shutdown.store(true, Ordering::SeqCst);
        let _ = handle.join();
        assert!(
            indexed,
            "watcher must build the index once consent (an existing DB) appears"
        );
    }
}
