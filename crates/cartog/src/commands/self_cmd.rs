//! Implementations for the `cartog self` subcommand group.
//!
//! Pure logic is factored into helpers (`resolve_install_source`,
//! `VersionInfo`) that take their inputs as arguments so integration tests
//! can drive them without touching the real environment, filesystem, or
//! network. The thin `cmd_self_*` wrappers gather the real-world inputs and
//! delegate to the pure helpers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

use crate::state::{self, State};
use cartog::time_fmt::rfc3339_now;

/// Compile-time channel as written by `build.rs`. One of:
/// - `"release-tarball"` — built by the GitHub release workflow.
/// - `"dev"` — every other build (local `cargo build`, `cargo install`, …).
const COMPILE_TIME_INSTALL_SOURCE: &str = env!("CARTOG_INSTALL_SOURCE");

/// Compile-time target triple, e.g. `aarch64-apple-darwin`.
const TARGET_TRIPLE: &str = env!("CARTOG_TARGET_TRIPLE");

/// Test seam: when set to `release-tarball`, `cargo`, or `dev`, the install
/// source is forced to that value, bypassing the compile-time + path
/// heuristics. Lets the integration suite drive the cargo-refusal branch
/// without producing a real cargo install. Read only by `effective_install_source`.
const TEST_INSTALL_SOURCE_ENV: &str = "CARTOG_TEST_INSTALL_SOURCE";

/// Resolve the install source, honoring the test override env var if set.
fn effective_install_source() -> &'static str {
    if let Ok(forced) = std::env::var(TEST_INSTALL_SOURCE_ENV) {
        match forced.as_str() {
            "release-tarball" => return "release-tarball",
            "cargo" => return "cargo",
            "dev" => return "dev",
            _ => {} // ignore garbage; fall through to real detection
        }
    }
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let binary_path = std::env::current_exe().ok();
    resolve_install_source(
        COMPILE_TIME_INSTALL_SOURCE,
        binary_path.as_deref(),
        cargo_home.as_deref(),
    )
}

/// Resolve the *effective* install source.
///
/// `build.rs` only distinguishes `release-tarball` from `dev` because it has
/// no idea where the resulting binary will be installed. The cargo case is
/// detected at runtime: if the compile-time channel is `dev` AND the running
/// binary lives under a `.cargo/bin` directory, the user almost certainly
/// ran `cargo install cartog`.
///
/// `binary_path` is taken as an argument so tests can drive every branch.
pub(crate) fn resolve_install_source(
    compile_time: &str,
    binary_path: Option<&Path>,
    cargo_home: Option<&Path>,
) -> &'static str {
    if compile_time == "release-tarball" {
        return "release-tarball";
    }
    if let Some(bin) = binary_path {
        if looks_like_cargo_install(bin, cargo_home) {
            return "cargo";
        }
    }
    "dev"
}

fn looks_like_cargo_install(binary_path: &Path, cargo_home: Option<&Path>) -> bool {
    // Honor an explicit CARGO_HOME first.
    if let Some(home) = cargo_home {
        let bin_dir = home.join("bin");
        if binary_path.starts_with(&bin_dir) {
            return true;
        }
    }
    // Catches `~/.cargo/bin` even when CARGO_HOME is unset (common on macOS).
    let mut prev: Option<&std::ffi::OsStr> = None;
    for component in binary_path.components() {
        let cur = component.as_os_str();
        if prev == Some(std::ffi::OsStr::new(".cargo")) && cur == std::ffi::OsStr::new("bin") {
            return true;
        }
        prev = Some(cur);
    }
    false
}

/// Snapshot of "what version of cartog am I, and how did I get here?".
#[derive(Debug, Clone, Serialize)]
pub(crate) struct VersionInfo {
    pub version: String,
    pub target: String,
    pub install_source: String,
    /// RFC3339 timestamp of the last successful update check, or `None`.
    /// Serialised as JSON `null` when absent.
    pub last_update_check: Option<String>,
}

impl VersionInfo {
    pub(crate) fn build(state: &State) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            target: TARGET_TRIPLE.to_string(),
            install_source: effective_install_source().to_string(),
            last_update_check: state.last_update_check.clone(),
        }
    }

    /// Render the human-readable form printed when `--json` is not set.
    pub(crate) fn render_human(&self) -> String {
        let last = self.last_update_check.as_deref().unwrap_or("never");
        format!(
            "cartog {version}\n  target:           {target}\n  install source:   {source}\n  last update check: {last}\n",
            version = self.version,
            target = self.target,
            source = self.install_source,
            last = last,
        )
    }
}

/// `cartog self version` entry point. Reads the on-disk state file, then
/// prints either a human-readable summary or a JSON object.
pub fn cmd_self_version(json: bool) -> Result<()> {
    let state = match state::default_state_file() {
        Some(p) => State::load_from(&p),
        None => State::default(),
    };
    let info = VersionInfo::build(&state);
    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        print!("{}", info.render_human());
    }
    Ok(())
}

/// `cartog self update [--check] [--quiet] [--json]` entry point.
///
/// In `--check` mode this is read-only (see [`run_check`]).
///
/// In upgrade mode the flow is:
/// 1. Refuse for cargo-installed binaries (exit 3) — direct user to
///    `cargo install cartog --force`.
/// 2. Refuse if a peer `cartog serve`/`watch` is still running (exit 6).
/// 3. Fetch the latest stable tag. Already up to date → exit 0.
/// 4. Download the platform tarball/zip and `SHA256SUMS`, verify the
///    checksum (exit 4 on mismatch), atomically swap the binary in
///    place, preserve `<bin>.old`, smoke-test the new binary
///    (exit 5 on failure → restore `.old`).
pub fn cmd_self_update(check: bool, quiet: bool, json: bool) -> Result<()> {
    if check {
        let exit_code = run_check(quiet, json);
        std::process::exit(exit_code);
    }
    let exit_code = run_upgrade(quiet, json);
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

const DEFAULT_GITHUB_LATEST_URL: &str =
    "https://api.github.com/repos/jrollin/cartog/releases/latest";

/// Resolve the GitHub latest-release endpoint. Honors `CARTOG_GITHUB_API_URL`
/// for tests and locked-down environments; falls back to the public default.
fn github_latest_url() -> String {
    std::env::var("CARTOG_GITHUB_API_URL").unwrap_or_else(|_| DEFAULT_GITHUB_LATEST_URL.to_string())
}

/// Fetch the latest stable release tag from GitHub and return it as a bare
/// `MAJOR.MINOR.PATCH` string. Errors out on transport failure, non-2xx
/// status, malformed JSON, or a tag carrying a prerelease suffix.
fn fetch_latest_version(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("cartog/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        // Pin REST API version per GitHub docs — guards against silent schema drift.
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("GitHub API returned status {status}");
    }
    let body = response.text()?;
    parse_release_tag(&body).ok_or_else(|| {
        anyhow::anyhow!("could not extract a stable release tag from GitHub response")
    })
}

/// Pull `tag_name` out of the GitHub release JSON, strip a leading `v`, and
/// return `None` for any prerelease-shaped tag. SemVer prerelease metadata
/// is delimited by `-`, so any hyphen in the version (e.g. `0.15.0-rc.1`,
/// `0.15.0-alpha`, `0.15.0-nightly.42`) disqualifies the tag.
fn parse_release_tag(json: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = parsed.get("tag_name")?.as_str()?;
    let trimmed = tag.strip_prefix('v').unwrap_or(tag);
    if trimmed.contains('-') {
        return None;
    }
    if !is_stable_semver(trimmed) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Quick guard: accept exactly three dot-separated non-empty numeric parts.
fn is_stable_semver(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// JSON-friendly view of an update check. A single shape covers both the
/// success and failure cases so consumers don't have to switch on schema:
/// on failure, `latest` and `outdated` are `null` and `error` is set.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct CheckOutcome {
    current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outdated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl CheckOutcome {
    fn ok(current: &str, latest: &str) -> Self {
        let outdated = compare_stable_versions(current, latest) == std::cmp::Ordering::Less;
        Self {
            current: current.to_string(),
            latest: Some(latest.to_string()),
            outdated: Some(outdated),
            error: None,
        }
    }

    fn failed(current: &str, error: &str) -> Self {
        Self {
            current: current.to_string(),
            latest: None,
            outdated: None,
            error: Some(error.to_string()),
        }
    }

    fn to_human(&self) -> String {
        match (&self.latest, self.outdated, &self.error) {
            (Some(latest), Some(true), _) => {
                format!(
                    "cartog: update available: {current} -> {latest}",
                    current = self.current,
                    latest = latest,
                )
            }
            (_, Some(false), _) => format!("cartog: up to date ({})", self.current),
            (_, _, Some(err)) => format!("cartog: update check failed: {err}"),
            // Unreachable in practice — every outcome is built via `ok` or `failed`.
            _ => "cartog: update check produced an empty outcome".to_string(),
        }
    }
}

/// Lexicographic compare on `(major, minor, patch)`.
///
/// Both inputs are expected to be stable `MAJOR.MINOR.PATCH` triples — any
/// non-numeric component is treated as `0`, so the function never panics on
/// weird input but degrades gracefully.
fn compare_stable_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> [u64; 3] {
        let mut parts = s.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        [
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        ]
    };
    parse(a).cmp(&parse(b))
}

// ── upgrade flow ──────────────────────────────────────────────────────

/// Exit codes for the upgrade path. Mirrors the contract documented on
/// [`cmd_self_update`].
mod exit {
    pub const SUCCESS: i32 = 0;
    pub const NETWORK_OR_PARSE_ERROR: i32 = 2;
    pub const CARGO_INSTALL_REFUSED: i32 = 3;
    pub const CHECKSUM_FAILED: i32 = 4;
    pub const DISK_OR_PERMISSION_FAILED: i32 = 5;
    pub const PEER_RUNNING: i32 = 6;
}

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
        Err(UpgradeError::Filesystem(msg)) => {
            emit_upgrade_message(quiet, json, "filesystem-failed", &msg);
            exit::DISK_OR_PERMISSION_FAILED
        }
    }
}

/// Categorised error so [`run_upgrade`] can map to the right exit code.
enum UpgradeError {
    Network(String),
    Checksum(String),
    Filesystem(String),
}

fn perform_upgrade(
    current: &str,
    latest: &str,
    quiet: bool,
    json: bool,
) -> std::result::Result<(), UpgradeError> {
    let archive_name = archive_name_for(TARGET_TRIPLE);
    let download_base = github_download_base(latest);
    let archive_url = format!("{download_base}/{archive_name}");
    let sums_url = format!("{download_base}/SHA256SUMS");

    if !quiet && !json {
        println!("cartog: downloading {archive_name}");
    }

    let archive_bytes = http_get_bytes(&archive_url)
        .map_err(|e| UpgradeError::Network(format!("failed to download {archive_url}: {e}")))?;
    let sums_text = http_get_text(&sums_url)
        .map_err(|e| UpgradeError::Network(format!("failed to download {sums_url}: {e}")))?;

    let expected = parse_sha256sums(&sums_text, &archive_name).ok_or_else(|| {
        UpgradeError::Checksum(format!(
            "SHA256SUMS does not contain an entry for {archive_name}"
        ))
    })?;
    let actual = compute_sha256(&archive_bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(UpgradeError::Checksum(format!(
            "checksum mismatch for {archive_name}: expected {expected}, got {actual}"
        )));
    }

    // Stage in install_dir (same FS) — default $TMPDIR could trigger EXDEV on rename.
    let current_bin = std::env::current_exe()
        .map_err(|e| UpgradeError::Filesystem(format!("cannot resolve current exe: {e}")))?;
    let install_dir = current_bin.parent().ok_or_else(|| {
        UpgradeError::Filesystem(format!(
            "current exe {} has no parent directory",
            current_bin.display(),
        ))
    })?;
    // SIGKILL/SIGINT during a prior upgrade can orphan staging dirs (TempDir
    // Drop never runs). Sweep entries older than 1h before creating a new one.
    sweep_stale_staging_dirs(install_dir);
    let staging = tempfile::Builder::new()
        .prefix(".cartog-update-")
        .tempdir_in(install_dir)
        .map_err(|e| {
            UpgradeError::Filesystem(format!(
                "failed to create staging dir under {}: {e}",
                install_dir.display(),
            ))
        })?;
    let archive_path = staging.path().join(&archive_name);
    std::fs::write(&archive_path, &archive_bytes)
        .map_err(|e| UpgradeError::Filesystem(format!("failed to stage archive: {e}")))?;
    self_update::Extract::from_source(&archive_path)
        .extract_file(staging.path(), bin_name_in_archive())
        .map_err(|e| UpgradeError::Filesystem(format!("failed to extract binary: {e}")))?;
    let new_bin = staging.path().join(bin_name_in_archive());

    let backup_path = backup_path_for(&current_bin);

    self_update::Move::from_source(&new_bin)
        .replace_using_temp(&backup_path)
        .to_dest(&current_bin)
        .map_err(|e| UpgradeError::Filesystem(format!("atomic swap failed: {e}")))?;

    if let Err(smoke_err) = smoke_test(&current_bin) {
        match std::fs::rename(&backup_path, &current_bin) {
            Ok(()) => {
                return Err(UpgradeError::Filesystem(format!(
                    "new binary failed smoke test ({smoke_err}); previous binary restored"
                )));
            }
            Err(restore_err) => {
                // The new binary is broken AND we could not restore the old one.
                // The user must intervene manually. Be explicit about both failures.
                return Err(UpgradeError::Filesystem(format!(
                    "new binary failed smoke test ({smoke_err}) AND restore of {} -> {} \
                     also failed ({restore_err}); manually rename the .old back",
                    backup_path.display(),
                    current_bin.display(),
                )));
            }
        }
    }

    if let Some(state_path) = state::default_state_file() {
        let mut state = State::load_from(&state_path);
        state.last_known_latest = Some(latest.to_string());
        state.last_known_outdated = false;
        state.last_update_check = Some(rfc3339_now());
        if let Err(e) = state.save_to(&state_path) {
            tracing::warn!(
                error = %e,
                path = %state_path.display(),
                "failed to persist update state",
            );
        }
    }

    if !quiet {
        if json {
            let payload = serde_json::json!({
                "status": "updated",
                "current": current,
                "latest": latest,
                "backup": backup_path.to_string_lossy(),
            });
            println!("{payload}");
        } else {
            println!(
                "cartog: updated {current} -> {latest} (previous binary saved at {})",
                backup_path.display()
            );
        }
    }
    Ok(())
}

/// Emit a one-line status message in the right shape for the user.
fn emit_upgrade_message(quiet: bool, json: bool, status: &str, message: &str) {
    if quiet {
        return;
    }
    if json {
        let payload = serde_json::json!({
            "status": status,
            "message": message,
        });
        println!("{payload}");
    } else {
        eprintln!("cartog: {message}");
    }
}

const DEFAULT_GITHUB_DOWNLOAD_BASE: &str = "https://github.com/jrollin/cartog/releases/download";

/// Resolve the per-version download base URL. Honors
/// `CARTOG_GITHUB_DOWNLOAD_BASE` for tests and locked-down environments.
fn github_download_base(version: &str) -> String {
    let base = std::env::var("CARTOG_GITHUB_DOWNLOAD_BASE")
        .unwrap_or_else(|_| DEFAULT_GITHUB_DOWNLOAD_BASE.to_string());
    format!("{base}/v{version}")
}

/// Compose the platform-specific archive name. Mirrors the names produced
/// by the release workflow: tar.gz on unix, zip on windows. The version
/// is NOT embedded in the filename — it lives in the URL path
/// (`releases/download/v<version>/<archive>`), matching install.sh.
fn archive_name_for(target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("cartog-{target}.{ext}")
}

fn bin_name_in_archive() -> &'static str {
    if cfg!(windows) {
        "cartog.exe"
    } else {
        "cartog"
    }
}

fn backup_path_for(current: &Path) -> PathBuf {
    let mut name = current
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("cartog"));
    name.push(".old");
    current.with_file_name(name)
}

/// Find the hash for `archive_name` in a `sha256sum -c`-style file.
/// Lines look like `<hex>  <filename>` (two spaces or one + a `*`).
fn parse_sha256sums(text: &str, archive_name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Accept "<hash>  <name>", "<hash> *<name>", or "<hash> <name>".
        let mut parts = line.splitn(2, char::is_whitespace);
        let hash = parts.next()?.trim();
        let rest = parts.next()?.trim();
        let name = rest.strip_prefix('*').unwrap_or(rest).trim();
        if name == archive_name {
            return Some(hash.to_string());
        }
    }
    None
}

fn compute_sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("cartog/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let response = client.get(url).send()?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status} from {url}");
    }
    Ok(response.bytes()?.to_vec())
}

fn http_get_text(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("cartog/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = client.get(url).send()?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status} from {url}");
    }
    Ok(response.text()?)
}

/// Hard ceiling on how long we wait for the new binary's `--version` to
/// exit. A corrupt-but-not-crashing binary that hangs on startup would
/// otherwise hang `cartog self update` indefinitely with the swap
/// already done; the timeout lets the restore branch fire.
const SMOKE_TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Stale staging directory cutoff. A previous upgrade killed by SIGINT
/// or SIGKILL leaves `.cartog-update-<rand>/` behind; anything older
/// than this is safely abandoned.
const STAGING_SWEEP_AGE: Duration = Duration::from_secs(3600);

/// Best-effort sweep of `.cartog-update-*` directories left behind by a
/// previous interrupted upgrade. Errors are swallowed — this runs as a
/// hygiene step before the real upgrade, never the operation the user
/// asked for.
fn sweep_stale_staging_dirs(install_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(install_dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with(".cartog-update-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let modified_age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok());
        if let Some(age) = modified_age {
            if age >= STAGING_SWEEP_AGE {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

fn smoke_test(bin: &Path) -> Result<()> {
    let mut child = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let deadline = std::time::Instant::now() + SMOKE_TEST_TIMEOUT;
    loop {
        match child.try_wait()? {
            Some(status) => {
                if !status.success() {
                    anyhow::bail!("{bin:?} --version exited with {:?}", status.code());
                }
                return Ok(());
            }
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("{bin:?} --version did not exit within {SMOKE_TEST_TIMEOUT:?}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

// ── migrate-db ──────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PlannedMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Plan the moves needed to migrate `.cartog.db` (+ -wal, -shm, .pre-v*.bak)
/// at `root` into `.cartog/`. Empty vec when nothing to migrate. Errors when
/// any destination already exists: we never overwrite.
pub(crate) fn plan_migration(root: &Path) -> Result<Vec<PlannedMove>> {
    let legacy_db = root.join(cartog_db::LEGACY_DB_FILE);
    if !legacy_db.exists() {
        return Ok(Vec::new());
    }
    // fs::rename moves the link, not the target. Symlinks usually point to a
    // shared / network DB the user wants to keep where it is.
    let meta = std::fs::symlink_metadata(&legacy_db)
        .map_err(|e| anyhow::anyhow!("stat {}: {e}", legacy_db.display()))?;
    if meta.file_type().is_symlink() {
        anyhow::bail!(
            "refusing to migrate {}: it is a symlink. Resolve or update it manually.",
            legacy_db.display()
        );
    }
    let new_dir = root.join(cartog_db::DB_DIR);
    let new_db = new_dir.join(cartog_db::DB_FILENAME);

    let mut moves = Vec::new();
    let mut push_move = |from: PathBuf, to: PathBuf| -> Result<()> {
        if to.exists() {
            anyhow::bail!("refusing to migrate: {} already exists", to.display());
        }
        // Same symlink guard as legacy_db above: fs::rename moves the link, not the target.
        let meta = std::fs::symlink_metadata(&from)
            .map_err(|e| anyhow::anyhow!("stat {}: {e}", from.display()))?;
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "refusing to migrate {}: it is a symlink. Resolve or update it manually.",
                from.display()
            );
        }
        moves.push(PlannedMove { from, to });
        Ok(())
    };

    push_move(legacy_db.clone(), new_db.clone())?;

    for suffix in ["-wal", "-shm"] {
        let from = root.join(format!("{}{suffix}", cartog_db::LEGACY_DB_FILE));
        if from.exists() {
            let to = new_dir.join(format!("{}{suffix}", cartog_db::DB_FILENAME));
            push_move(from, to)?;
        }
    }

    let entries = std::fs::read_dir(root)
        .map_err(|e| anyhow::anyhow!("read_dir({}): {e}", root.display()))?;
    let prefix = format!("{}.pre-v", cartog_db::LEGACY_DB_FILE);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(suffix) = name_str.strip_prefix(&prefix) else {
            continue;
        };
        let from = entry.path();
        let to = new_dir.join(format!("{}.pre-v{suffix}", cartog_db::DB_FILENAME));
        push_move(from, to)?;
    }

    Ok(moves)
}

/// Test seam: when set, skips the peer-lock check. Only honored in `cfg(test)` builds.
#[cfg(test)]
const TEST_SKIP_PEER_LOCK_ENV: &str = "CARTOG_TEST_SKIP_PEER_LOCK";

#[cfg(test)]
fn peer_lock_check_skipped() -> bool {
    std::env::var_os(TEST_SKIP_PEER_LOCK_ENV).is_some()
}

#[cfg(not(test))]
fn peer_lock_check_skipped() -> bool {
    false
}

/// Peer-lock guard decision for `migrate-db`, factored out so it can be unit
/// tested without touching the filesystem or process state. A dry-run mutates
/// nothing, so it never blocks on a live peer; a real migration bails if any
/// peer holds a lock.
fn migrate_peer_guard(dry_run: bool, active: &[cartog_process_lock::ActiveLock]) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    if let Some(peer) = active.first() {
        anyhow::bail!(
            "another cartog process is running ({slot}, PID {pid}); stop it before migrating",
            slot = peer.slot,
            pid = peer.pid,
        );
    }
    Ok(())
}

/// `cartog self migrate-db [--dry-run]`. Moves legacy DB files into `.cartog/`.
/// Refuses to run while another cartog peer holds the lock.
pub fn cmd_self_migrate_db(root: &Path, dry_run: bool, json: bool) -> Result<()> {
    if !peer_lock_check_skipped() {
        let active = state::default_state_dir()
            .map(|dir| cartog_process_lock::find_active_locks(&dir))
            .unwrap_or_default();
        migrate_peer_guard(dry_run, &active)?;
    }

    let preview = plan_migration(root)?;
    if preview.is_empty() {
        emit_migrate_result(root, &preview, false, json, "nothing-to-do");
        return Ok(());
    }

    if dry_run {
        emit_migrate_result(root, &preview, false, json, "dry-run");
        return Ok(());
    }

    // Checkpointing closes the WAL/SHM siblings, so re-plan afterwards.
    // Non-fatal: the post-rename sweep picks up any siblings left behind.
    let legacy_db = root.join(cartog_db::LEGACY_DB_FILE);
    if let Err(e) = cartog_db::checkpoint_wal(&legacy_db) {
        tracing::warn!(
            path = %legacy_db.display(),
            error = %e,
            "WAL checkpoint failed before migrate-db; proceeding anyway",
        );
    }
    let moves = plan_migration(root)?;

    let new_dir = root.join(cartog_db::DB_DIR);
    std::fs::create_dir_all(&new_dir)
        .map_err(|e| anyhow::anyhow!("create_dir_all({}): {e}", new_dir.display()))?;

    for mv in &moves {
        std::fs::rename(&mv.from, &mv.to).map_err(|e| {
            anyhow::anyhow!(
                "failed to move {} → {}: {e}",
                mv.from.display(),
                mv.to.display(),
            )
        })?;
    }

    // Sweep again: another SQLite reader may have re-created -wal/-shm
    // between the checkpoint and the renames. Move them too so the new
    // layout has the full set and the legacy path is empty.
    let mut extra_moves = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let from = root.join(format!("{}{suffix}", cartog_db::LEGACY_DB_FILE));
        if from.exists() {
            let meta = std::fs::symlink_metadata(&from)
                .map_err(|e| anyhow::anyhow!("stat {}: {e}", from.display()))?;
            if meta.file_type().is_symlink() {
                anyhow::bail!(
                    "refusing to migrate {}: it is a symlink. Resolve or update it manually.",
                    from.display()
                );
            }
            let to = new_dir.join(format!("{}{suffix}", cartog_db::DB_FILENAME));
            if !to.exists() {
                std::fs::rename(&from, &to).map_err(|e| {
                    anyhow::anyhow!(
                        "post-move sweep failed to move {} → {}: {e}",
                        from.display(),
                        to.display(),
                    )
                })?;
                extra_moves.push(PlannedMove { from, to });
            }
        }
    }
    let mut all_moves = moves;
    all_moves.extend(extra_moves);

    emit_migrate_result(root, &all_moves, true, json, "migrated");
    Ok(())
}

#[derive(Debug, Serialize)]
struct MigrateOutcome<'a> {
    status: &'a str,
    root: String,
    performed: bool,
    moves: Vec<MigrateMove>,
}

#[derive(Debug, Serialize)]
struct MigrateMove {
    from: String,
    to: String,
}

fn emit_migrate_result(
    root: &Path,
    moves: &[PlannedMove],
    performed: bool,
    json: bool,
    status: &str,
) {
    if json {
        let outcome = MigrateOutcome {
            status,
            root: root.display().to_string(),
            performed,
            moves: moves
                .iter()
                .map(|m| MigrateMove {
                    from: m.from.display().to_string(),
                    to: m.to.display().to_string(),
                })
                .collect(),
        };
        println!(
            "{}",
            serde_json::to_string(&outcome).expect("MigrateOutcome serialises")
        );
        return;
    }
    if moves.is_empty() {
        println!(
            "cartog: no legacy database found at {} — nothing to migrate.",
            root.display()
        );
        return;
    }
    let verb = if performed { "Moved" } else { "Would move" };
    for m in moves {
        println!("{verb}: {} → {}", m.from.display(), m.to.display());
    }
    if !performed {
        println!("(dry run — pass without --dry-run to apply)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_name_drops_version_to_match_release_workflow() {
        assert_eq!(
            archive_name_for("aarch64-apple-darwin"),
            "cartog-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            archive_name_for("x86_64-pc-windows-msvc"),
            "cartog-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn parse_sha256sums_finds_named_entry() {
        let text = "\
abcd1234  cartog-aarch64-apple-darwin.tar.gz
deadbeef *cartog-x86_64-unknown-linux-gnu.tar.gz
# comment line
0123 cartog-x86_64-pc-windows-msvc.zip
";
        assert_eq!(
            parse_sha256sums(text, "cartog-aarch64-apple-darwin.tar.gz"),
            Some("abcd1234".to_string())
        );
        assert_eq!(
            parse_sha256sums(text, "cartog-x86_64-unknown-linux-gnu.tar.gz"),
            Some("deadbeef".to_string()),
            "binary-mode `*` prefix should be stripped"
        );
        assert_eq!(
            parse_sha256sums(text, "cartog-missing.tar.gz"),
            None,
            "absent entries should return None"
        );
    }

    #[test]
    fn compute_sha256_matches_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            compute_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            compute_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // is_leap / utc_breakdown / rfc3339_now tests live in `time_fmt::tests`.

    #[test]
    fn backup_path_appends_dot_old() {
        let bin = Path::new("/usr/local/bin/cartog");
        assert_eq!(
            backup_path_for(bin),
            PathBuf::from("/usr/local/bin/cartog.old")
        );
        // Windows-style suffix is preserved.
        let win = Path::new(r"C:\Program Files\cartog\cartog.exe");
        assert_eq!(
            backup_path_for(win),
            PathBuf::from(r"C:\Program Files\cartog\cartog.exe.old")
        );
    }

    #[test]
    fn bin_name_matches_target_os() {
        if cfg!(windows) {
            assert_eq!(bin_name_in_archive(), "cartog.exe");
        } else {
            assert_eq!(bin_name_in_archive(), "cartog");
        }
    }

    /// Sync + close before chmod to avoid Linux ETXTBSY on fast spawn.
    #[cfg(unix)]
    fn write_exec_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(body.as_bytes()).unwrap();
            f.sync_data().unwrap();
        }
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Spin until exec(2) on the script no longer hits ETXTBSY (Linux
    /// flags the inode briefly post-write).
    #[cfg(unix)]
    fn wait_for_exec_ready(bin: &Path) {
        for attempt in 0..10 {
            match std::process::Command::new(bin)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
                }
                Err(e) => panic!("unexpected spawn error from {bin:?}: {e}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_passes_on_zero_exit() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_exec_script(dir.path(), "ok", "#!/bin/sh\nexit 0\n");
        wait_for_exec_ready(&bin);
        smoke_test(&bin).expect("zero-exit binary must pass");
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_fails_on_non_zero_exit() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_exec_script(dir.path(), "fail", "#!/bin/sh\nexit 7\n");
        wait_for_exec_ready(&bin);
        let err = smoke_test(&bin).expect_err("non-zero exit must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("exited with"), "got: {msg}");
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_kills_a_hung_binary_after_timeout() {
        // Override the timeout via the SMOKE_TEST_TIMEOUT constant is not
        // possible without exposing a seam; instead, the deadline branch
        // is reachable as long as the script sleeps longer than the
        // 5-second ceiling. To keep this fast, we use a script that
        // sleeps for 30s — the watchdog kills it within the 5s budget.
        // A regression that drops the timeout would hang this test for
        // 30 seconds; the test runner's per-test budget is the safety
        // net. Marked #[ignore] would mask the bug; better to fail loud.
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_exec_script(dir.path(), "hang", "#!/bin/sh\nsleep 30\n");
        wait_for_exec_ready(&bin);
        let start = std::time::Instant::now();
        let err = smoke_test(&bin).expect_err("hanging binary must time out");
        let elapsed = start.elapsed();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("did not exit"),
            "expected timeout message, got: {msg}"
        );
        // Deadline is 5s; allow generous slack for slow CI but verify we
        // didn't actually wait the full 30s.
        assert!(
            elapsed < Duration::from_secs(15),
            "smoke_test should have killed the child within ~5s, took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sweep_removes_old_staging_dirs_and_keeps_fresh() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::TempDir::new().unwrap();
        let stale = dir.path().join(".cartog-update-stale");
        let fresh = dir.path().join(".cartog-update-fresh");
        let unrelated = dir.path().join("not-a-staging-dir");
        std::fs::create_dir(&stale).unwrap();
        std::fs::create_dir(&fresh).unwrap();
        std::fs::create_dir(&unrelated).unwrap();

        // Backdate the stale dir's mtime via utimes(2) — filetime is not in
        // deps and `tempfile` doesn't expose mtime mutation.
        let two_hours_ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 2 * 3600;
        let path_c = std::ffi::CString::new(stale.as_os_str().as_encoded_bytes()).unwrap();
        let times = [
            libc::timeval {
                tv_sec: two_hours_ago,
                tv_usec: 0,
            },
            libc::timeval {
                tv_sec: two_hours_ago,
                tv_usec: 0,
            },
        ];
        // SAFETY: utimes is POSIX; both args are valid pointers and `times`
        // points to a 2-element array as required.
        let rc = unsafe { libc::utimes(path_c.as_ptr(), times.as_ptr()) };
        assert_eq!(rc, 0, "utimes failed");
        let m = std::fs::metadata(&stale).unwrap();
        assert!(m.mtime() < two_hours_ago + 60);

        sweep_stale_staging_dirs(dir.path());

        assert!(!stale.exists(), "stale staging dir must be swept");
        assert!(fresh.exists(), "fresh staging dir must survive");
        assert!(unrelated.exists(), "non-cartog dirs must not be touched");
    }

    // ── resolve_install_source ────────────────────────────────────────

    #[test]
    fn resolve_release_tarball_short_circuits_runtime_detection() {
        // A release-built binary stays "release-tarball" no matter where
        // it sits on disk.
        assert_eq!(
            resolve_install_source(
                "release-tarball",
                Some(Path::new("/home/user/.cargo/bin/cartog")),
                None,
            ),
            "release-tarball",
        );
    }

    #[test]
    fn resolve_dev_binary_in_cargo_home_classified_as_cargo() {
        let cargo_home = Path::new("/home/user/.cargo");
        let bin = Path::new("/home/user/.cargo/bin/cartog");
        assert_eq!(
            resolve_install_source("dev", Some(bin), Some(cargo_home)),
            "cargo",
        );
    }

    #[test]
    fn resolve_dev_binary_outside_cargo_classified_as_dev() {
        assert_eq!(
            resolve_install_source(
                "dev",
                Some(Path::new("/usr/local/bin/cartog")),
                Some(Path::new("/home/user/.cargo")),
            ),
            "dev",
        );
    }

    #[test]
    fn resolve_dev_falls_back_to_path_heuristic_when_cargo_home_unset() {
        // Catches `~/.cargo/bin` even without CARGO_HOME (common on macOS).
        assert_eq!(
            resolve_install_source(
                "dev",
                Some(Path::new("/Users/alice/.cargo/bin/cartog")),
                None,
            ),
            "cargo",
        );
    }

    #[test]
    fn resolve_dev_no_binary_path_stays_dev() {
        assert_eq!(resolve_install_source("dev", None, None), "dev");
    }

    // ── parse_release_tag / is_stable_semver ──────────────────────────

    #[test]
    fn parse_release_tag_strips_v_prefix() {
        assert_eq!(
            parse_release_tag(r#"{"tag_name":"v0.14.0"}"#),
            Some("0.14.0".to_string()),
        );
        assert_eq!(
            parse_release_tag(r#"{"tag_name":"0.14.0"}"#),
            Some("0.14.0".to_string()),
            "leading v is optional",
        );
    }

    #[test]
    fn parse_release_tag_rejects_prereleases() {
        for tag in [
            r#"{"tag_name":"v0.14.0-rc.1"}"#,
            r#"{"tag_name":"v0.14.0-alpha"}"#,
            r#"{"tag_name":"v0.14.0-nightly.42"}"#,
        ] {
            assert_eq!(parse_release_tag(tag), None, "must reject {tag}");
        }
    }

    #[test]
    fn parse_release_tag_rejects_malformed() {
        assert_eq!(parse_release_tag("not json at all"), None);
        assert_eq!(parse_release_tag(r#"{}"#), None, "missing tag_name");
        assert_eq!(
            parse_release_tag(r#"{"tag_name":"v0.14"}"#),
            None,
            "two-part is not stable semver",
        );
        assert_eq!(
            parse_release_tag(r#"{"tag_name":"v0.14.0.0"}"#),
            None,
            "four-part is not stable semver",
        );
        assert_eq!(
            parse_release_tag(r#"{"tag_name":"vfoo.bar.baz"}"#),
            None,
            "non-numeric components rejected",
        );
    }

    #[test]
    fn is_stable_semver_accepts_canonical_triples() {
        assert!(is_stable_semver("0.0.0"));
        assert!(is_stable_semver("1.2.3"));
        assert!(is_stable_semver("99.0.0"));
    }

    #[test]
    fn is_stable_semver_rejects_anything_else() {
        assert!(!is_stable_semver(""));
        assert!(!is_stable_semver("1.2"));
        assert!(!is_stable_semver("1.2.3.4"));
        assert!(!is_stable_semver("1.2.x"));
        assert!(!is_stable_semver("1..3"));
    }

    // ── compare_stable_versions ───────────────────────────────────────

    #[test]
    fn compare_stable_versions_orders_lexicographically() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_stable_versions("0.13.2", "0.14.0"), Less);
        assert_eq!(compare_stable_versions("0.14.0", "0.13.2"), Greater);
        assert_eq!(compare_stable_versions("0.14.0", "0.14.0"), Equal);
        assert_eq!(compare_stable_versions("1.0.0", "0.99.99"), Greater);
        assert_eq!(compare_stable_versions("0.13.10", "0.13.2"), Greater);
    }

    #[test]
    fn compare_stable_versions_garbage_treated_as_zero() {
        // Documented degradation: non-numeric parts become 0 instead of panicking.
        use std::cmp::Ordering::*;
        assert_eq!(compare_stable_versions("0.0.0", "abc.def.ghi"), Equal);
        assert_eq!(compare_stable_versions("0.1.0", "abc.def.ghi"), Greater);
    }

    // ── CheckOutcome ──────────────────────────────────────────────────

    #[test]
    fn check_outcome_ok_marks_outdated_when_latest_is_newer() {
        let outcome = CheckOutcome::ok("0.13.2", "0.14.0");
        assert_eq!(outcome.outdated, Some(true));
        assert_eq!(outcome.latest.as_deref(), Some("0.14.0"));
        assert_eq!(outcome.current, "0.13.2");
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn check_outcome_ok_not_outdated_when_versions_match() {
        let outcome = CheckOutcome::ok("0.14.0", "0.14.0");
        assert_eq!(outcome.outdated, Some(false));
    }

    #[test]
    fn check_outcome_ok_not_outdated_when_local_is_ahead() {
        // Pre-release dev builds (e.g. 0.15.0 mid-development) must not be
        // told to "downgrade" to a published 0.14.0.
        let outcome = CheckOutcome::ok("0.15.0", "0.14.0");
        assert_eq!(outcome.outdated, Some(false));
    }

    #[test]
    fn check_outcome_failed_reports_error_with_null_latest() {
        let outcome = CheckOutcome::failed("0.13.2", "connection refused");
        assert_eq!(outcome.latest, None);
        assert_eq!(outcome.outdated, None);
        assert_eq!(outcome.error.as_deref(), Some("connection refused"));
    }

    #[test]
    fn check_outcome_to_human_outdated() {
        let s = CheckOutcome::ok("0.13.2", "0.14.0").to_human();
        assert!(s.contains("update available"), "got: {s}");
        assert!(s.contains("0.13.2"));
        assert!(s.contains("0.14.0"));
    }

    #[test]
    fn check_outcome_to_human_up_to_date() {
        let s = CheckOutcome::ok("0.14.0", "0.14.0").to_human();
        assert!(s.contains("up to date"), "got: {s}");
        assert!(s.contains("0.14.0"));
    }

    #[test]
    fn check_outcome_to_human_failed() {
        let s = CheckOutcome::failed("0.13.2", "DNS lookup failed").to_human();
        assert!(s.contains("update check failed"), "got: {s}");
        assert!(s.contains("DNS lookup failed"));
    }

    #[test]
    fn check_outcome_serialises_with_skip_none_keys() {
        // Failure shape must omit `latest` and `outdated`, never serialise as null.
        let outcome = CheckOutcome::failed("0.13.2", "boom");
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains(r#""error":"boom""#));
        assert!(json.contains(r#""current":"0.13.2""#));
        assert!(
            !json.contains("latest"),
            "latest must be skipped, got: {json}"
        );
        assert!(
            !json.contains("outdated"),
            "outdated must be skipped, got: {json}"
        );
    }

    #[test]
    fn check_outcome_serialises_success_with_all_fields() {
        let outcome = CheckOutcome::ok("0.13.2", "0.14.0");
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains(r#""current":"0.13.2""#));
        assert!(json.contains(r#""latest":"0.14.0""#));
        assert!(json.contains(r#""outdated":true"#));
        assert!(!json.contains("error"), "error must be skipped on success");
    }

    // ── looks_like_cargo_install ─────────────────────────────────────

    #[test]
    fn cargo_install_detected_via_cargo_home_prefix() {
        assert!(looks_like_cargo_install(
            Path::new("/home/u/.cargo/bin/cartog"),
            Some(Path::new("/home/u/.cargo")),
        ));
    }

    #[test]
    fn cargo_install_detected_via_dotcargo_path_segment() {
        assert!(looks_like_cargo_install(
            Path::new("/Users/alice/.cargo/bin/cartog"),
            None,
        ));
    }

    #[test]
    fn cargo_install_not_detected_for_unrelated_paths() {
        assert!(!looks_like_cargo_install(
            Path::new("/usr/local/bin/cartog"),
            None,
        ));
        assert!(!looks_like_cargo_install(
            Path::new("/home/u/.cargo-tools/bin/cartog"),
            None,
        ));
    }

    // ── migrate-db ──────────────────────────────────────────────────────────

    #[test]
    fn plan_migration_no_legacy_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let moves = plan_migration(dir.path()).expect("plan succeeds");
        assert!(moves.is_empty());
    }

    #[test]
    fn plan_migration_moves_db_and_wal_siblings() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(".cartog.db"), b"db").unwrap();
        std::fs::write(dir.path().join(".cartog.db-wal"), b"wal").unwrap();
        std::fs::write(dir.path().join(".cartog.db-shm"), b"shm").unwrap();
        std::fs::write(
            dir.path().join(".cartog.db.pre-v3-20260101T000000Z.bak"),
            b"bak",
        )
        .unwrap();

        let moves = plan_migration(dir.path()).expect("plan succeeds");
        let names: std::collections::BTreeSet<_> = moves
            .iter()
            .map(|m| m.to.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let expected: std::collections::BTreeSet<_> = [
            "db.sqlite".to_string(),
            "db.sqlite-wal".to_string(),
            "db.sqlite-shm".to_string(),
            "db.sqlite.pre-v3-20260101T000000Z.bak".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(names, expected);
        for m in &moves {
            assert_eq!(m.to.parent().unwrap(), dir.path().join(".cartog"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn plan_migration_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let real_target = dir.path().join("real.db");
        std::fs::write(&real_target, b"real").unwrap();
        symlink(&real_target, dir.path().join(".cartog.db")).unwrap();

        let err = plan_migration(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "expected symlink refusal, got: {err}"
        );
    }

    #[test]
    fn plan_migration_refuses_when_destination_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(".cartog.db"), b"db").unwrap();
        std::fs::create_dir_all(dir.path().join(".cartog")).unwrap();
        std::fs::write(dir.path().join(".cartog").join("db.sqlite"), b"existing").unwrap();

        let err = plan_migration(dir.path()).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    #[serial_test::serial]
    fn cmd_self_migrate_db_dry_run_does_not_move() {
        let dir = tempfile::TempDir::new().unwrap();
        let legacy = dir.path().join(".cartog.db");
        std::fs::write(&legacy, b"db").unwrap();

        // Safety: tests using process env vars are serialised via #[serial].
        unsafe { std::env::set_var(TEST_SKIP_PEER_LOCK_ENV, "1") };
        let result = cmd_self_migrate_db(dir.path(), true, true);
        unsafe { std::env::remove_var(TEST_SKIP_PEER_LOCK_ENV) };
        result.expect("dry run succeeds");

        assert!(legacy.exists(), "dry run must not touch the filesystem");
        assert!(!dir.path().join(".cartog").exists());
    }

    #[test]
    fn migrate_peer_guard_bails_for_real_run_when_peer_present() {
        let active = vec![cartog_process_lock::ActiveLock {
            slot: "serve-abc".to_string(),
            pid: 4242,
            start_time: None,
        }];
        let err = migrate_peer_guard(false, &active).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("serve-abc"), "names the slot: {msg}");
        assert!(msg.contains("4242"), "names the pid: {msg}");
    }

    #[test]
    fn migrate_peer_guard_allows_dry_run_despite_live_peer() {
        // Same live peer as above, but dry_run=true must bypass the guard.
        let active = vec![cartog_process_lock::ActiveLock {
            slot: "serve-abc".to_string(),
            pid: 4242,
            start_time: None,
        }];
        migrate_peer_guard(true, &active).expect("dry-run ignores the peer lock");
    }

    #[test]
    fn migrate_peer_guard_allows_real_run_when_no_peer() {
        migrate_peer_guard(false, &[]).expect("no peer → real run proceeds");
    }

    #[test]
    #[serial_test::serial]
    fn cmd_self_migrate_db_moves_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let legacy = dir.path().join(".cartog.db");

        // Create a real SQLite database so the WAL checkpoint can run without
        // tripping on a malformed file. The DB content itself is irrelevant.
        {
            let db = cartog_db::Database::open(&legacy, 384).unwrap();
            drop(db);
        }
        assert!(legacy.exists());

        unsafe { std::env::set_var(TEST_SKIP_PEER_LOCK_ENV, "1") };
        let result = cmd_self_migrate_db(dir.path(), false, true);
        unsafe { std::env::remove_var(TEST_SKIP_PEER_LOCK_ENV) };
        result.expect("migrate succeeds");

        assert!(!legacy.exists());
        let new_db = dir.path().join(".cartog").join("db.sqlite");
        assert!(new_db.exists(), "main db moved into .cartog/");
        // The migrated DB still opens cleanly.
        let _ = cartog_db::Database::open(&new_db, 384).expect("migrated db opens");
    }
}
