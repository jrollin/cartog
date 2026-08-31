//! Auto-check predicate and helpers for the daily background update probe.
//!
//! Two responsibilities:
//! 1. The pure `should_check` predicate decides whether to fire the
//!    background probe at all (env, TTY, command kind, interval).
//! 2. `run_check_once` / `spawn_check` perform the actual probe:
//!    fetch the latest release tag and update the on-disk state file.
//!
//! Both halves take their inputs as parameters so unit tests can exercise
//! them without touching real env / FS / network.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::semver::{compare_stable_versions, parse_release_tag};
use crate::state::State;
use crate::time_fmt::{parse_rfc3339_secs, rfc3339_now};

/// Kind of command currently running. Long-lived commands (`serve`,
/// `watch`) deliberately skip the auto-check — they are typically started
/// by editor integrations, run for hours, and the user never sees a hint
/// printed at the *start* anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Quick,
    LongLived,
}

/// Resolved interval policy. Mirrors `CARTOG_UPDATE_CHECK={never,daily,always}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    Never,
    Daily,
    Always,
}

/// Inputs to the [`should_check`] predicate. Bundled into a struct so the
/// pure decision is trivially testable — tests construct an input by hand;
/// the binary fills it from real env / FS state.
#[derive(Debug, Clone)]
pub struct ShouldCheckInput<'a> {
    pub command_kind: CommandKind,
    pub stdout_is_tty: bool,
    /// `CARTOG_NO_UPDATE_CHECK=1` (any non-empty value) disables the check.
    pub disabled_env: bool,
    pub mode: CheckMode,
    /// RFC3339 timestamp of the last check, or `None` if never.
    pub last_check: Option<&'a str>,
    pub now: SystemTime,
}

const DAILY_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Pure decision: should the post-command epilogue spawn an update-check
/// thread? Returns `true` iff every gating signal allows it.
pub fn should_check(input: &ShouldCheckInput<'_>) -> bool {
    if input.disabled_env {
        return false;
    }
    if matches!(input.mode, CheckMode::Never) {
        return false;
    }
    if matches!(input.command_kind, CommandKind::LongLived) {
        return false;
    }
    if !input.stdout_is_tty {
        return false;
    }
    if matches!(input.mode, CheckMode::Always) {
        return true;
    }
    match input.last_check.and_then(parse_rfc3339_secs) {
        Some(last_secs) => match input.now.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(now_secs) => {
                now_secs.saturating_sub(Duration::from_secs(last_secs)) >= DAILY_INTERVAL
            }
            Err(_) => true,
        },
        None => true,
    }
}

/// Parse `CARTOG_UPDATE_CHECK` into a [`CheckMode`]. Unknown / unset values
/// fall back to `Daily` so users never accidentally disable themselves.
pub fn parse_check_mode(raw: Option<&str>) -> CheckMode {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("never") => CheckMode::Never,
        Some("always") => CheckMode::Always,
        _ => CheckMode::Daily,
    }
}

/// `CARTOG_NO_UPDATE_CHECK` kill switch: any non-empty value disables.
pub fn parse_disabled_env(raw: Option<&str>) -> bool {
    matches!(raw, Some(v) if !v.is_empty())
}

// ── post-command epilogue glue ────────────────────────────────────────

/// Inputs needed to decide whether to fire the daily auto-check at the
/// end of `main`. All ambient signals (env vars, TTY, state file path)
/// are passed in explicitly so the binary can read them once and tests
/// can exercise the glue without fighting global state.
#[derive(Debug)]
pub struct MaybeSpawnInput<'a> {
    pub command_kind: CommandKind,
    pub stdout_is_tty: bool,
    /// Raw value of `CARTOG_NO_UPDATE_CHECK` (any non-empty value disables).
    pub disabled_env: Option<&'a str>,
    /// Raw value of `CARTOG_UPDATE_CHECK` (`never`/`daily`/`always`).
    pub mode_env: Option<&'a str>,
    /// Resolved `state.toml` path, or `None` if no state directory could
    /// be determined (sandboxed env). Without a state path the worker has
    /// nowhere to persist the result, so the spawn is suppressed —
    /// otherwise every invocation would re-fire the check (no `last_check`
    /// record to gate on) and hammer the GitHub API.
    pub state_path: Option<&'a std::path::Path>,
    /// API URL for the latest-release endpoint. Tests inject a localhost
    /// stub here; production passes the real GitHub URL.
    pub api_url: &'a str,
    pub current_version: &'a str,
    /// Wall-clock time used for the 24-hour interval gate only. The
    /// timestamp eventually written into `state.toml` by the worker is
    /// captured at write time, not from this field.
    pub now: SystemTime,
}

/// Post-command epilogue: consult the gating predicate and, if all signals
/// agree, spawn the detached background check. Returns `true` iff a
/// thread was spawned.
///
/// Cheap gates (env, mode, command kind, TTY) short-circuit before the
/// `state.toml` read so quick commands on non-TTY stdout pay zero I/O.
pub fn maybe_spawn(input: MaybeSpawnInput<'_>) -> bool {
    let disabled = parse_disabled_env(input.disabled_env);
    let mode = parse_check_mode(input.mode_env);
    if disabled
        || matches!(mode, CheckMode::Never)
        || matches!(input.command_kind, CommandKind::LongLived)
        || !input.stdout_is_tty
    {
        return false;
    }
    let Some(state_path) = input.state_path else {
        return false;
    };
    let last_check = State::load_from(state_path).last_update_check;
    let predicate_input = ShouldCheckInput {
        command_kind: input.command_kind,
        stdout_is_tty: input.stdout_is_tty,
        disabled_env: disabled,
        mode,
        last_check: last_check.as_deref(),
        now: input.now,
    };
    if !should_check(&predicate_input) {
        return false;
    }
    spawn_check(
        input.api_url.to_string(),
        Some(state_path.to_path_buf()),
        input.current_version.to_string(),
    );
    true
}

// ── background fetch + state write ────────────────────────────────────

/// Spawn a detached background thread that fetches the latest release tag
/// and writes the result + timestamp to the state file. Returns
/// immediately; the caller never waits.
///
/// This intentionally swallows all failures: a network blip or transient
/// permission error must never disturb the user's actual command. The
/// state file simply won't be updated; the next check (24h+ later, or on
/// the next invocation in `Always` mode) will retry.
///
/// Note: the design contemplates a "best-effort 100 ms join hint" so
/// fast networks get state persisted before process exit, with detach as
/// fallback. We currently pure-detach: on a localhost / fast LAN the
/// network call usually finishes before `main` returns, but a slow probe
/// against `api.github.com` can be killed by process exit. Acceptable
/// trade-off — the next invocation reruns the check.
pub fn spawn_check(api_url: String, state_path: Option<PathBuf>, current_version: String) {
    std::thread::spawn(move || {
        if let Err(e) = run_check_once(&api_url, state_path.as_deref(), &current_version) {
            tracing::debug!(error = %e, "background update check failed");
        }
    });
}

/// Synchronous body of the background check. Factored out so tests can
/// drive it without spawning a thread.
pub fn run_check_once(
    api_url: &str,
    state_path: Option<&std::path::Path>,
    current_version: &str,
) -> Result<(), CheckOnceError> {
    let latest = fetch_latest_tag(api_url)?;
    let outdated = compare_stable_versions(current_version, &latest) == std::cmp::Ordering::Less;
    if let Some(path) = state_path {
        let mut state = State::load_from(path);
        state.last_update_check = Some(rfc3339_now());
        state.last_known_latest = Some(latest);
        state.last_known_outdated = outdated;
        state
            .save_to(path)
            .map_err(|e| CheckOnceError::StateSave(e.to_string()))?;
    }
    Ok(())
}

/// Categorised error surface for [`run_check_once`]. Tests assert on the
/// variants; production code only ever logs the message.
#[derive(Debug)]
pub enum CheckOnceError {
    Network(String),
    Parse(String),
    StateSave(String),
}

impl std::fmt::Display for CheckOnceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckOnceError::Network(m) => write!(f, "network error: {m}"),
            CheckOnceError::Parse(m) => write!(f, "parse error: {m}"),
            CheckOnceError::StateSave(m) => write!(f, "state save failed: {m}"),
        }
    }
}

impl std::error::Error for CheckOnceError {}

/// Fetch GitHub's `releases/latest`, return the bare semver tag.
///
/// Mirrors the strict-stable-only contract from `commands::self_cmd`: a
/// prerelease-shaped tag (`-alpha`, `-rc`, `-nightly`, …) is treated as
/// "no eligible release" and reported as a parse error so the auto-check
/// thread doesn't write garbage into the state file.
///
/// Duplicated from `commands::self_cmd` because the two callers want
/// different error shapes (`CheckOnceError` here vs `anyhow::Error`
/// there). When a third caller appears, extract to a shared helper.
fn fetch_latest_tag(url: &str) -> Result<String, CheckOnceError> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("cartog/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| CheckOnceError::Network(e.to_string()))?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|e| CheckOnceError::Network(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CheckOnceError::Network(format!("HTTP {status}")));
    }
    let body = response
        .text()
        .map_err(|e| CheckOnceError::Network(e.to_string()))?;
    parse_release_tag(&body)
        .ok_or_else(|| CheckOnceError::Parse("no stable release tag in response".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch_plus(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn input_default<'a>(now_secs: u64) -> ShouldCheckInput<'a> {
        ShouldCheckInput {
            command_kind: CommandKind::Quick,
            stdout_is_tty: true,
            disabled_env: false,
            mode: CheckMode::Daily,
            last_check: None,
            now: epoch_plus(now_secs),
        }
    }

    #[test]
    fn should_check_first_run_with_tty_returns_true() {
        let input = input_default(1_000_000);
        assert!(should_check(&input));
    }

    #[test]
    fn should_check_disabled_env_blocks() {
        let mut input = input_default(1_000_000);
        input.disabled_env = true;
        assert!(!should_check(&input));
    }

    #[test]
    fn should_check_never_mode_blocks() {
        let mut input = input_default(1_000_000);
        input.mode = CheckMode::Never;
        assert!(!should_check(&input));
    }

    #[test]
    fn should_check_long_lived_commands_blocked() {
        let mut input = input_default(1_000_000);
        input.command_kind = CommandKind::LongLived;
        assert!(!should_check(&input));
    }

    #[test]
    fn should_check_non_tty_blocked() {
        let mut input = input_default(1_000_000);
        input.stdout_is_tty = false;
        assert!(!should_check(&input));
    }

    #[test]
    fn should_check_always_mode_overrides_interval() {
        let mut input = input_default(1_000_000);
        input.mode = CheckMode::Always;
        // Last check was just a moment ago; daily mode would refuse, but
        // always mode does not consult the interval.
        input.last_check = Some("1970-01-12T13:46:39Z");
        assert!(should_check(&input));
    }

    #[test]
    fn should_check_daily_within_24h_blocked() {
        // last_check = 2024-01-01T00:00:00Z; now = 2024-01-01T12:00:00Z.
        let last_secs: u64 = 1_704_067_200;
        let now_secs = last_secs + 12 * 3600;
        let mut input = input_default(now_secs);
        input.last_check = Some("2024-01-01T00:00:00Z");
        assert!(!should_check(&input));
    }

    #[test]
    fn should_check_daily_after_24h_allowed() {
        let last_secs: u64 = 1_704_067_200;
        let now_secs = last_secs + 25 * 3600;
        let mut input = input_default(now_secs);
        input.last_check = Some("2024-01-01T00:00:00Z");
        assert!(should_check(&input));
    }

    #[test]
    fn should_check_daily_unparseable_last_treated_as_never() {
        let mut input = input_default(1_000_000);
        input.last_check = Some("not a real timestamp");
        assert!(should_check(&input));
    }

    #[test]
    fn should_check_long_lived_beats_always_mode() {
        // Long-lived gating runs before mode==Always so `serve` never
        // triggers a check, even with `CARTOG_UPDATE_CHECK=always`.
        let mut input = input_default(1_000_000);
        input.command_kind = CommandKind::LongLived;
        input.mode = CheckMode::Always;
        assert!(!should_check(&input));
    }

    #[test]
    fn should_check_disabled_env_beats_always_mode() {
        let mut input = input_default(1_000_000);
        input.disabled_env = true;
        input.mode = CheckMode::Always;
        assert!(!should_check(&input));
    }

    // ── parse_check_mode ──

    #[test]
    fn parse_check_mode_known_values() {
        assert_eq!(parse_check_mode(Some("never")), CheckMode::Never);
        assert_eq!(parse_check_mode(Some("NEVER")), CheckMode::Never);
        assert_eq!(parse_check_mode(Some("always")), CheckMode::Always);
        assert_eq!(parse_check_mode(Some("Daily")), CheckMode::Daily);
        assert_eq!(parse_check_mode(Some("  always ")), CheckMode::Always);
    }

    #[test]
    fn parse_check_mode_unknown_falls_back_to_daily() {
        assert_eq!(parse_check_mode(None), CheckMode::Daily);
        assert_eq!(parse_check_mode(Some("")), CheckMode::Daily);
        assert_eq!(parse_check_mode(Some("something-else")), CheckMode::Daily);
    }

    // ── parse_disabled_env ──

    #[test]
    fn parse_disabled_env_truthy() {
        assert!(parse_disabled_env(Some("1")));
        assert!(parse_disabled_env(Some("yes")));
        assert!(parse_disabled_env(Some("0"))); // any non-empty value disables
    }

    #[test]
    fn parse_disabled_env_empty_or_unset() {
        assert!(!parse_disabled_env(None));
        assert!(!parse_disabled_env(Some("")));
    }

    // parse_rfc3339_secs tests live in `time_fmt::tests`.
}
