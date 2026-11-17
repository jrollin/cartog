//! Binary download, checksum verification, atomic swap, and smoke test.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use super::*;
use crate::state::{self, State};
use cartog::time_fmt::rfc3339_now;

/// Download, verify, swap, and smoke-test `latest`. `stale_peers` are the
/// serve/watch processes a forced swap ran under; they are reported as still on
/// the previous version.
pub(crate) fn perform_upgrade(
    current: &str,
    latest: &str,
    stale_peers: &[RunningPeer],
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
    let current_bin = installed_binary_path()
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
            if is_permission_error(&e) {
                UpgradeError::Filesystem(permission_hint(install_dir, latest, &e, cfg!(windows)))
            } else {
                UpgradeError::Filesystem(format!(
                    "failed to create staging dir under {}: {e}",
                    install_dir.display(),
                ))
            }
        })?;
    let archive_path = staging.path().join(&archive_name);
    std::fs::write(&archive_path, &archive_bytes)
        .map_err(|e| UpgradeError::Filesystem(format!("failed to stage archive: {e}")))?;
    let new_bin = extract_binary(&archive_path, staging.path())
        .map_err(|e| UpgradeError::Filesystem(format!("failed to extract binary: {e}")))?;

    let backup_path = backup_path_for(&current_bin);

    self_update::Move::from_source(&new_bin)
        .replace_using_temp(&backup_path)
        .to_dest(&current_bin)
        .map_err(|e| match e {
            self_update::errors::Error::Io(ref io) if is_permission_error(io) => {
                UpgradeError::Filesystem(permission_hint(install_dir, latest, io, cfg!(windows)))
            }
            other => UpgradeError::Filesystem(format!("atomic swap failed: {other}")),
        })?;

    if let Err(smoke_err) = smoke_test(&current_bin) {
        match std::fs::rename(&backup_path, &current_bin) {
            Ok(()) => {
                return Err(UpgradeError::Smoke(format!(
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
            let mut payload = serde_json::json!({
                "status": "updated",
                "current": current,
                "latest": latest,
                "backup": backup_path.to_string_lossy(),
            });
            if !stale_peers.is_empty() {
                payload["stale_peers"] = serde_json::json!(stale_peers);
            }
            println!("{payload}");
        } else {
            println!(
                "cartog: updated {current} -> {latest} (previous binary saved at {})",
                backup_path.display()
            );
            if !stale_peers.is_empty() {
                println!("{}", stale_peers_message(stale_peers, latest));
            }
        }
    }
    Ok(())
}

/// The binary installed where this process was started from.
///
/// After another updater swapped the binary, Linux reports this process's
/// executable at its renamed path (`<bin>.old`, or `<bin>.old (deleted)`, or
/// `<bin> (deleted)` after an installer replaced it). Swapping there would
/// overwrite the rollback copy instead of the installed binary.
pub(crate) fn installed_binary_path() -> std::io::Result<PathBuf> {
    std::env::current_exe().map(|running| installed_path_for(&running))
}

/// Map a running executable's reported path back to the installed binary.
#[must_use]
pub(crate) fn installed_path_for(running: &Path) -> PathBuf {
    let Some(name) = running.file_name().and_then(|n| n.to_str()) else {
        return running.to_path_buf();
    };
    let name = name.strip_suffix(" (deleted)").unwrap_or(name);
    let name = name.strip_suffix(".old").unwrap_or(name);
    running.with_file_name(name)
}

/// The version `bin --version` reports, or `None` if it cannot be run or read
/// within [`SMOKE_TEST_TIMEOUT`].
pub(crate) fn installed_binary_version(bin: &Path) -> Option<String> {
    let mut child = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + SMOKE_TEST_TIMEOUT;
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    parse_version_output(&out)
}

/// `cartog 0.36.0` (first line of `--version`) → `0.36.0`.
#[must_use]
pub(crate) fn parse_version_output(out: &str) -> Option<String> {
    let version = out.lines().next()?.strip_prefix("cartog ")?.trim();
    is_stable_semver(version).then(|| version.to_string())
}

/// Whether an I/O failure means this user cannot write the install directory.
pub(crate) fn is_permission_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Single-quote `s` for a POSIX shell, so a pasted command survives spaces.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The failure message when cartog cannot write its own install directory,
/// ending with a command the user can run themselves. An agent often cannot
/// (no sudo password, sandboxed shell), so the command must be copy-pasteable.
pub(crate) fn permission_hint(
    install_dir: &Path,
    target: &str,
    err: &std::io::Error,
    windows: bool,
) -> String {
    let dir = install_dir.display().to_string();
    if windows {
        format!(
            "cannot write to {dir} ({err}). Run `cartog self update --to {target}` from a \
             terminal opened as administrator, or download the .zip for {target} from \
             https://github.com/jrollin/cartog/releases"
        )
    } else {
        format!(
            "cannot write to {dir} ({err}). Run this in your own terminal; it asks for sudo \
             only if needed: curl -fsSL https://www.cartog.dev/install.sh | \
             CARTOG_VERSION={target} CARTOG_INSTALL_DIR={} sh",
            shell_quote(&dir),
        )
    }
}

/// Emit a one-line status message in the right shape for the user.
pub(crate) fn emit_upgrade_message(quiet: bool, json: bool, status: &str, message: &str) {
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
pub(crate) fn archive_name_for(target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("cartog-{target}.{ext}")
}

pub(crate) fn bin_name_in_archive() -> &'static str {
    if cfg!(windows) {
        "cartog.exe"
    } else {
        "cartog"
    }
}

/// Extract the cartog binary from a release archive into `into`, returning its path.
pub(crate) fn extract_binary(archive: &Path, into: &Path) -> Result<PathBuf> {
    self_update::Extract::from_source(archive).extract_file(into, bin_name_in_archive())?;
    Ok(into.join(bin_name_in_archive()))
}

pub(crate) fn backup_path_for(current: &Path) -> PathBuf {
    let mut name = current
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("cartog"));
    name.push(".old");
    current.with_file_name(name)
}

/// Find the hash for `archive_name` in a `sha256sum -c`-style file.
/// Lines look like `<hex>  <filename>` (two spaces or one + a `*`).
pub(crate) fn parse_sha256sums(text: &str, archive_name: &str) -> Option<String> {
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

pub(crate) fn compute_sha256(bytes: &[u8]) -> String {
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
///
/// Raised 5s → 30s. The ceiling only has to be *finite* to serve its purpose
/// (unwedging a hung binary), while being too tight has a much worse failure:
/// a **healthy** binary gets rejected, the swap is rolled back, and the target
/// is marked as having failed verification. That happened — a freshly-written
/// ~88 MB binary, cold in the page cache, first-execution on a machine running
/// a dozen concurrent processes, exceeded 5s and was thrown away.
///
/// A hang costs the user 30s once; a false rejection costs them the upgrade
/// and tells them their download was corrupt. The asymmetry says be generous.
pub(crate) const SMOKE_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Stale staging directory cutoff. A previous upgrade killed by SIGINT
/// or SIGKILL leaves `.cartog-update-<rand>/` behind; anything older
/// than this is safely abandoned.
const STAGING_SWEEP_AGE: Duration = Duration::from_secs(3600);

/// Best-effort sweep of `.cartog-update-*` directories left behind by a
/// previous interrupted upgrade. Errors are swallowed — this runs as a
/// hygiene step before the real upgrade, never the operation the user
/// asked for.
pub(crate) fn sweep_stale_staging_dirs(install_dir: &Path) {
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

/// Verify a freshly-swapped binary answers `--version` within
/// [`SMOKE_TEST_TIMEOUT`].
///
/// # Errors
///
/// Errors if the binary cannot be spawned, exits non-zero, or does not exit
/// before the ceiling (in which case the child is killed).
pub(crate) fn smoke_test(bin: &Path) -> Result<()> {
    smoke_test_within(bin, SMOKE_TEST_TIMEOUT)
}

/// [`smoke_test`] with an explicit ceiling.
///
/// Takes the timeout as a parameter rather than reading a global so tests can
/// exercise the deadline branch cheaply. An env-var seam was tried first and
/// rejected: a process-global override makes every *sibling* test's
/// correctness depend on scheduling — a 300 ms ceiling leaking out of the
/// hung-binary test made a healthy-binary test time out. A parameter cannot
/// leak.
pub(crate) fn smoke_test_within(bin: &Path, timeout: Duration) -> Result<()> {
    let mut child = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let deadline = std::time::Instant::now() + timeout;
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
                    anyhow::bail!("{bin:?} --version did not exit within {timeout:?}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
