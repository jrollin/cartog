//! Update orchestration: in-place upgrade, rollback, and the defer/apply flow.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use super::{exit, *};
use crate::state::{self, State};
use cartog::time_fmt::rfc3339_now;

/// Which variant of `cartog self update` the user invoked. Closed set — the
/// three mode flags are mutually exclusive at the clap layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateMode {
    /// Read-only: report whether an update exists (`--check`).
    Check,
    /// Arm a deferred update without swapping (`--defer`). `Some(version)`
    /// pins an explicit target (`--to`); `None` resolves the latest stable.
    Defer(Option<String>),
    /// Apply a previously-armed deferred update (`--apply-pending`). The bool
    /// is `--at-startup`: exclude this project's own serve/watch peer from the
    /// peer-wait (the same-session peer never clears; the swap is safe under it).
    ApplyPending { at_startup: bool },
    /// Default: upgrade in place now.
    Now,
}

impl UpdateMode {
    /// Map the (mutually exclusive) clap flags to a mode. clap guarantees at
    /// most one of check/defer/apply_pending is set (via `conflicts_with_all`)
    /// and that `to` is only present with `defer` (via `requires`).
    #[must_use]
    pub fn from_flags(
        check: bool,
        defer: bool,
        to: Option<String>,
        apply_pending: bool,
        at_startup: bool,
    ) -> Self {
        match (check, defer, apply_pending) {
            (true, _, _) => Self::Check,
            (_, true, _) => Self::Defer(to),
            (_, _, true) => Self::ApplyPending { at_startup },
            _ => Self::Now,
        }
    }
}

/// `cartog self update [--check|--defer|--apply-pending] [--quiet] [--json]`.
///
/// - `--check` is read-only (see [`run_check`]).
/// - `--defer` arms a deferred update without swapping (see [`run_arm`]) —
///   succeeds even while a peer is running.
/// - `--apply-pending` applies a previously-armed update (see
///   [`run_apply_pending`]) once no peer holds the lock.
///
/// In the default upgrade mode the flow is:
/// 1. Refuse for cargo-installed binaries (exit 3) — direct user to
///    `cargo install cartog --force`.
/// 2. Refuse if a peer `cartog serve`/`watch` is still running (exit 6).
/// 3. Fetch the latest stable tag. Already up to date → exit 0.
/// 4. Download the platform tarball/zip and `SHA256SUMS`, verify the
///    checksum (exit 4 on mismatch), atomically swap the binary in
///    place, preserve `<bin>.old`, smoke-test the new binary
///    (exit 5 on failure → restore `.old`).
pub fn cmd_self_update(mode: UpdateMode, db_path: &Path, quiet: bool, json: bool) -> Result<()> {
    let exit_code = match mode {
        UpdateMode::Check => run_check(quiet, json),
        UpdateMode::Defer(target) => run_arm(target.as_deref(), quiet, json),
        UpdateMode::ApplyPending { at_startup } => {
            run_apply_pending(at_startup, db_path, quiet, json)
        }
        UpdateMode::Now => run_upgrade(quiet, json),
    };
    std::process::exit(exit_code);
}

/// Drive the read-only `--check` flow and return the desired exit code.
/// Split out so `cmd_self_update` stays readable and the exit-code mapping
/// lives in one place.
fn run_check(quiet: bool, json: bool) -> i32 {
    let api_url = github_latest_url();
    match fetch_latest_version(&api_url) {
        Ok(latest) => {
            let outcome = CheckOutcome::ok(env!("CARGO_PKG_VERSION"), &latest);
            if !quiet {
                emit_check_outcome(&outcome, json);
            }
            if outcome.outdated == Some(true) {
                1
            } else {
                0
            }
        }
        Err(e) => {
            if !quiet {
                let outcome = CheckOutcome::failed(env!("CARGO_PKG_VERSION"), &e.to_string());
                emit_check_outcome(&outcome, json);
            }
            2
        }
    }
}

fn emit_check_outcome(outcome: &CheckOutcome, json: bool) {
    if json {
        // Serialising a flat struct of strings/bools never fails.
        println!(
            "{}",
            serde_json::to_string(outcome).expect("CheckOutcome serialises")
        );
    } else {
        println!("{}", outcome.to_human());
    }
}

/// `cartog self rollback` entry point.
///
/// Restores the binary previously saved at `<bin>.old` (created by a
/// successful `self update`) onto `<bin>`. The currently-running broken
/// binary is staged aside via `Move::replace_using_temp` and then deleted
/// so the user is left with a single binary and no leftover sibling.
///
/// Exit codes:
/// - `0` — successfully restored
/// - `1` — no `.old` to restore
/// - `2` — swap failed
///
/// Platform note: on Windows, renaming a running `.exe` is forbidden by
/// the OS and the swap will fail with exit 2. Users on Windows who need
/// to roll back must invoke rollback from a different running process.
pub fn cmd_self_rollback() -> Result<()> {
    let exit_code = run_rollback();
    std::process::exit(exit_code);
}

fn run_rollback() -> i32 {
    let current_bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cartog: cannot resolve current exe: {e}");
            return 2;
        }
    };
    let backup_path = backup_path_for(&current_bin);
    if !backup_path.exists() {
        eprintln!(
            "cartog: no previous binary to roll back to (looked for {})",
            backup_path.display(),
        );
        return 1;
    }

    // Stage the currently-running binary aside via a per-PID temp path so
    // a parallel `self update` cannot collide with our intermediate file.
    let install_dir = match current_bin.parent() {
        Some(p) => p,
        None => {
            eprintln!(
                "cartog: current exe {} has no parent directory",
                current_bin.display(),
            );
            return 2;
        }
    };
    let intermediate = install_dir.join(format!(".cartog.broken.{}.tmp", std::process::id()));

    if let Err(e) = self_update::Move::from_source(&backup_path)
        .replace_using_temp(&intermediate)
        .to_dest(&current_bin)
    {
        eprintln!("cartog: rollback failed: {e}");
        return 2;
    }

    // Per RD-2 the user is back to a single binary with no `.old` sibling.
    // Move::to_dest consumed `<bin>.old`, so only the staged broken binary
    // remains at `intermediate`. Best-effort delete; a failure here is
    // worth surfacing but does not invalidate the rollback.
    if let Err(e) = std::fs::remove_file(&intermediate) {
        tracing::warn!(
            error = %e,
            path = %intermediate.display(),
            "rollback succeeded but failed to clean up staged broken binary",
        );
    }

    println!(
        "cartog: rolled back to previous binary ({})",
        current_bin.display(),
    );
    0
}

// ── --check internals ─────────────────────────────────────────────────

/// Drive the upgrade path and return the desired exit code.
fn run_upgrade(quiet: bool, json: bool) -> i32 {
    let source = effective_install_source();
    if source == "cargo" {
        emit_upgrade_message(
            quiet,
            json,
            "cargo",
            "cartog was installed via cargo. Run `cargo install cartog --force` to upgrade.",
        );
        return exit::CARGO_INSTALL_REFUSED;
    }

    if let Some(dir) = state::default_state_dir() {
        let active = cartog_process_lock::find_active_locks(&dir);
        if let Some(peer) = active.first() {
            emit_upgrade_message(
                quiet,
                json,
                "peer-running",
                &format!(
                    "another cartog process is running ({slot}, PID {pid}); stop it before updating",
                    slot = peer.slot,
                    pid = peer.pid,
                ),
            );
            return exit::PEER_RUNNING;
        }
    }

    // 3. Fetch latest release tag.
    let api_url = github_latest_url();
    let latest = match fetch_latest_version(&api_url) {
        Ok(v) => v,
        Err(e) => {
            emit_upgrade_message(quiet, json, "fetch-failed", &e.to_string());
            return exit::NETWORK_OR_PARSE_ERROR;
        }
    };
    let current = env!("CARGO_PKG_VERSION");
    if compare_stable_versions(current, &latest) != std::cmp::Ordering::Less {
        if !quiet {
            if json {
                let payload = serde_json::json!({
                    "status": "up-to-date",
                    "current": current,
                    "latest": latest,
                });
                println!("{payload}");
            } else {
                println!("cartog: already up to date ({current})");
            }
        }
        return exit::SUCCESS;
    }

    // 4. Download tarball + SHA256SUMS, verify, swap.
    match perform_upgrade(current, &latest, quiet, json) {
        Ok(()) => exit::SUCCESS,
        Err(UpgradeError::Network(msg)) => {
            emit_upgrade_message(quiet, json, "fetch-failed", &msg);
            exit::NETWORK_OR_PARSE_ERROR
        }
        Err(UpgradeError::Checksum(msg)) => {
            emit_upgrade_message(quiet, json, "checksum-failed", &msg);
            exit::CHECKSUM_FAILED
        }
        // Smoke and Filesystem both surface as exit 5 here: the new binary
        // failed and the old one was restored (or a true disk fault occurred).
        Err(UpgradeError::Smoke(msg)) | Err(UpgradeError::Filesystem(msg)) => {
            emit_upgrade_message(quiet, json, "filesystem-failed", &msg);
            exit::DISK_OR_PERMISSION_FAILED
        }
    }
}

/// Categorised error so [`run_upgrade`] can map to the right exit code.
pub(crate) enum UpgradeError {
    Network(String),
    Checksum(String),
    Filesystem(String),
    /// The downloaded binary swapped in but failed its smoke test, and the
    /// previous binary was restored. Deterministic for a fixed target — unlike
    /// [`Self::Filesystem`] (a transient disk/permission fault), retrying the
    /// same tarball will fail identically, so a deferred update clears its
    /// intent on this rather than retry-looping every session.
    Smoke(String),
}

// ── deferred update (--defer / --apply-pending) ───────────────────────

/// How long `--apply-pending` waits for a peer `serve`/`watch` lock to clear
/// before giving up and deferring to the next session. At a session boundary
/// the serve process is already exiting and unlinks its PID file shortly after
/// dropping its `ProcessLock`. The exact SessionEnd-vs-MCP-teardown ordering in
/// Claude Code is not contractually guaranteed, so this budget is generous (it
/// matches the secondary-promoter takeover window) to absorb teardown lag and
/// land the swap on the same boundary rather than deferring a session.
pub(crate) const APPLY_PEER_WAIT: Duration = Duration::from_secs(10);

/// Ceiling when every blocker is one of *this project's* own slots. That is
/// either our own `serve` mid-teardown (its lock clears within ~1s) or a sibling
/// window on the same repo (never clears while it stays open) — slot names are
/// db-path hashes carrying no session identity, so the two are indistinguishable
/// here. Bounded well under the SessionEnd hook's budget so the ambiguous case
/// cannot get the hook killed (`Hook cancelled`, #154); anything that misses the
/// boundary is picked up by the SessionStart `--at-startup` catch-up.
pub(crate) const APPLY_OWN_PEER_GRACE: Duration = Duration::from_secs(2);

/// No wait at all once a blocker is known to belong to another project: it holds
/// its lock for as long as that session stays open, so polling can only burn the
/// hook's budget before deferring anyway (#154). A single probe decides; the
/// intent stays armed and the next boundary retries.
pub(crate) const APPLY_FOREIGN_PEER_WAIT: Duration = Duration::ZERO;

/// Poll interval while waiting for peers to clear in [`wait_for_no_peer_excluding`].
const APPLY_PEER_POLL: Duration = Duration::from_millis(200);

/// Machine-global lock slot serializing concurrent `--apply-pending` swaps.
/// Not DB-scoped: every apply on this machine swaps the same binary, so they
/// must mutually exclude regardless of which project triggered them.
pub(crate) const APPLY_LOCK_SLOT: &str = "apply-update";

/// Test seam: overrides the whole tiered budget (milliseconds) so the suite can
/// exercise the timeout branch without a real wait. A true override, not a cap —
/// a value above a tier still applies, so a test can exercise the teardown-lag
/// absorption path.
const TEST_APPLY_PEER_WAIT_MS_ENV: &str = "CARTOG_TEST_APPLY_PEER_WAIT_MS";

/// The test-seam override, if set and parseable.
fn test_peer_wait_override() -> Option<Duration> {
    std::env::var(TEST_APPLY_PEER_WAIT_MS_ENV)
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
}

/// Whether a pending update should still be applied given the running version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyDecision {
    /// The running binary already satisfies (or exceeds) the target — clear
    /// the intent and no-op.
    Skip,
    /// The target is newer than the running binary — proceed with the swap.
    Proceed,
}

/// Pure idempotency/staleness check, factored out for unit testing. Applies
/// only when the armed `target` is a well-formed version strictly newer than
/// the running `current`. A malformed/garbage `target` (hand-edited or
/// foreign state) is treated as stale rather than silently parsed to `0.0.0`,
/// so the caller clears it with an honest message instead of a misleading
/// "already at X".
pub(crate) fn decide_apply(current: &str, target: &str) -> ApplyDecision {
    if !is_stable_semver(target) {
        return ApplyDecision::Skip;
    }
    if compare_stable_versions(current, target) == std::cmp::Ordering::Less {
        ApplyDecision::Proceed
    } else {
        ApplyDecision::Skip
    }
}

/// What `--apply-pending` does with the armed intent after a failed swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntentDisposition {
    /// Keep the intent armed; the next boundary retries (transient fault).
    Keep,
    /// Clear the intent; retrying the same target would fail identically.
    Clear,
}

/// Pure mapping from an upgrade failure to whether the deferred intent should
/// survive. Factored out so all four dispositions are unit-testable without a
/// real failing upgrade. Network and a true filesystem/permission fault are
/// transient (keep); a checksum mismatch or a smoke-test failure is
/// deterministic for a fixed target (clear, else it retry-loops every session).
pub(crate) fn intent_disposition(err: &UpgradeError) -> IntentDisposition {
    match err {
        UpgradeError::Network(_) | UpgradeError::Filesystem(_) => IntentDisposition::Keep,
        UpgradeError::Checksum(_) | UpgradeError::Smoke(_) => IntentDisposition::Clear,
    }
}

/// Poll for peer locks to clear, up to `budget`. Returns `Ok(())` once no peer
/// holds a serve/watch lock; `Err(peer)` if the budget elapses with one still
/// live. `find_active_locks` already applies `is_same_process`, so a PID
/// reused by an unrelated process is correctly treated as not-a-peer.
///
/// The [`APPLY_LOCK_SLOT`] coordination lock is excluded: it is held by a
/// concurrent `--apply-pending`, not a serve/watch process whose live binary we
/// would unlink. A second apply must reach the lock-acquire step (and cleanly
/// skip via `Held`) rather than time out here waiting on its sibling.
/// Test-only thin wrapper: the no-exclusions case. Production always goes
/// through [`wait_for_no_peer_excluding`] (empty slice when not at startup).
#[cfg(test)]
pub(crate) fn wait_for_no_peer(
    state_dir: &Path,
    budget: Duration,
) -> std::result::Result<(), cartog_process_lock::ActiveLock> {
    wait_for_no_peer_excluding(state_dir, budget, &[])
}

/// Poll for peer locks to clear, up to `budget`. Treats the [`APPLY_LOCK_SLOT`]
/// coordination lock and any slot in `extra_excluded` as not-a-peer. Returns
/// `Err(peer)` if the budget elapses with a real peer still live. Used by
/// `--apply-pending --at-startup` to ignore this project's own serve/watch
/// peer, which never clears mid-session.
pub(crate) fn wait_for_no_peer_excluding(
    state_dir: &Path,
    budget: Duration,
    extra_excluded: &[String],
) -> std::result::Result<(), cartog_process_lock::ActiveLock> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let mut active = cartog_process_lock::find_active_locks(state_dir);
        active.retain(|lock| {
            lock.slot != APPLY_LOCK_SLOT && !extra_excluded.iter().any(|s| s == &lock.slot)
        });
        match active.into_iter().next() {
            None => return Ok(()),
            Some(peer) => {
                if std::time::Instant::now() >= deadline {
                    return Err(peer);
                }
                std::thread::sleep(APPLY_PEER_POLL);
            }
        }
    }
}

/// Pick the peer-wait budget from the slots that can actually block the wait.
///
/// `own_slots` are this project's serve/watch slots; `active_slots` is every live
/// peer slot; `excluded_slots` are the slots the wait itself ignores (the
/// `--at-startup` own-peer exclusion). The decision runs over
/// `active_slots - excluded_slots` so the budget and
/// [`wait_for_no_peer_excluding`] agree on what counts as a blocker — evaluating
/// it over the unfiltered set would grant a long budget for a peer the wait
/// never even looks at.
///
/// Tiers, in the order they are tested:
/// - **nothing blocking** → [`APPLY_PEER_WAIT`]: no lock stands in the way yet, so
///   keep the generous window that absorbs a serve appearing mid-wait.
/// - **any blocker is foreign** → [`APPLY_FOREIGN_PEER_WAIT`]: one unclearable
///   lock keeps the wait blocked for the whole budget no matter what the other
///   blockers do, so waiting is futile. This is the `any`, not `all`, case — the
///   inverse would let a co-live own peer buy a full budget the foreign lock then
///   consumes (#154).
/// - **every blocker is our own** → [`APPLY_OWN_PEER_GRACE`]: plausible teardown
///   lag, but capped because a same-repo sibling window is indistinguishable.
pub(crate) fn peer_wait_budget(
    own_slots: &[String],
    active_slots: &[String],
    excluded_slots: &[String],
) -> Duration {
    let blocking: Vec<&String> = active_slots
        .iter()
        .filter(|slot| !excluded_slots.iter().any(|ex| ex == *slot))
        .collect();
    if blocking.is_empty() {
        return APPLY_PEER_WAIT;
    }
    let any_foreign = blocking
        .iter()
        .any(|slot| !own_slots.iter().any(|own| own == *slot));
    if any_foreign {
        APPLY_FOREIGN_PEER_WAIT
    } else {
        APPLY_OWN_PEER_GRACE
    }
}

/// The budget `--apply-pending` actually waits: the test seam when set, else the
/// tier from [`peer_wait_budget`]. Single place the call site consults, so the
/// composition is unit-testable rather than assembled inline.
pub(crate) fn effective_peer_wait(
    own_slots: &[String],
    active_slots: &[String],
    excluded_slots: &[String],
) -> Duration {
    test_peer_wait_override()
        .unwrap_or_else(|| peer_wait_budget(own_slots, active_slots, excluded_slots))
}

/// Build the exit-6 explanation. Always names the blocking slot + PID: it is the
/// only identification the caller gets (`emit_upgrade_message` serialises just
/// status + message), and `update_on_exit.sh` deliberately writes no `last-error`
/// for exit 6, so dropping it would leave nothing to grep when an update never
/// lands.
///
/// `foreign_processes` counts distinct processes outside this project (see
/// [`foreign_peer_process_count`]), so this session's own server is never
/// reported as somebody else's. Says the update retries on its own, and mentions
/// closing the other sessions only as an optional way to land it sooner — a
/// blocker is not necessarily transient (a wedged serve or a hand-started
/// `cartog watch` may never exit), so promising "no action needed" would be
/// false. Deliberately names no specific product: a lock holder can be any MCP
/// client or a plain terminal `cartog watch`.
pub(crate) fn peer_running_message(
    blocking_slot: &str,
    blocking_pid: u32,
    foreign_processes: usize,
) -> String {
    if foreign_processes > 0 {
        let sessions = if foreign_processes == 1 {
            "session"
        } else {
            "sessions"
        };
        format!(
            "cartog is still running in {foreign_processes} other {sessions} \
             (blocking lock: {blocking_slot}, PID {blocking_pid}); deferred update kept \
             and retries at the next session boundary — close the other sessions if you \
             want it to land sooner"
        )
    } else {
        format!(
            "a cartog process for this project is still shutting down \
             ({blocking_slot}, PID {blocking_pid}); deferred update kept and retries \
             at the next session boundary"
        )
    }
}

/// Slots currently holding a live lock, excluding the apply-coordination lock.
/// Mirrors the filtering in [`wait_for_no_peer_excluding`] so the budget decision
/// and the wait itself agree on what counts as a peer.
pub(crate) fn active_peer_slots(state_dir: &Path) -> Vec<String> {
    cartog_process_lock::find_active_locks(state_dir)
        .into_iter()
        .filter(|lock| lock.slot != APPLY_LOCK_SLOT)
        .map(|lock| lock.slot)
        .collect()
}

/// How many distinct processes hold a lock that is neither the apply-coordination
/// lock, one of `excluded_slots`, nor one of this project's `own_slots` — i.e.
/// peers belonging to another project or session.
///
/// Counts PIDs, not slots: one session's `serve` and `watch` locks are held by a
/// single process, so counting slots would report one session as two peers.
pub(crate) fn foreign_peer_process_count(
    state_dir: &Path,
    own_slots: &[String],
    excluded_slots: &[String],
) -> usize {
    let pids: std::collections::HashSet<u32> = cartog_process_lock::find_active_locks(state_dir)
        .into_iter()
        .filter(|lock| lock.slot != APPLY_LOCK_SLOT)
        .filter(|lock| !excluded_slots.iter().any(|ex| ex == &lock.slot))
        .filter(|lock| !own_slots.iter().any(|own| own == &lock.slot))
        .map(|lock| lock.pid)
        .collect();
    pids.len()
}

/// The serve/watch lock slots this project's own peers would hold, so
/// `--apply-pending --at-startup` can exclude them from the peer-wait. Covers
/// both command families on the resolved DB path only — unlike
/// `migrate::target_db_slots`, it does NOT also hash the legacy `.cartog.db`
/// path, so a peer started pre-migration on the legacy path is not excluded
/// (it blocks the apply, which is the safe default).
fn current_project_peer_slots(db_path: &Path) -> Vec<String> {
    ["serve", "watch"]
        .iter()
        .map(|prefix| state::slot_for_db(prefix, db_path))
        .collect()
}

/// Drive `--defer`: arm a deferred update without swapping the binary. Unlike
/// [`run_upgrade`], this deliberately does NOT check for a running peer — the
/// whole point is to arm while a `cartog serve`/`watch` (e.g. the MCP server)
/// holds the lock. The swap happens later via [`run_apply_pending`].
///
/// `target_override` (from `--to`) pins an explicit version — `/cartog-install`
/// passes the plugin's pin so arming can't overshoot it. When `None`, the
/// latest stable release is resolved from GitHub.
fn run_arm(target_override: Option<&str>, quiet: bool, json: bool) -> i32 {
    if effective_install_source() == "cargo" {
        emit_upgrade_message(
            quiet,
            json,
            "cargo",
            "cartog was installed via cargo. Run `cargo install cartog --force` to upgrade.",
        );
        return exit::CARGO_INSTALL_REFUSED;
    }

    let latest = match target_override {
        Some(v) => {
            // An explicit pin must be a bare MAJOR.MINOR.PATCH — reject garbage
            // up front rather than arming an unappliable target.
            if !is_stable_semver(v) {
                emit_upgrade_message(
                    quiet,
                    json,
                    "invalid-version",
                    &format!("`--to {v}` is not a MAJOR.MINOR.PATCH version"),
                );
                return exit::NETWORK_OR_PARSE_ERROR;
            }
            v.to_string()
        }
        None => {
            let api_url = github_latest_url();
            match fetch_latest_version(&api_url) {
                Ok(v) => v,
                Err(e) => {
                    emit_upgrade_message(quiet, json, "fetch-failed", &e.to_string());
                    return exit::NETWORK_OR_PARSE_ERROR;
                }
            }
        }
    };
    let current = env!("CARGO_PKG_VERSION");

    // Already current (or ahead): nothing to arm. Clear any stale intent.
    if compare_stable_versions(current, &latest) != std::cmp::Ordering::Less {
        if let Some(state_path) = state::default_state_file() {
            let mut state = State::load_from(&state_path);
            if state.pending_update.take().is_some() {
                let _ = state.save_to(&state_path);
            }
        }
        if !quiet {
            if json {
                let payload = serde_json::json!({
                    "status": "up-to-date",
                    "current": current,
                    "latest": latest,
                });
                println!("{payload}");
            } else {
                println!("cartog: already up to date ({current})");
            }
        }
        return exit::SUCCESS;
    }

    let Some(state_path) = state::default_state_file() else {
        emit_upgrade_message(
            quiet,
            json,
            "filesystem-failed",
            "cannot resolve state file to record the deferred update",
        );
        return exit::DISK_OR_PERMISSION_FAILED;
    };
    let mut state = State::load_from(&state_path);
    state.pending_update = Some(crate::state::PendingUpdate {
        target_version: latest.clone(),
        armed_from: Some(current.to_string()),
        armed_at: Some(rfc3339_now()),
    });
    state.last_known_latest = Some(latest.clone());
    state.last_known_outdated = true;
    state.last_update_check = Some(rfc3339_now());
    if let Err(e) = state.save_to(&state_path) {
        emit_upgrade_message(
            quiet,
            json,
            "filesystem-failed",
            &format!("failed to persist deferred update intent: {e}"),
        );
        return exit::DISK_OR_PERMISSION_FAILED;
    }

    if !quiet {
        if json {
            let payload = serde_json::json!({
                "status": "armed",
                "current": current,
                "target": latest,
                "apply": "session-end-or-restart",
            });
            println!("{payload}");
        } else {
            println!(
                "cartog: armed update {current} -> {latest}; it will be applied when the \
                 current session ends (or run `cartog self update` from a terminal with no \
                 cartog serve/watch running)."
            );
        }
    }
    exit::SUCCESS
}

/// Drive `--apply-pending`: apply a previously-armed deferred update once no
/// peer holds the lock. Owns read-state → re-check-peer → swap → clear-state so
/// the hook stays thin and the race logic is unit-tested in Rust.
///
/// `at_startup` excludes this project's own serve/watch slots from the
/// peer-wait: at SessionStart the session's own `serve --watch` holds the lock
/// for the whole session, so waiting on it can never clear. The atomic same-FS
/// swap is safe under a live same-project peer (it keeps its fd on the old
/// inode); other projects' peers still block. No-op on Windows.
fn run_apply_pending(at_startup: bool, db_path: &Path, quiet: bool, json: bool) -> i32 {
    let Some(state_path) = state::default_state_file() else {
        // No state file means nothing could have been armed.
        return exit::SUCCESS;
    };
    let mut state = State::load_from(&state_path);
    let Some(pending) = state.pending_update.clone() else {
        return exit::SUCCESS; // nothing armed — clean no-op
    };
    let target = pending.target_version;
    let current = env!("CARGO_PKG_VERSION");

    // A binary reinstalled via cargo after arming can never be swapped here.
    if effective_install_source() == "cargo" {
        state.pending_update = None;
        let _ = state.save_to(&state_path);
        emit_upgrade_message(
            quiet,
            json,
            "cargo",
            "cartog was installed via cargo. Run `cargo install cartog --force` to upgrade.",
        );
        return exit::CARGO_INSTALL_REFUSED;
    }

    // Idempotency / staleness: already at or past the armed target.
    if decide_apply(current, &target) == ApplyDecision::Skip {
        state.pending_update = None;
        let _ = state.save_to(&state_path);
        if !quiet && !json {
            println!("cartog: already at {current}; cleared stale deferred update ({target}).");
        }
        return exit::SUCCESS;
    }

    // Re-check for a live peer, waiting briefly for normal session-teardown
    // lag. If a peer is still live after the budget, keep the intent armed and
    // retry on the next boundary. At startup, ignore this project's own peer
    // (see fn doc) — except on Windows, where a peer's running .exe can't be
    // renamed, so the full wait still applies.
    let self_peer_slots = if at_startup && !cfg!(windows) {
        current_project_peer_slots(db_path)
    } else {
        Vec::new()
    };
    if let Some(dir) = state::default_state_dir() {
        // Size the wait to what can actually block it: an unclearable foreign
        // lock fast-fails instead of running a session hook out of time (#154).
        // `self_peer_slots` is passed to both the budget and the wait so the two
        // agree on which locks count as blockers.
        let own_slots = current_project_peer_slots(db_path);
        let budget = effective_peer_wait(&own_slots, &active_peer_slots(&dir), &self_peer_slots);
        if let Err(peer) = wait_for_no_peer_excluding(&dir, budget, &self_peer_slots) {
            let foreign = foreign_peer_process_count(&dir, &own_slots, &self_peer_slots);
            emit_upgrade_message(
                quiet,
                json,
                "peer-running",
                &peer_running_message(&peer.slot, peer.pid, foreign),
            );
            return exit::PEER_RUNNING;
        }
    }

    // Serialize the swap+clear against another `--apply-pending` (e.g. two
    // Claude Code windows closing at once). Both would otherwise pass the peer
    // wait, download in parallel, and interleave atomic swaps — muddying the
    // `<bin>.old` rollback target. Held → another apply is in flight; treat as a
    // benign no-op (it will land the swap and clear the intent). Held by RAII
    // until this function returns. An Io failure here is non-fatal: fall through
    // and let the swap proceed unserialized rather than block the update.
    let _apply_lock = match state::default_state_dir() {
        Some(dir) => match cartog_process_lock::ProcessLock::acquire(&dir, APPLY_LOCK_SLOT) {
            Ok(lock) => Some(lock),
            Err(cartog_process_lock::AcquireError::Held(_)) => {
                if !quiet && !json {
                    println!("cartog: another deferred update is already in progress; skipping.");
                }
                return exit::SUCCESS;
            }
            Err(cartog_process_lock::AcquireError::Io(e)) => {
                tracing::warn!(error = %e, "apply-update lock unavailable; proceeding unserialized");
                None
            }
        },
        None => None,
    };

    let err = match perform_upgrade(current, &target, quiet, json) {
        Ok(()) => {
            // perform_upgrade already refreshed last_known_*; reload and clear
            // the pending intent.
            let mut state = State::load_from(&state_path);
            state.pending_update = None;
            let _ = state.save_to(&state_path);
            write_last_update_breadcrumb(&target);
            return exit::SUCCESS;
        }
        Err(e) => e,
    };

    // Clear the intent for deterministic failures (checksum, smoke) so we don't
    // retry the identical tarball every session; keep it for transient ones
    // (network, disk/permission) so the next boundary retries.
    if intent_disposition(&err) == IntentDisposition::Clear {
        let mut state = State::load_from(&state_path);
        state.pending_update = None;
        let _ = state.save_to(&state_path);
    }

    match err {
        UpgradeError::Network(msg) => {
            emit_upgrade_message(quiet, json, "fetch-failed", &msg);
            exit::NETWORK_OR_PARSE_ERROR
        }
        UpgradeError::Checksum(msg) => {
            emit_upgrade_message(quiet, json, "checksum-failed", &msg);
            exit::CHECKSUM_FAILED
        }
        // Smoke-test failure is deterministic for this target. Distinct exit
        // (7) so the hook treats it as terminal; the message names a manual
        // next step instead of implying a silent retry will fix it.
        UpgradeError::Smoke(msg) => {
            emit_upgrade_message(
                quiet,
                json,
                "smoke-failed",
                &format!(
                    "{msg}. This target failed verification and will not be retried \
                     automatically; run `cartog self update` from a terminal or reinstall \
                     via /cartog-install."
                ),
            );
            exit::SMOKE_TEST_FAILED
        }
        UpgradeError::Filesystem(msg) => {
            emit_upgrade_message(quiet, json, "filesystem-failed", &msg);
            exit::DISK_OR_PERMISSION_FAILED
        }
    }
}

/// Drop a one-line breadcrumb the SessionStart hook surfaces once to confirm a
/// completed deferred update. Best-effort: a failure to write it never affects
/// the (already successful) upgrade. Sibling of the hook's `last-error` file.
pub(crate) fn write_last_update_breadcrumb(target: &str) {
    let Some(path) = last_update_breadcrumb_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("cartog updated to {target}.\n"));
}

/// Path of the `last-update` breadcrumb, in the exact directory the hooks read
/// from. Mirrors their precedence: `$CARTOG_LOG_DIR` used verbatim (it already
/// points at the log dir), else `$XDG_CACHE_HOME/cartog`, else `~/.cache/cartog`.
/// Returns `None` if none resolves. Keeping this in lockstep with the hooks
/// matters: a divergence would write the breadcrumb where the hook never looks,
/// silently dropping the "cartog updated to X" confirmation.
pub(crate) fn last_update_breadcrumb_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CARTOG_LOG_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("last-update"));
        }
    }
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home).join(".cache")
        }
    };
    Some(base.join("cartog").join("last-update"))
}
