//! `cartog self version` and update-availability checks.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::state::{self, State};

const COMPILE_TIME_INSTALL_SOURCE: &str = env!("CARTOG_INSTALL_SOURCE");

/// Compile-time target triple, e.g. `aarch64-apple-darwin`.
pub(crate) const TARGET_TRIPLE: &str = env!("CARTOG_TARGET_TRIPLE");

/// `git describe` display version (e.g. `v0.29.1-2-g3e2822c`); distinct from
/// the clean semver in [`VersionInfo::version`] used for update comparisons.
const BUILD_VERSION: &str = env!("CARTOG_BUILD_VERSION");

/// Test seam: when set to `release-tarball`, `cargo`, or `dev`, the install
/// source is forced to that value, bypassing the compile-time + path
/// heuristics. Lets the integration suite drive the cargo-refusal branch
/// without producing a real cargo install. Read only by `effective_install_source`.
const TEST_INSTALL_SOURCE_ENV: &str = "CARTOG_TEST_INSTALL_SOURCE";

/// Resolve the install source, honoring the test override env var if set.
pub(crate) fn effective_install_source() -> &'static str {
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

pub(crate) fn looks_like_cargo_install(binary_path: &Path, cargo_home: Option<&Path>) -> bool {
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
    /// `git describe` display string; additive JSON field (no `deny_unknown_fields`).
    /// Serialized as `describe` to match the human label and docs.
    #[serde(rename = "describe")]
    pub build_version: String,
    pub target: String,
    pub install_source: String,
    /// RFC3339 timestamp of the last successful update check, or `None`.
    /// Serialised as JSON `null` when absent.
    pub last_update_check: Option<String>,
    /// A deferred update armed but not yet applied. Lets the SessionStart hook
    /// read the pending target via the binary's own state-path resolution
    /// instead of re-deriving the platform path in shell. `None` when nothing
    /// is armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_update: Option<crate::state::PendingUpdate>,
}

impl VersionInfo {
    pub(crate) fn build(state: &State) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            build_version: BUILD_VERSION.to_string(),
            target: TARGET_TRIPLE.to_string(),
            install_source: effective_install_source().to_string(),
            last_update_check: state.last_update_check.clone(),
            pending_update: state.pending_update.clone(),
        }
    }

    /// Render the human-readable form printed when `--json` is not set.
    pub(crate) fn render_human(&self) -> String {
        let last = self.last_update_check.as_deref().unwrap_or("never");
        // Values aligned to the widest label (`last update check:`) so all four
        // detail lines start their value at the same column.
        format!(
            "cartog {version}\n  describe:          {build}\n  target:            {target}\n  install source:    {source}\n  last update check: {last}\n",
            version = self.version,
            build = self.build_version,
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

const DEFAULT_GITHUB_LATEST_URL: &str =
    "https://api.github.com/repos/jrollin/cartog/releases/latest";

/// Resolve the GitHub latest-release endpoint. Honors `CARTOG_GITHUB_API_URL`
/// for tests and locked-down environments; falls back to the public default.
pub(crate) fn github_latest_url() -> String {
    std::env::var("CARTOG_GITHUB_API_URL").unwrap_or_else(|_| DEFAULT_GITHUB_LATEST_URL.to_string())
}

/// Fetch the latest stable release tag from GitHub and return it as a bare
/// `MAJOR.MINOR.PATCH` string. Errors out on transport failure, non-2xx
/// status, malformed JSON, or a tag carrying a prerelease suffix.
pub(crate) fn fetch_latest_version(url: &str) -> Result<String> {
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
pub(crate) fn parse_release_tag(json: &str) -> Option<String> {
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
pub(crate) fn is_stable_semver(s: &str) -> bool {
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
pub(crate) struct CheckOutcome {
    pub(crate) current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) outdated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl CheckOutcome {
    pub(crate) fn ok(current: &str, latest: &str) -> Self {
        let outdated = compare_stable_versions(current, latest) == std::cmp::Ordering::Less;
        Self {
            current: current.to_string(),
            latest: Some(latest.to_string()),
            outdated: Some(outdated),
            error: None,
        }
    }

    pub(crate) fn failed(current: &str, error: &str) -> Self {
        Self {
            current: current.to_string(),
            latest: None,
            outdated: None,
            error: Some(error.to_string()),
        }
    }

    pub(crate) fn to_human(&self) -> String {
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
pub(crate) fn compare_stable_versions(a: &str, b: &str) -> std::cmp::Ordering {
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
