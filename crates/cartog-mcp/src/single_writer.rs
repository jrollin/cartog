//! Single-writer election and the promoter task: `cartog serve` peers on the
//! same DB elect one primary via an O_EXCL PID lock; the secondary promotes if
//! the primary dies. Split out of `lib.rs` to separate this subsystem from the
//! MCP tool surface.

use super::*;

pub const SERVE_LOCK_SLOT: &str = "serve";

/// Convert a serve-family slot (legacy `"serve"` or DB-scoped
/// `"serve-<hash>"`) to the matching watch-family slot
/// (`"watch"` / `"watch-<hash>"`). Used by [`run_server`] so both PID files
/// for the same DB share their scope.
///
/// Anchored on the exact prefixes `"serve"` (literal, legacy) and
/// `"serve-"` (DB-scoped, followed by the hex suffix). Inputs that
/// happen to start with the four letters `serve` but are NOT a
/// serve-family slot (`"server"`, `"serverless"`, `"servefoo"`, …) are
/// rejected: silently folding them to the global watch slot would let
/// distinct embedders collide on `watch.pid` while their serve slots
/// stay distinct (same hazard as the pid_lock_dir/slot half-config).
pub(crate) fn serve_to_watch_slot(serve_slot: &str) -> anyhow::Result<String> {
    if serve_slot == "serve" {
        return Ok(watch::WATCH_LOCK_SLOT.to_string());
    }
    // strip_prefix + non-empty filter: `"serve-"` alone (trailing dash,
    // empty hex) is treated as off-pattern, not as the legitimate
    // serve-<hex> shape. Without the filter it would silently produce
    // the slot "watch-" which validates but encodes no DB scope.
    if let Some(rest) = serve_slot.strip_prefix("serve-").filter(|r| !r.is_empty()) {
        return Ok(format!("watch-{rest}"));
    }
    Err(anyhow::anyhow!(
        "ServerOptions::pid_lock_slot {serve_slot:?} is not a serve-family slot; \
         expected `serve` or `serve-<hex>`. Library embedders should derive the slot \
         via `cartog::state::slot_for_db(\"serve\", db_path)` so the watcher's slot \
         can be scoped to the same DB."
    ))
}

/// Environment variable that, when set to `0`, disables single-writer
/// election (every cartog process opens RW like pre-Phase-2 cartog). The
/// migration-busy-retry from Phase 6a remains the only defense in that mode.
pub const SINGLE_WRITER_ENV: &str = "CARTOG_SINGLE_WRITER";

#[derive(Default)]
pub struct ServerOptions {
    /// Directory for the server's PID file (written on startup, removed on
    /// graceful exit). `None` disables PID-file tracking. Consulted by
    /// `cartog self update` to detect a running peer.
    pub pid_lock_dir: Option<PathBuf>,
    /// Slot name used when acquiring the serve PID file. Required when
    /// `pid_lock_dir` is set — [`acquire_serve_lock`] hard-fails if a
    /// directory is configured without a slot, to prevent a global-slot
    /// peer from silently colliding with DB-scoped peers in
    /// multi-project setups. `None` is only valid when `pid_lock_dir`
    /// is also `None` (untracked mode used by tests).
    ///
    /// In the cartog binary the slot is derived via
    /// `cartog::state::slot_for_db("serve", db_path)`. Library
    /// embedders should follow the same shape: `<prefix>-<16 hex chars>`
    /// where the hex is a SHA-256 prefix of the canonicalized DB path.
    pub pid_lock_slot: Option<String>,
}

/// Outcome of trying to claim the `serve` lock at MCP startup.
#[derive(Debug)]
pub enum ServeLockOutcome {
    /// No `pid_lock_dir` configured — election skipped, this process runs
    /// as if it were the only one (legacy behavior, used in tests).
    Untracked,
    /// We won the election; the lock is held until this value is dropped.
    Primary(cartog_process_lock::ProcessLock),
    /// Another cartog process holds the lock. The caller decides whether
    /// to exit cleanly or (later, in Phase 4) attach read-only.
    Held(cartog_process_lock::ActiveLock),
}

/// Read `CARTOG_SINGLE_WRITER` from the environment. Defaults to election
/// enabled; set to `0` / `false` / `no` to opt out.
fn single_writer_election_enabled() -> bool {
    match std::env::var(SINGLE_WRITER_ENV) {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"),
        Err(_) => true,
    }
}

/// Acquire the serve PID lock with single-writer election. Returns
/// [`ServeLockOutcome::Held`] when a live peer already owns the slot so the
/// caller can branch on it (exit cleanly today, attach read-only in a
/// later phase).
pub fn acquire_serve_lock(opts: &ServerOptions) -> anyhow::Result<ServeLockOutcome> {
    let dir = match opts.pid_lock_dir.as_deref() {
        Some(d) => d,
        None => {
            // Inverse half-config: slot set but no dir. The slot is unused
            // and the caller's intent is silently dropped, so we surface
            // it as an error rather than running untracked.
            if opts.pid_lock_slot.is_some() {
                return Err(anyhow::anyhow!(
                    "ServerOptions::pid_lock_slot is set but pid_lock_dir is None; \
                     a slot without a directory is silently ignored — either set \
                     both fields or clear both to run untracked"
                ));
            }
            return Ok(ServeLockOutcome::Untracked);
        }
    };
    // Reject the dangerous half-configured state: pid_lock_dir set but no
    // slot. Falling back to a global SERVE_LOCK_SLOT here would let an
    // embedder claim `serve.pid` while a CLI peer on the same DB derives
    // `serve-<hash>.pid`, producing two primaries on the same DB. Require
    // the caller to opt into a slot explicitly (use
    // `cartog::state::slot_for_db("serve", db_path)` from the bin crate).
    let slot: &str = opts.pid_lock_slot.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "ServerOptions::pid_lock_dir is set but pid_lock_slot is None; \
             refusing to claim the global serve slot — pass a DB-scoped slot \
             (e.g. `cartog::state::slot_for_db(\"serve\", db_path)`)"
        )
    })?;
    if !single_writer_election_enabled() {
        // Kill switch: use the old overwrite-on-acquire behavior. We still
        // write our PID file so `cartog self update` and friends see us.
        let lock =
            cartog_process_lock::ProcessLock::acquire_overwriting(dir, slot).map_err(|e| {
                anyhow::anyhow!(
                    "failed to acquire serve PID lock at {} (single-writer election disabled): {e}",
                    dir.display()
                )
            })?;
        return Ok(ServeLockOutcome::Primary(lock));
    }
    match cartog_process_lock::ProcessLock::acquire(dir, slot) {
        Ok(lock) => Ok(ServeLockOutcome::Primary(lock)),
        Err(cartog_process_lock::AcquireError::Held(held)) => Ok(ServeLockOutcome::Held(held)),
        Err(cartog_process_lock::AcquireError::Io(e)) => Err(anyhow::anyhow!(
            "failed to acquire serve PID lock at {}: {e}",
            dir.display()
        )),
    }
}

/// Start the MCP server over stdio.
///
/// When `watch` is true, a background file watcher keeps the index fresh.
/// `rag_override` controls auto-embedding (requires `watch`): `Some(true)`/
/// `Some(false)` force on/off, `None` lets the watcher auto-detect from the DB.
#[allow(clippy::too_many_arguments)] // order-stable server knobs threaded from main
pub async fn run_server(
    db_path: &std::path::Path,
    watch: bool,
    rag_override: Option<bool>,
    rag_config: rag::EmbeddingProviderConfig,
    redact: indexer::RedactionConfig,
    lsp_overrides: std::collections::HashMap<String, Vec<String>>,
    filter: indexer::WalkFilter,
    opts: ServerOptions,
) -> anyhow::Result<()> {
    info!("starting cartog MCP server v{}", env!("CARGO_PKG_VERSION"));

    // Acquire first so an election loss is resolved before opening DB or
    // spawning the watcher.
    let (role, initial_lock, primary_to_watch) = match acquire_serve_lock(&opts)? {
        ServeLockOutcome::Primary(lock) => (Role::Primary, Some(lock), None),
        ServeLockOutcome::Untracked => (Role::Primary, None, None),
        ServeLockOutcome::Held(held) => {
            info!(
                primary_pid = held.pid,
                primary_start_time = ?held.start_time,
                "another cartog process is the primary writer for this DB \
                 (PID {}); attaching read-only. \
                 Indexing tools will return a read-only error; queries work normally. \
                 Promotion to primary happens automatically if the holder dies.",
                held.pid
            );
            (Role::ReadOnly, None, Some(held))
        }
    };

    // Only the primary owns the watcher: starting one as a secondary would
    // give us two indexers fighting over the DB. Read-only clients ride
    // along on the primary's index updates via WAL.
    let db_path_str = db_path.to_string_lossy().into_owned();
    // Derive the watcher's slot from the serve slot so per-DB scoping is
    // consistent across both PID files. When serve runs un-scoped (legacy /
    // tests), the watcher also runs un-scoped via the WATCH_LOCK_SLOT
    // fallback inside `cartog-watch`. An off-pattern serve slot is a hard
    // error here — silently using the global slot would let distinct
    // embedders collide on `watch.pid`.
    let watch_slot: Option<String> = match opts.pid_lock_slot.as_deref() {
        Some(s) => Some(serve_to_watch_slot(s)?),
        None => None,
    };
    // Staleness state the primary's watcher publishes for banner decisions.
    // `None` when there's no primary watcher (read-only peer / no `--watch`).
    let initial_stale: Option<Arc<cartog_watch::StaleState>> =
        (watch && role == Role::Primary).then(cartog_watch::StaleState::new);
    let initial_watch_handle: Option<WatchHandle> = if watch && role == Role::Primary {
        let cwd = std::env::current_dir()?;
        let mut config = WatchConfig::new(cwd);
        config.rag_override = rag_override;
        config.rag_config = rag_config.clone();
        config.redact = redact;
        config.walk_filter = filter.clone();
        config.stale = initial_stale.clone();
        // Claim the watcher's PID slot so a separately-running `cartog watch`
        // from a terminal correctly refuses to start against the same DB.
        config.pid_lock_dir = opts.pid_lock_dir.clone();
        config.pid_lock_slot = watch_slot.clone();
        match watch::spawn_watch(config, &db_path_str) {
            Ok(handle) => {
                info!(?rag_override, "background file watcher started");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to start background watcher, continuing without it");
                None
            }
        }
    } else {
        if watch && role == Role::ReadOnly {
            info!(
                "watcher skipped: this is a read-only secondary; the primary owns indexing \
                 (will start automatically on promotion)"
            );
        }
        None
    };

    // Construction opens the DB and builds the embedding provider — both
    // blocking, and a remote provider (openai/ollama) probes the network for its
    // dimension. Run off the runtime thread so a slow endpoint can't stall the
    // async server before it serves request #1.
    let server = {
        let db_path = db_path.to_path_buf();
        let rag_config = rag_config.clone();
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || match role {
            Role::Primary => CartogServer::new(&db_path, rag_config, redact, lsp_overrides, filter),
            Role::ReadOnly => {
                CartogServer::new_read_only(&db_path, rag_config, redact, lsp_overrides, filter)
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("server construction task panicked: {e}"))??
    };

    // Reflect initial watcher state on the server's flag so `cartog_stats`
    // surfaces it accurately from request #1. Will be updated by the
    // promoter on a successful post-promotion watcher spawn.
    server.watcher_active.store(
        initial_watch_handle.is_some(),
        std::sync::atomic::Ordering::Relaxed,
    );

    // Install the staleness state only when the watcher actually started, so
    // the cell stays consistent with `watcher_active`.
    if initial_watch_handle.is_some() {
        if let Ok(mut cell) = server.stale.lock() {
            *cell = initial_stale;
        }
    }

    // Shared cells so the promoter (if any) can install the lock + watcher
    // after winning election, and so the cells stay alive for the whole
    // `run_server` lifetime — Drop on shutdown fires here.
    let lock_cell = Arc::new(Mutex::new(initial_lock));
    let watch_cell = Arc::new(Mutex::new(initial_watch_handle));

    // The promoter requires all four of (held primary, state dir, serve
    // slot, watch slot) to be present. The all-Some case is the production
    // CLI path; partial-Some shapes occur only when a library embedder has
    // an off-pattern config — we silently run without a promoter rather
    // than panicking inside the spawned task, and `cartog_stats` keeps
    // surfacing the ReadOnly role so the operator can debug.
    let promoter_handle: Option<tokio::task::JoinHandle<()>> = if role == Role::ReadOnly {
        match (
            primary_to_watch,
            opts.pid_lock_dir.clone(),
            opts.pid_lock_slot.clone(),
            watch_slot.clone(),
        ) {
            (Some(primary), Some(state_dir), Some(serve_slot), Some(watch_slot)) => {
                let pinned = server
                    .db
                    .lock()
                    .ok()
                    .and_then(|g| g.pinned_attach().cloned());
                let cwd = (*server.cwd).to_path_buf();
                Some(tokio::task::spawn(promoter_task(PromoterArgs {
                    db: Arc::clone(&server.db),
                    role: Arc::clone(&server.role),
                    lock_cell: Arc::clone(&lock_cell),
                    watch_cell: Arc::clone(&watch_cell),
                    stale_cell: Arc::clone(&server.stale),
                    watcher_active: Arc::clone(&server.watcher_active),
                    embedding_provider: Arc::clone(&server.embedding_provider),
                    db_path: db_path.to_path_buf(),
                    state_dir,
                    serve_slot,
                    watch_slot,
                    cwd,
                    primary,
                    pinned,
                    watch_requested: watch,
                    rag_override,
                    rag_config,
                    redact: server.redact,
                    walk_filter: (*server.walk_filter).clone(),
                    poll_interval: DEFAULT_PROMOTER_POLL_INTERVAL,
                })))
            }
            _ => None,
        }
    } else {
        None
    };

    let service = server.serve(stdio()).await?;

    // Wait for any of: rmcp's normal shutdown (stdin EOF when the parent
    // dies, or an explicit close), SIGINT (Ctrl+C in a foreground terminal),
    // or SIGTERM (kill <pid>; only fires on Unix). Returning from
    // `run_server` lets the `ProcessLock` Drop impl unlink the PID file —
    // we deliberately avoid `std::process::exit` here to keep that cleanup.
    tokio::select! {
        result = service.waiting() => {
            result?;
        }
        _ = wait_for_sigint() => {
            info!("received SIGINT, shutting down");
        }
        _ = wait_for_sigterm() => {
            info!("received SIGTERM, shutting down");
        }
    }

    // Cancel the promoter task before this function returns, otherwise
    // dropping its JoinHandle would NOT stop the task — it would keep
    // polling against `args.db` for up to one `poll_interval` and could
    // race the shutdown by promoting after `run_server` is logically
    // done. `abort()` is non-blocking; the runtime drops the task on
    // its next yield point (the await in `tokio::time::sleep`).
    if let Some(h) = promoter_handle {
        h.abort();
    }

    // WatchHandle is dropped here, signaling the watcher thread to stop.
    info!("cartog MCP server stopped");
    Ok(())
}

/// Inputs for the Phase 5 promoter task. Bundled so the call site in
/// [`run_server`] stays readable; all fields are owned or `Arc`-cloned
/// before the task is spawned.
pub(crate) struct PromoterArgs {
    /// Live DB handle on the secondary. The promoter replaces its contents
    /// with a fresh RW [`Database`] when it takes ownership.
    pub(crate) db: Arc<Mutex<Database>>,
    /// Role flag visible to tool handlers. Flipped to `Primary` on
    /// successful promotion.
    pub(crate) role: Arc<AtomicRole>,
    /// Slot for the acquired [`ProcessLock`] once we win election.
    pub(crate) lock_cell: Arc<Mutex<Option<cartog_process_lock::ProcessLock>>>,
    /// Slot for the watcher handle spawned after promotion (when the user
    /// asked for `serve --watch`).
    pub(crate) watch_cell: Arc<Mutex<Option<WatchHandle>>>,
    /// Server's staleness cell. The promoter installs a fresh [`StaleState`]
    /// here when it spawns a post-promotion watcher, so banners work after
    /// failover.
    pub(crate) stale_cell: Arc<Mutex<Option<Arc<cartog_watch::StaleState>>>>,
    /// Reflects whether a file watcher is currently running. Set to true
    /// on a successful post-promotion spawn, left false if the watcher
    /// failed to start (degraded Primary: surfaced in `cartog_stats`).
    pub(crate) watcher_active: Arc<std::sync::atomic::AtomicBool>,
    /// Secondary's embedding provider. Used to reconcile the on-disk
    /// embedding fingerprint against the secondary's actual provider when
    /// we promote — CartogServer::new does this on first start, but
    /// open_existing_ro deliberately skips it.
    pub(crate) embedding_provider: Arc<Mutex<Box<dyn rag::provider::EmbeddingProvider>>>,
    pub(crate) db_path: std::path::PathBuf,
    pub(crate) state_dir: std::path::PathBuf,
    /// Slot to claim when the promoter wins election. Matches the slot the
    /// originally-attached primary held — DB-scoped (`serve-<hash>`) in
    /// production, the global `SERVE_LOCK_SLOT` in legacy / test paths.
    pub(crate) serve_slot: String,
    /// Slot the post-promotion watcher claims. Derived from `serve_slot`
    /// (see `serve_to_watch_slot`) so both PID files share their scope.
    pub(crate) watch_slot: String,
    /// CWD captured at server startup. Reused for the post-promotion
    /// watcher so the watch root doesn't follow a later `std::env::set_current_dir`.
    pub(crate) cwd: std::path::PathBuf,
    /// Snapshot of the primary we attached behind. Promotion fires when
    /// this process is no longer running.
    pub(crate) primary: cartog_process_lock::ActiveLock,
    /// What we saw in `metadata` at attach time. Compared against the
    /// on-disk values at promotion time so we abort cleanly if the primary
    /// upgraded the schema or swapped the embedding stack under us.
    pub(crate) pinned: Option<PinnedAttach>,
    pub(crate) watch_requested: bool,
    /// Auto-embed override for the post-promotion watcher; `None` = auto-detect.
    pub(crate) rag_override: Option<bool>,
    pub(crate) rag_config: rag::EmbeddingProviderConfig,
    pub(crate) redact: indexer::RedactionConfig,
    /// Walk filter (`[index] exclude` globs + gitignore policy) forwarded to
    /// the post-promotion watcher.
    pub(crate) walk_filter: indexer::WalkFilter,
    /// Polling interval. Const in production
    /// ([`DEFAULT_PROMOTER_POLL_INTERVAL`]); override in tests to keep
    /// the suite fast.
    pub(crate) poll_interval: std::time::Duration,
}

/// How often the promoter checks whether the primary is still alive. Kept
/// short enough that handoff feels responsive to a user closing the other
/// Claude Code window, long enough that the polling cost is invisible.
pub(crate) const DEFAULT_PROMOTER_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Background task that runs in read-only mode and watches the primary's
/// liveness. On primary death, validates schema/fingerprint and attempts
/// promotion (atomic O_EXCL lock acquire → swap DB to RW → spawn watcher
/// if the user asked for one → flip role). Exits cleanly on schema drift
/// or when another reader wins the race.
pub(crate) async fn promoter_task(args: PromoterArgs) {
    loop {
        tokio::time::sleep(args.poll_interval).await;

        // Primary still alive? Liveness uses start_time when available
        // (closes the PID-reuse window).
        let primary_alive = match args.primary.start_time {
            Some(st) => cartog_process_lock::is_same_process(args.primary.pid, st),
            None => cartog_process_lock::is_alive(args.primary.pid),
        };
        if primary_alive {
            continue;
        }

        info!(
            primary_pid = args.primary.pid,
            "primary cartog process is gone; attempting promotion to primary"
        );

        // Cheap pre-check: skip the lock acquire if state already diverged.
        // We re-validate AFTER acquire too — the TOCTOU window between
        // here and acquire lets a third writer slip in.
        if let Err(e) = validate_pinned_state(&args.db_path, args.pinned.as_ref()) {
            info!(error = %e, "aborting promotion: on-disk state diverged before lock acquire");
            return;
        }

        // Atomic O_EXCL acquire. Other readers may race us; the loser stays
        // read-only and tries again on the next tick.
        let new_lock =
            match cartog_process_lock::ProcessLock::acquire(&args.state_dir, &args.serve_slot) {
                Ok(lock) => lock,
                Err(cartog_process_lock::AcquireError::Held(held)) => {
                    info!(
                        new_primary_pid = held.pid,
                        "another reader won the promotion race; staying read-only"
                    );
                    continue;
                }
                Err(cartog_process_lock::AcquireError::Io(e)) => {
                    tracing::warn!(error = %e, "promotion lock acquire failed; staying read-only");
                    continue;
                }
            };

        // Re-validate AFTER acquire: between the first validate and the
        // acquire, a third writer could have promoted itself, upgraded the
        // schema, and exited (releasing the lock to us). We now own the
        // lock, so the state can't change again — checking once here is
        // sufficient. On drift, drop the lock and exit cleanly so the
        // user restarts against the new schema.
        if let Err(e) = validate_pinned_state(&args.db_path, args.pinned.as_ref()) {
            info!(
                error = %e,
                "aborting promotion: on-disk state diverged after lock acquire"
            );
            drop(new_lock);
            return;
        }

        // Open a fresh RW Database. We DON'T install it into args.db yet —
        // we first reconcile the embedding fingerprint and try to spawn
        // the watcher (if requested), so a failure at either step rolls
        // back cleanly (drop rw + lock, loop and retry next tick) without
        // leaving the secondary with a half-promoted state.
        let rw = match Database::open_existing_rw(&args.db_path) {
            Ok(rw) => rw,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "open_existing_rw failed during promotion; dropping lock and retrying"
                );
                drop(new_lock);
                continue;
            }
        };

        // Reconcile the embedding fingerprint against the freshly-opened
        // RW connection. CartogServer::new does this on first start, but
        // a ReadOnly secondary skips it (open_existing_ro is a verbatim
        // attach). Without this step, vectors written via cartog_rag_index
        // after promotion would silently mismatch the fingerprint persisted
        // by the previous primary.
        let provider_fp = match args.embedding_provider.lock() {
            Ok(guard) => rag::fingerprint_of(guard.as_ref()),
            Err(_) => {
                tracing::error!(
                    "embedding_provider mutex poisoned; cannot reconcile fingerprint, exiting promoter without promoting"
                );
                drop(rw);
                drop(new_lock);
                return;
            }
        };
        if let Err(e) = rw.reconcile_embedding_fingerprint(&provider_fp) {
            tracing::warn!(
                error = %e,
                "embedding fingerprint reconcile failed during promotion; dropping lock and retrying"
            );
            drop(rw);
            drop(new_lock);
            continue;
        }

        // Try to spawn the watcher BEFORE flipping role / installing the
        // lock. If spawn fails (e.g. a separately-running `cartog watch`
        // grabbed the watch slot in the gap between our serve-slot acquire
        // and here), we drop both the rw handle and the new lock and loop
        // — the next poll re-checks primary liveness and re-attempts. This
        // keeps the invariant "Primary always owns its watcher when
        // watch_requested" intact rather than leaving a degraded Primary
        // with no watcher and only a stderr warning.
        // Fresh staleness state for the post-promotion watcher; installed
        // into the server's cell alongside the watcher handle below.
        let new_stale = cartog_watch::StaleState::new();
        let new_watch_handle: Option<WatchHandle> = if args.watch_requested {
            // Reuse the cwd captured at server startup, not
            // std::env::current_dir() — the latter follows runtime
            // chdir() calls (rare in MCP children but possible in tests
            // and embedded uses).
            let mut config = WatchConfig::new(args.cwd.clone());
            config.rag_override = args.rag_override;
            config.rag_config = args.rag_config.clone();
            config.redact = args.redact;
            config.walk_filter = args.walk_filter.clone();
            config.stale = Some(Arc::clone(&new_stale));
            config.pid_lock_dir = Some(args.state_dir.clone());
            config.pid_lock_slot = Some(args.watch_slot.clone());
            // Skip migrations because we validated the schema when we
            // attached read-only — re-running them would re-trigger the
            // embedding-dimension reconcile the election prevents.
            //
            // We DO still acquire `watch.pid` (the watcher's own slot)
            // even though we already hold `serve.pid`. The two slots
            // serve different consumers: `serve.pid` blocks other MCP
            // servers, `watch.pid` blocks a separately-running
            // `cartog watch` from a terminal. Without the watch slot a
            // terminal `cartog watch` would happily start and create
            // two concurrent indexers writing to the same DB.
            config.skip_migrations = true;
            let db_path_str = args.db_path.to_string_lossy().into_owned();
            match watch::spawn_watch(config, &db_path_str) {
                Ok(handle) => Some(handle),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "post-promotion watcher failed to start; rolling back promotion and retrying"
                    );
                    drop(rw);
                    drop(new_lock);
                    continue;
                }
            }
        } else {
            None
        };

        // From here on the commit is one-way: install RW DB, lock, watcher
        // (if any), and flip role. A poisoned cell is fatal for symmetric
        // reasons — letting the lock Drop unlink the PID file while role
        // stays ReadOnly would let a fresh `cartog serve` win the next
        // O_EXCL acquire alongside our still-running RW DB connection.
        match args.db.lock() {
            Ok(mut guard) => {
                *guard = rw;
            }
            Err(_) => {
                tracing::error!("db mutex poisoned; cannot promote, exiting promoter task");
                drop(new_lock);
                if let Some(h) = new_watch_handle {
                    drop(h);
                }
                return;
            }
        }
        match args.lock_cell.lock() {
            Ok(mut guard) => {
                *guard = Some(new_lock);
            }
            Err(_) => {
                tracing::error!(
                    "lock_cell mutex poisoned; cannot install serve lock, exiting promoter task without flipping role"
                );
                drop(new_lock);
                if let Some(h) = new_watch_handle {
                    drop(h);
                }
                return;
            }
        }
        if let Some(handle) = new_watch_handle {
            // If watch_cell is poisoned, dropping `handle` here signals
            // shutdown to the watcher thread (its shutdown flag flips in
            // Drop). We've already committed the lock and DB swap, so we
            // can't roll back — proceed degraded with watcher_active=false
            // so `cartog_stats` surfaces the missing watcher.
            match args.watch_cell.lock() {
                Ok(mut guard) => {
                    *guard = Some(handle);
                    if let Ok(mut cell) = args.stale_cell.lock() {
                        *cell = Some(new_stale);
                    }
                    args.watcher_active
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                Err(_) => {
                    tracing::error!(
                        "watch_cell mutex poisoned; post-promotion watcher discarded — \
                         server is Primary but will not auto-reindex"
                    );
                    drop(handle);
                }
            }
        }
        args.role.store(Role::Primary);

        info!("promoted to primary for {}", args.db_path.display());
        return;
    }
}

/// Re-read `schema_version` and the embedding fingerprint from disk and
/// compare to what the secondary saw at attach. Used by the promoter
/// before attempting to take over — if either changed, a third writer
/// already took over and upgraded under us.
pub(crate) fn validate_pinned_state(
    db_path: &std::path::Path,
    pinned: Option<&PinnedAttach>,
) -> anyhow::Result<()> {
    let pinned = match pinned {
        Some(p) => p,
        None => return Ok(()),
    };
    // Bump AFTER the None-pin early return so the test counter only
    // tracks meaningful validations (a None pin is a trivial pass).
    #[cfg(test)]
    test_validate_call_counter::bump();
    let reader = Database::open_readonly(db_path)
        .map_err(|e| anyhow::anyhow!("re-attach read-only failed: {e}"))?;
    let now = reader
        .pinned_attach()
        .ok_or_else(|| anyhow::anyhow!("internal: re-attached DB has no pinned state"))?;
    if now != pinned {
        anyhow::bail!(
            "DB metadata changed since attach: was {pinned:?}, now {now:?} (another writer took over)"
        );
    }
    Ok(())
}

/// Resolve to a future that completes when the process receives SIGTERM.
/// On Windows this also covers `CTRL_CLOSE_EVENT` (console window closed)
/// and `CTRL_SHUTDOWN_EVENT`. On platforms where the relevant signal source
/// can't be installed, the future never completes — `service.waiting()`
/// remains the shutdown signal.
///
/// `wait_for_sigint` wraps `tokio::signal::ctrl_c()` so a failure to
/// install the SIGINT handler does NOT immediately win the
/// `tokio::select!` branch with an `Err` resolved future — without the
/// wrapper, `_ = tokio::signal::ctrl_c()` would treat installation
/// failure as "SIGINT fired" and exit. Mirrors `wait_for_sigterm`.
async fn wait_for_sigint() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(error = %e, "failed to install SIGINT handler; falling back to other shutdown signals");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(unix)]
async fn wait_for_sigterm() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to install SIGTERM handler; falling back to stdin-EOF only");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(windows)]
async fn wait_for_sigterm() {
    use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
    let close = ctrl_close();
    let shutdown = ctrl_shutdown();
    match (close, shutdown) {
        (Ok(mut c), Ok(mut s)) => {
            tokio::select! {
                _ = c.recv() => {}
                _ = s.recv() => {}
            }
        }
        _ => {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn wait_for_sigterm() {
    std::future::pending::<()>().await;
}

/// Test-only call counter for `validate_pinned_state`. Lets the promoter
/// suite assert that BOTH the pre-acquire and post-acquire validate
/// branches fire — deleting either call site would surface as a count
/// regression. Production builds compile this whole module away.
///
/// `COUNT` is the running tally; `SERIAL` is a separate `tokio` Mutex
/// tests hold for their full duration to serialize against any sibling
/// test that also reads/writes the counter (Cargo runs tests in parallel
/// by default, and these statics are process-global). We use the async
/// Mutex so the guard can be held across `.await` in `#[tokio::test]`
/// bodies; sync tests use `blocking_lock()`.
#[cfg(test)]
pub(crate) mod test_validate_call_counter {
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    pub(crate) static COUNT: AtomicUsize = AtomicUsize::new(0);

    pub(crate) fn bump() {
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn reset() {
        COUNT.store(0, Ordering::SeqCst);
    }

    pub(crate) fn snapshot() -> usize {
        COUNT.load(Ordering::SeqCst)
    }
}
