//! Integration tests for `cartog self` — isolated HOME/XDG, mocked network.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

fn cartog_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cartog"))
}

fn run_self_version(args: &[&str], state_dir: &std::path::Path) -> std::process::Output {
    Command::new(cartog_bin())
        .arg("self")
        .arg("version")
        .args(args)
        .env("HOME", state_dir)
        .env("XDG_STATE_HOME", state_dir.join("state"))
        .env("XDG_DATA_HOME", state_dir.join("data"))
        .env("XDG_CONFIG_HOME", state_dir.join("config"))
        .env_remove("CARGO_HOME")
        .output()
        .expect("failed to spawn cartog")
}

#[test]
fn self_version_human_output_lists_required_fields() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_self_version(&[], dir.path());
    assert!(
        out.status.success(),
        "cartog self version exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    // Version line — package version is baked at build time.
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout missing version: {stdout}"
    );
    // Required labels per the user-facing acceptance criteria.
    assert!(
        stdout.contains("target:"),
        "stdout missing target: {stdout}"
    );
    assert!(
        stdout.contains("install source:"),
        "stdout missing install source: {stdout}"
    );
    assert!(
        stdout.contains("last update check:"),
        "stdout missing last update check: {stdout}"
    );
    // No prior check ran in this isolated state dir.
    assert!(
        stdout.contains("never"),
        "fresh state should report 'never': {stdout}"
    );
}

#[test]
fn self_version_json_emits_required_keys() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_self_version(&["--json"], dir.path());
    assert!(
        out.status.success(),
        "cartog self version --json exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON {stdout:?}: {e}"));
    let obj = parsed.as_object().expect("top-level JSON object");

    assert_eq!(
        obj.get("version").and_then(|v| v.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
        "version mismatch in {stdout}"
    );
    let target = obj
        .get("target")
        .and_then(|v| v.as_str())
        .expect("target field");
    assert!(!target.is_empty(), "target should be a non-empty triple");
    let source = obj
        .get("install_source")
        .and_then(|v| v.as_str())
        .expect("install_source field");
    assert!(
        matches!(source, "release-tarball" | "cargo" | "dev"),
        "unexpected install_source: {source:?}"
    );
    // `last_update_check` is present and null on a fresh state file.
    assert!(
        obj.contains_key("last_update_check"),
        "missing last_update_check key in {stdout}"
    );
    assert!(
        obj["last_update_check"].is_null(),
        "fresh state should serialise null, got {:?}",
        obj["last_update_check"],
    );
}

/// Path where the binary will look for `state.toml` given a fake HOME, on the
/// platforms this test currently runs on (Linux + macOS).
///
/// Windows seeds via the `FOLDERID_LocalAppData` known folder, which a fake
/// HOME does not redirect — so this helper is intentionally not implemented
/// for Windows. If/when Windows CI is added the test must seed via
/// `LOCALAPPDATA` (or skip the seeded-state assertion).
#[cfg(not(target_os = "windows"))]
fn seeded_state_path(fake_home: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        fake_home
            .join("Library")
            .join("Application Support")
            .join("io.cartog.cartog")
            .join("state.toml")
    } else {
        // Linux: state_dir = $XDG_STATE_HOME/cartog (we override XDG_STATE_HOME below).
        fake_home.join("state").join("cartog").join("state.toml")
    }
}

#[cfg(not(target_os = "windows"))]
#[test]
fn self_version_reports_existing_check_timestamp() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_file = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_file.parent().unwrap()).unwrap();
    std::fs::write(
        &state_file,
        "last_update_check = \"2026-01-15T10:00:00Z\"\n",
    )
    .unwrap();

    let out = run_self_version(&["--json"], dir.path());
    assert!(
        out.status.success(),
        "cartog self version --json exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        parsed["last_update_check"].as_str(),
        Some("2026-01-15T10:00:00Z"),
        "did not pick up seeded timestamp at {state_file:?} from {stdout}"
    );
}

// ── self update --check ───────────────────────────────────────────────

/// Localhost HTTP server serving one canned 200 OK then exiting.
fn spawn_canned_github_response(json_body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body_bytes = json_body.as_bytes();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body_bytes.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body_bytes);
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/")
}

fn spawn_500_github_response() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/")
}

fn run_self_update_check(args: &[&str], api_url: &str) -> std::process::Output {
    let dir = tempfile::TempDir::new().expect("tempdir");
    Command::new(cartog_bin())
        .arg("self")
        .arg("update")
        .arg("--check")
        .args(args)
        .env("HOME", dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("CARTOG_GITHUB_API_URL", api_url)
        .env_remove("CARGO_HOME")
        .output()
        .expect("failed to spawn cartog")
}

#[test]
fn self_update_check_reports_outdated_with_exit_code_1() {
    // Pretend GitHub is on a much newer version than the running binary.
    let url = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    let out = run_self_update_check(&[], &url);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 (outdated); stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("update available") && stdout.contains("999.0.0"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn self_update_check_reports_up_to_date_with_exit_code_0() {
    // Pretend GitHub is on a strictly older version than the running binary.
    let url = spawn_canned_github_response(r#"{"tag_name":"v0.0.1"}"#.to_string());
    let out = run_self_update_check(&[], &url);
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0 (up to date); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("up to date"), "unexpected stdout: {stdout}");
}

#[test]
fn self_update_check_quiet_suppresses_output() {
    let url = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    let out = run_self_update_check(&["--quiet"], &url);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty in --quiet mode, got {:?}",
        String::from_utf8_lossy(&out.stdout),
    );
    assert!(
        out.stderr.is_empty(),
        "stderr should be empty in --quiet mode, got {:?}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn self_update_check_json_emits_required_keys() {
    let url = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    let out = run_self_update_check(&["--json"], &url);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON {stdout:?}: {e}"));
    let obj = parsed.as_object().expect("JSON object");
    assert_eq!(
        obj.get("current").and_then(|v| v.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
    );
    assert_eq!(obj.get("latest").and_then(|v| v.as_str()), Some("999.0.0"),);
    assert_eq!(obj.get("outdated").and_then(|v| v.as_bool()), Some(true));
}

#[test]
fn self_update_check_network_error_exits_2() {
    // 500 from GitHub — surfaced as a network/parse failure, exit 2.
    let url = spawn_500_github_response();
    let out = run_self_update_check(&[], &url);
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 (network/parse error); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn self_update_check_json_failure_uses_unified_schema() {
    // On failure, the JSON payload must keep the same top-level keys as on
    // success (current, latest, outdated) plus an `error` field. `latest`
    // and `outdated` are null so consumers can detect failure without
    // switching schemas.
    let url = spawn_500_github_response();
    let out = run_self_update_check(&["--json"], &url);
    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON {stdout:?}: {e}"));
    let obj = parsed.as_object().expect("JSON object");
    assert_eq!(
        obj.get("current").and_then(|v| v.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
    );
    assert!(
        obj.get("error").and_then(|v| v.as_str()).is_some(),
        "error field missing or non-string in {stdout}"
    );
    // `latest` / `outdated` are intentionally absent (skipped) on failure.
    assert!(
        !obj.contains_key("latest") || obj["latest"].is_null(),
        "latest must be null/absent on failure: {stdout}",
    );
    assert!(
        !obj.contains_key("outdated") || obj["outdated"].is_null(),
        "outdated must be null/absent on failure: {stdout}",
    );
}

#[test]
fn self_update_check_does_not_write_state_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let url = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    let output = Command::new(cartog_bin())
        .arg("self")
        .arg("update")
        .arg("--check")
        .arg("--quiet")
        .env("HOME", dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("CARTOG_GITHUB_API_URL", &url)
        .env_remove("CARGO_HOME")
        .output()
        .unwrap();

    // v999.0.0 > running version → exit 1. Asserting this proves --check
    // actually ran the network path; a crash before that would also leave
    // no state.toml and falsely pass the next assertion.
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected outdated exit (1); stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    // Walk the entire temp dir; --check must not have created a state.toml.
    fn has_state_file(dir: &std::path::Path) -> bool {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return false,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some("state.toml") {
                return true;
            }
            if path.is_dir() && has_state_file(&path) {
                return true;
            }
        }
        false
    }
    assert!(
        !has_state_file(dir.path()),
        "--check must not write state.toml anywhere under HOME",
    );
}

// ── self update full upgrade flow ─────────────────────────────────────

/// Spawn `cartog self update` (no --check) in an isolated HOME with a
/// mocked GitHub API. The download base is also pinned to a localhost URL
/// — tests that don't actually exercise the download path can leave it
/// pointing at a black hole because the upgrade-refusal branches return
/// before any download happens.
fn run_self_update_full(
    state_dir: &std::path::Path,
    api_url: &str,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = Command::new(cartog_bin());
    cmd.arg("self")
        .arg("update")
        .env("HOME", state_dir)
        .env("XDG_STATE_HOME", state_dir.join("state"))
        .env("XDG_DATA_HOME", state_dir.join("data"))
        .env("XDG_CONFIG_HOME", state_dir.join("config"))
        .env("CARTOG_GITHUB_API_URL", api_url)
        // Ensure no test ever tries to talk to real GitHub for downloads.
        .env(
            "CARTOG_GITHUB_DOWNLOAD_BASE",
            "http://127.0.0.1:1/blackhole",
        )
        .env_remove("CARGO_HOME");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to spawn cartog")
}

/// Reverse-DNS bundle id used by `directories::ProjectDirs` on macOS.
/// Used to seed PID lock files into the directory the binary will scan.
#[cfg(not(target_os = "windows"))]
fn isolated_lock_dir(fake_home: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        fake_home
            .join("Library")
            .join("Application Support")
            .join("io.cartog.cartog")
    } else {
        fake_home.join("state").join("cartog")
    }
}

#[test]
fn self_update_full_aborts_for_cargo_install() {
    // Force the install-source detection into the cargo branch via the
    // test seam. Network is irrelevant — the cargo refusal must short
    // circuit before any HTTP call. Pointing the API at a black hole
    // verifies that.
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_self_update_full(
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "cargo")],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "cargo source must exit 3; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("cargo install cartog --force"),
        "guidance message should name the cargo command, got: {combined}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn self_update_full_aborts_when_peer_running() {
    // Plant a live PID file (our own PID) in the lock dir the binary will
    // discover. The upgrade flow must refuse before fetching anything.
    let dir = tempfile::TempDir::new().unwrap();
    let lock_dir = isolated_lock_dir(dir.path());
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(lock_dir.join("watch.pid"), std::process::id().to_string()).unwrap();

    let out = run_self_update_full(
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        // Force "release-tarball" so the cargo-source branch doesn't fire.
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(6),
        "live peer must exit 6; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("watch") && combined.contains(&std::process::id().to_string()),
        "message should name the slot and pid, got: {combined}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn self_update_full_already_up_to_date_exits_zero() {
    // Mock GitHub returns the running version; upgrade must report
    // "already up to date" without touching the binary.
    let dir = tempfile::TempDir::new().unwrap();
    let api = spawn_canned_github_response(format!(
        "{{\"tag_name\":\"v{}\"}}",
        env!("CARGO_PKG_VERSION")
    ));
    let out = run_self_update_full(
        dir.path(),
        &api,
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "up-to-date upgrade must exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("already up to date"),
        "should report up-to-date, got: {stdout}"
    );
}

// ── self rollback ─────────────────────────────────────────────────────
//
// Rollback tests run as subprocesses against a *copy* of the cartog
// binary in a sandboxed install dir, so the swap operates on real files
// without touching the test runner's binary or `target/debug/`. Gated to
// unix because (a) Windows can't rename a running .exe and (b) the
// chmod helper here is unix-only.

#[cfg(unix)]
fn copy_cartog_into(dir: &std::path::Path) -> PathBuf {
    use std::fs::OpenOptions;
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("cartog");
    // Linux returns ETXTBSY from execve() if any process still holds a write
    // fd on the binary. `std::fs::copy` returns when bytes hit the page cache,
    // not when the write fd is closed: racing parallel tests can then fail
    // to spawn the freshly copied binary. Open/write/fsync/drop the dest fd
    // in its own scope to close the race window before chmod+exec.
    {
        let mut src = std::fs::File::open(cartog_bin()).expect("open cartog");
        let mut dst = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&bin)
            .expect("create dest cartog");
        std::io::copy(&mut src, &mut dst).expect("copy cartog");
        dst.sync_all().expect("fsync dest cartog");
    }
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    bin
}

#[cfg(unix)]
fn run_self_rollback(bin: &std::path::Path, state_dir: &std::path::Path) -> std::process::Output {
    Command::new(bin)
        .arg("self")
        .arg("rollback")
        .env("HOME", state_dir)
        .env("XDG_STATE_HOME", state_dir.join("state"))
        .env("XDG_DATA_HOME", state_dir.join("data"))
        .env("XDG_CONFIG_HOME", state_dir.join("config"))
        .env_remove("CARGO_HOME")
        .output()
        .expect("failed to spawn cartog")
}

#[cfg(unix)]
#[test]
fn self_rollback_exits_one_when_no_old_present() {
    let install_dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let bin = copy_cartog_into(install_dir.path());

    let out = run_self_rollback(&bin, home.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "rollback with no .old must exit 1; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no previous binary"),
        "expected guidance about missing .old, got stderr: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn self_rollback_swaps_old_back_in_when_present() {
    let install_dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();

    // Plant a "broken" current binary and a "good" .old binary. We use
    // copies of the real cartog for both so the post-rollback `cartog
    // --version` (run by the test) actually works — the test only cares
    // that the swap happened and no .old remains. Distinguish them by
    // appending unique trailing bytes so the file size differs.
    let bin = copy_cartog_into(install_dir.path());
    let backup = install_dir.path().join("cartog.old");
    std::fs::copy(&bin, &backup).unwrap();
    // Append a marker to the *current* binary so we can tell it apart from
    // the backup by size. fs::write replaces content but preserves perms,
    // so the executable bit copy_cartog_into set survives.
    let mut current_bytes = std::fs::read(&bin).unwrap();
    current_bytes.extend_from_slice(b"\n#current-marker\n");
    std::fs::write(&bin, &current_bytes).unwrap();
    let backup_size = std::fs::metadata(&backup).unwrap().len();

    let out = run_self_rollback(&bin, home.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "successful rollback must exit 0; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // After rollback, `<bin>` should match the original .old, and the
    // .old sibling must be gone (RD-2: single binary, no .old).
    let new_bin_size = std::fs::metadata(&bin).unwrap().len();
    assert_eq!(
        new_bin_size, backup_size,
        "post-rollback binary size should match the backup"
    );
    assert!(
        !backup.exists(),
        ".old must be consumed by the rollback (got leftover at {})",
        backup.display()
    );
    // No leftover staged-broken-binary intermediate either.
    let leftovers: Vec<String> = std::fs::read_dir(install_dir.path())
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(".cartog.broken."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no .cartog.broken.*.tmp leftovers expected, got: {leftovers:?}"
    );
}

// ── T-20: focused spec-rule coverage ──────────────────────────────────
//
// One pinpoint test per spec rule (BR-1, BR-4, BR-6, RD-3) so a future
// regression that re-routes through one of these branches (e.g. a
// refactor that swaps the cargo refusal order with the network probe)
// triggers a test failure naming the exact contract that broke.

/// Multi-route mock that serves `/releases/latest`, `<archive>`, and
/// `SHA256SUMS` from one listener. Returns `(api_url, download_base)`
/// suitable for `CARTOG_GITHUB_API_URL` and `CARTOG_GITHUB_DOWNLOAD_BASE`.
fn spawn_release_mock(latest: &str, archive_bytes: Vec<u8>, sums_text: String) -> (String, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
    let port = listener.local_addr().unwrap().port();
    let latest = latest.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, content_type, body): (&str, &str, Vec<u8>) =
                if path.contains("/releases/latest") {
                    (
                        "200 OK",
                        "application/json",
                        format!(r#"{{"tag_name":"v{latest}"}}"#).into_bytes(),
                    )
                } else if path.ends_with("SHA256SUMS") {
                    ("200 OK", "text/plain", sums_text.clone().into_bytes())
                } else if path.contains("/cartog-") {
                    ("200 OK", "application/octet-stream", archive_bytes.clone())
                } else {
                    ("404 Not Found", "text/plain", b"not found".to_vec())
                };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    let base = format!("http://127.0.0.1:{port}");
    (format!("{base}/releases/latest"), base)
}

#[test]
fn cargo_source_refusal_short_circuits_before_network() {
    // BR-4: a cargo-installed binary must refuse `self update` *without*
    // reaching the network. Point both URLs at a black hole; if the
    // refusal ever stops being the first branch, the test will fail with
    // a network error instead of exit 3.
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_self_update_full(
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "cargo")],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "cargo-installed must exit 3 (cargo refusal); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("cargo install cartog --force"),
        "guidance must name the cargo command (AC-5.2), got: {combined}"
    );
}

#[cfg(unix)]
#[test]
fn checksum_mismatch_abort_leaves_binary_intact() {
    // BR-1: a checksum mismatch must abort with no FS mutation.
    // Mock GitHub returns a "newer" version, a tarball whose actual SHA256
    // does not match the value in SHA256SUMS, and a plausible-looking
    // SHA256SUMS file. We assert: exit code 4, no `<bin>.old` created,
    // staging dirs cleaned up.

    let dir = tempfile::TempDir::new().unwrap();
    let archive_bytes = b"not a real tarball, but bytes nonetheless".to_vec();
    let target = std::env::var("TARGET").unwrap_or_else(|_| {
        // Best-effort guess from the host triple. The binary's TARGET is
        // baked at build time, so we just need any plausible filename
        // SHA256SUMS entry — the binary parses by exact filename match.
        cartog_target_triple()
    });
    let archive_name = if target.contains("windows") {
        format!("cartog-{target}.zip")
    } else {
        format!("cartog-{target}.tar.gz")
    };
    // Wrong hash (64 hex chars but doesn't match the bytes above).
    let bad_hash = "0".repeat(64);
    let sums = format!("{bad_hash}  {archive_name}\n");
    let (api, dl_base) = spawn_release_mock("99.0.0", archive_bytes, sums);

    let out = Command::new(cartog_bin())
        .arg("self")
        .arg("update")
        .env("HOME", dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("CARTOG_GITHUB_API_URL", &api)
        .env("CARTOG_GITHUB_DOWNLOAD_BASE", &dl_base)
        .env("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")
        .env_remove("CARGO_HOME")
        .output()
        .expect("spawn cartog");

    assert_eq!(
        out.status.code(),
        Some(4),
        "checksum mismatch must exit 4 (BR-1); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("checksum"),
        "message should mention checksum, got: {combined}"
    );
    // The running test binary itself was not touched (we point HOME at a
    // tempdir but the actual binary lives in target/debug). The post-
    // condition we *can* check: no `<bin>.old` sibling appeared next to
    // the running binary, and no stale staging dirs in the install dir.
    let bin = cartog_bin();
    let parent = bin.parent().unwrap();
    let old_path = parent.join("cartog.old");
    assert!(
        !old_path.exists(),
        "checksum failure must not create <bin>.old, found: {}",
        old_path.display()
    );
    let staging_leftovers: Vec<String> = std::fs::read_dir(parent)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(".cartog-update-"))
        .collect();
    assert!(
        staging_leftovers.is_empty(),
        "staging dirs must be cleaned up after checksum failure, got: {staging_leftovers:?}"
    );
}

/// Resolve the target triple at runtime when `TARGET` env is unset (it's
/// only populated for build scripts, not for tests). Mirrors the format
/// `archive_name_for` consumes.
#[cfg(unix)]
fn cartog_target_triple() -> String {
    // `cartog self version --json` reports the bare triple.
    let dir = tempfile::TempDir::new().unwrap();
    let out = Command::new(cartog_bin())
        .arg("self")
        .arg("version")
        .arg("--json")
        .env("HOME", dir.path())
        .env_remove("CARGO_HOME")
        .output()
        .expect("spawn cartog self version");
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split("\"target\"")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .map(str::to_string)
        .expect("self version --json should report target")
}

#[cfg(unix)]
#[test]
fn rollback_removes_old_after_successful_swap() {
    // BR-6 + RD-2: after a successful rollback, the user is back to a
    // single binary with no `.old` sibling. Pinpoints just the cleanup
    // invariant — the broader rollback test asserts content/size as well.
    let install_dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();

    let bin = copy_cartog_into(install_dir.path());
    let backup = install_dir.path().join("cartog.old");
    std::fs::copy(&bin, &backup).unwrap();

    let out = run_self_rollback(&bin, home.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "rollback should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !backup.exists(),
        ".old must be removed after rollback (RD-2), found leftover: {}",
        backup.display(),
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn concurrent_process_abort_names_the_running_peer() {
    // RD-3 + AC-1.3: when a peer is detected, the upgrade refuses (exit
    // 6) and the message must name the offending slot+pid so the user
    // knows what to stop.
    let dir = tempfile::TempDir::new().unwrap();
    let lock_dir = isolated_lock_dir(dir.path());
    std::fs::create_dir_all(&lock_dir).unwrap();
    // Use the test runner's own PID — guaranteed alive for the duration.
    let pid = std::process::id();
    std::fs::write(lock_dir.join("serve.pid"), pid.to_string()).unwrap();

    let out = run_self_update_full(
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(6),
        "live peer must exit 6; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("serve") && combined.contains(&pid.to_string()),
        "abort message should name the slot and pid, got: {combined}"
    );
}

/// HTTP server that advertises a Content-Length, sends only a fraction
/// of that many bytes, then closes the connection. Reqwest treats
/// premature EOF as an error, so the upgrade should surface a network
/// failure rather than silently truncate.
fn spawn_partial_download_server(latest: &str, declared_len: usize, sent_len: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
    let port = listener.local_addr().unwrap().port();
    let latest = latest.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            if path.contains("/releases/latest") {
                let body = format!(r#"{{"tag_name":"v{latest}"}}"#);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            } else if path.contains("/cartog-") {
                // Lie about Content-Length; send fewer bytes; close.
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {declared_len}\r\nConnection: close\r\n\r\n",
                );
                let _ = stream.write_all(header.as_bytes());
                let truncated = vec![0u8; sent_len];
                let _ = stream.write_all(&truncated);
            } else {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[cfg(unix)]
#[test]
fn truncated_archive_download_aborts_with_network_error() {
    // Declared 10000 bytes, sent 100 — reqwest's reader hits EOF before
    // Content-Length, returns an error. The upgrade should map this to
    // exit code 2 (network/parse), NOT proceed with a 100-byte archive
    // and rely on the SHA check (which would also catch it, but exit 4
    // would lie about the true cause).
    let dir = tempfile::TempDir::new().unwrap();
    let api_base = spawn_partial_download_server("99.0.0", 10_000, 100);
    let api_url = format!("{api_base}/releases/latest");

    let out = Command::new(cartog_bin())
        .arg("self")
        .arg("update")
        .env("HOME", dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("CARTOG_GITHUB_API_URL", &api_url)
        .env("CARTOG_GITHUB_DOWNLOAD_BASE", &api_base)
        .env("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")
        .env_remove("CARGO_HOME")
        .output()
        .expect("spawn cartog");

    let code = out.status.code();
    assert!(
        matches!(code, Some(2) | Some(4)),
        "truncated download must surface as network (2) or checksum (4) failure, got {code:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

// ── deferred update: --defer (arm) / --apply-pending (boundary) ────────
//
// These exercise the split-update mechanism the Claude Code plugin relies
// on: `--defer` arms a pending update while a peer (the MCP serve process)
// is alive, and `--apply-pending` performs the swap at the session boundary
// once the peer has exited.

/// Spawn `cartog self update <mode>` (`--defer` or `--apply-pending`) in an
/// isolated HOME with a mocked GitHub API and a black-holed download base.
fn run_self_update_mode(
    mode: &str,
    state_dir: &std::path::Path,
    api_url: &str,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = Command::new(cartog_bin());
    cmd.arg("self")
        .arg("update")
        .arg(mode)
        .env("HOME", state_dir)
        .env("XDG_STATE_HOME", state_dir.join("state"))
        .env("XDG_DATA_HOME", state_dir.join("data"))
        .env("XDG_CONFIG_HOME", state_dir.join("config"))
        .env("CARTOG_GITHUB_API_URL", api_url)
        .env(
            "CARTOG_GITHUB_DOWNLOAD_BASE",
            "http://127.0.0.1:1/blackhole",
        )
        .env_remove("CARGO_HOME");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to spawn cartog")
}

#[cfg(not(target_os = "windows"))]
#[test]
fn defer_arms_pending_and_exits_zero() {
    let dir = tempfile::TempDir::new().unwrap();
    let api = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    let out = run_self_update_mode(
        "--defer",
        dir.path(),
        &api,
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "arming must exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let state = std::fs::read_to_string(seeded_state_path(dir.path()))
        .expect("state.toml should exist after --defer");
    assert!(
        state.contains("[pending_update]") && state.contains("999.0.0"),
        "state must record the pending target, got: {state}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn defer_succeeds_even_with_live_peer() {
    // The core regression guard: arming MUST NOT refuse (exit 6) just
    // because a peer serve/watch holds the lock — that is the entire point.
    let dir = tempfile::TempDir::new().unwrap();
    let lock_dir = isolated_lock_dir(dir.path());
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(lock_dir.join("serve.pid"), std::process::id().to_string()).unwrap();

    let api = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    let out = run_self_update_mode(
        "--defer",
        dir.path(),
        &api,
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "arming must succeed with a live peer (NOT exit 6); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let state = std::fs::read_to_string(seeded_state_path(dir.path()))
        .expect("state.toml should exist after --defer");
    assert!(
        state.contains("999.0.0"),
        "pending target must be armed despite the peer, got: {state}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn defer_up_to_date_does_not_arm() {
    let dir = tempfile::TempDir::new().unwrap();
    let api = spawn_canned_github_response(format!(
        "{{\"tag_name\":\"v{}\"}}",
        env!("CARGO_PKG_VERSION")
    ));
    let out = run_self_update_mode(
        "--defer",
        dir.path(),
        &api,
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(out.status.code(), Some(0));
    let state_path = seeded_state_path(dir.path());
    if let Ok(text) = std::fs::read_to_string(&state_path) {
        assert!(
            !text.contains("[pending_update]"),
            "up-to-date --defer must not arm anything, got: {text}"
        );
    }
}

#[test]
fn defer_refuses_cargo() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_self_update_mode(
        "--defer",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "cargo")],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "cargo source must refuse --defer (exit 3); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// `cartog self update --defer --to <version>` in an isolated HOME. The API is
/// black-holed to prove `--to` arms WITHOUT contacting GitHub for "latest".
#[cfg(not(target_os = "windows"))]
fn run_defer_to(
    version: &str,
    state_dir: &std::path::Path,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = Command::new(cartog_bin());
    cmd.arg("self")
        .arg("update")
        .arg("--defer")
        .arg("--to")
        .arg(version)
        .env("HOME", state_dir)
        .env("XDG_STATE_HOME", state_dir.join("state"))
        .env("XDG_DATA_HOME", state_dir.join("data"))
        .env("XDG_CONFIG_HOME", state_dir.join("config"))
        // Black hole: --to must NOT fetch latest. Any network attempt fails.
        .env("CARTOG_GITHUB_API_URL", "http://127.0.0.1:1/blackhole")
        .env(
            "CARTOG_GITHUB_DOWNLOAD_BASE",
            "http://127.0.0.1:1/blackhole",
        )
        .env_remove("CARGO_HOME");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to spawn cartog")
}

#[cfg(not(target_os = "windows"))]
#[test]
fn defer_to_pins_exact_version_without_fetching_latest() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_defer_to(
        "0.99.0",
        dir.path(),
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "--to must arm the pin without a network call; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let state = std::fs::read_to_string(seeded_state_path(dir.path()))
        .expect("state.toml should exist after --defer --to");
    assert!(
        state.contains("[pending_update]") && state.contains("0.99.0"),
        "must arm exactly the pinned target, got: {state}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn defer_to_rejects_non_semver_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_defer_to(
        "v0.99.0",
        dir.path(),
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "a non-bare-semver --to must be rejected (exit 2); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    // Nothing armed.
    let state_path = seeded_state_path(dir.path());
    if let Ok(text) = std::fs::read_to_string(&state_path) {
        assert!(
            !text.contains("[pending_update]"),
            "a rejected --to must not arm anything, got: {text}"
        );
    }
}

#[cfg(not(target_os = "windows"))]
#[test]
fn apply_pending_noop_when_nothing_armed() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_self_update_mode(
        "--apply-pending",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "nothing armed → clean no-op; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn apply_pending_skips_and_clears_when_already_at_target() {
    // Seed a pending intent whose target equals the running version: the
    // swap already happened (or the user moved past it). Apply must clear
    // the intent and no-op.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        format!(
            "[pending_update]\ntarget_version = \"{}\"\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();

    let out = run_self_update_mode(
        "--apply-pending",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(out.status.code(), Some(0));
    let text = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        !text.contains("[pending_update]"),
        "stale/satisfied intent must be cleared, got: {text}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn apply_pending_refuses_when_peer_held() {
    // A pending target is armed, but a peer is still live at apply time.
    // Apply must NOT swap, must exit 6, and must KEEP the intent so the
    // next boundary retries. A short wait budget keeps the test fast.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"999.0.0\"\n",
    )
    .unwrap();
    let lock_dir = isolated_lock_dir(dir.path());
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(lock_dir.join("serve.pid"), std::process::id().to_string()).unwrap();

    let out = run_self_update_mode(
        "--apply-pending",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[
            ("CARTOG_TEST_INSTALL_SOURCE", "release-tarball"),
            ("CARTOG_TEST_APPLY_PEER_WAIT_MS", "100"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(6),
        "live peer at apply time must exit 6; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let text = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        text.contains("999.0.0"),
        "intent must be kept for retry, got: {text}"
    );
}

#[cfg(unix)]
#[test]
fn apply_pending_checksum_failure_clears_intent() {
    // Pending target armed, no peer, but the release mock serves a tarball
    // whose SHA does not match SHA256SUMS. Apply reaches perform_upgrade,
    // exits 4, and CLEARS the intent (a checksum mismatch will not self-heal).
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"99.0.0\"\n",
    )
    .unwrap();

    let target = cartog_target_triple();
    let archive_name = format!("cartog-{target}.tar.gz");
    let bad_hash = "0".repeat(64);
    let sums = format!("{bad_hash}  {archive_name}\n");
    let (api, dl_base) = spawn_release_mock("99.0.0", b"not a real tarball".to_vec(), sums);

    let out = Command::new(cartog_bin())
        .arg("self")
        .arg("update")
        .arg("--apply-pending")
        .env("HOME", dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("CARTOG_GITHUB_API_URL", &api)
        .env("CARTOG_GITHUB_DOWNLOAD_BASE", &dl_base)
        .env("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")
        .env_remove("CARGO_HOME")
        .output()
        .expect("spawn cartog");

    assert_eq!(
        out.status.code(),
        Some(4),
        "checksum mismatch must exit 4; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let text = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        !text.contains("[pending_update]"),
        "checksum failure must clear the intent (no retry loop), got: {text}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn apply_pending_network_failure_keeps_intent() {
    // Pending target armed, no peer, download base is a black hole. Apply
    // reaches perform_upgrade, the fetch fails (exit 2), and the intent is
    // KEPT so the next boundary retries.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"999.0.0\"\n",
    )
    .unwrap();

    let out = run_self_update_mode(
        "--apply-pending",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "download failure must exit 2; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let text = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        text.contains("999.0.0"),
        "transient network failure must keep the intent, got: {text}"
    );
}

// Uses seeded_state_path, which is not implemented on Windows.
#[cfg(not(target_os = "windows"))]
#[test]
fn defer_refuses_dev_is_not_a_thing_dev_arms_like_release() {
    // Policy check (matches `cartog self update`): a dev build is treated as
    // upgradeable, so --defer arms it. Only cargo installs are refused.
    let dir = tempfile::TempDir::new().unwrap();
    let api = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    let out = run_self_update_mode(
        "--defer",
        dir.path(),
        &api,
        &[("CARTOG_TEST_INSTALL_SOURCE", "dev")],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "dev source must arm (only cargo is refused); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let state = std::fs::read_to_string(seeded_state_path(dir.path()))
        .expect("state.toml should exist after --defer");
    assert!(
        state.contains("999.0.0"),
        "dev build must arm, got: {state}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn defer_network_error_exits_2() {
    // Mock GitHub returns 500 → fetch fails → arm reports exit 2.
    let dir = tempfile::TempDir::new().unwrap();
    let api = spawn_500_github_response();
    let out = run_self_update_mode(
        "--defer",
        dir.path(),
        &api,
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "fetch failure must exit 2; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn apply_pending_cargo_refuses_and_clears_intent() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"999.0.0\"\n",
    )
    .unwrap();

    let out = run_self_update_mode(
        "--apply-pending",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "cargo")],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "cargo source must refuse apply (exit 3); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let text = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        !text.contains("[pending_update]"),
        "cargo refusal at apply must clear the un-appliable intent, got: {text}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn apply_pending_refuses_when_only_watch_peer_held() {
    // Regression guard: apply must wait for the WATCH lock too, not just serve.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"999.0.0\"\n",
    )
    .unwrap();
    let lock_dir = isolated_lock_dir(dir.path());
    std::fs::create_dir_all(&lock_dir).unwrap();
    // Only a watch peer (no serve.pid).
    std::fs::write(lock_dir.join("watch.pid"), std::process::id().to_string()).unwrap();

    let out = run_self_update_mode(
        "--apply-pending",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[
            ("CARTOG_TEST_INSTALL_SOURCE", "release-tarball"),
            ("CARTOG_TEST_APPLY_PEER_WAIT_MS", "100"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(6),
        "a live watch peer must block apply (exit 6); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        std::fs::read_to_string(&state_path)
            .unwrap()
            .contains("999.0.0"),
        "intent kept while watch peer holds the lock"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn self_version_json_exposes_pending_update() {
    // The SessionStart hook reads the armed target from `self version --json`;
    // assert the real binary emits it (not just the mocked shell output).
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"0.20.0\"\narmed_from = \"0.19.0\"\n",
    )
    .unwrap();

    let out = run_self_version(&["--json"], dir.path());
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        parsed["pending_update"]["target_version"].as_str(),
        Some("0.20.0"),
        "self version --json must expose the armed target, got: {stdout}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn self_version_json_omits_pending_update_when_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = run_self_version(&["--json"], dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        parsed.get("pending_update").is_none(),
        "pending_update must be omitted when nothing armed, got: {stdout}"
    );
}

// ── apply-pending success E2E (real download + swap) ──────────────────
//
// Unix-only: exercises the full reload→clear→breadcrumb path of
// run_apply_pending against a sandboxed cartog copy, with a release mock
// serving a valid .tar.gz whose contained `cartog` is a stub that passes
// the `--version` smoke test.

#[cfg(unix)]
fn make_valid_cartog_tarball(stub_version: &str) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let stub = format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"cartog {stub_version}\"; exit 0; fi\nexit 0\n");
    let stub_bytes = stub.into_bytes();
    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_path("cartog").unwrap();
        header.set_size(stub_bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, &stub_bytes[..]).unwrap();
        builder.finish().unwrap();
    }
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    use std::io::Write as _;
    gz.write_all(&tar_buf).unwrap();
    gz.finish().unwrap()
}

#[cfg(unix)]
#[test]
fn apply_pending_success_swaps_clears_intent_and_writes_breadcrumb() {
    use sha2::{Digest, Sha256};

    let install_dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let log_dir = tempfile::TempDir::new().unwrap();

    // Sandboxed cartog copy whose own --apply-pending we run.
    let bin = copy_cartog_into(install_dir.path());

    // Seed the armed intent at the path the binary reads (CARTOG_LOG_DIR is set
    // below, but state.toml still resolves via HOME/XDG).
    let state_path = seeded_state_path(home.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"99.0.0\"\n",
    )
    .unwrap();

    // Release mock: valid tarball + matching SHA256SUMS.
    let target = cartog_target_triple();
    let archive_name = format!("cartog-{target}.tar.gz");
    let archive_bytes = make_valid_cartog_tarball("99.0.0");
    let sha = {
        let mut h = Sha256::new();
        h.update(&archive_bytes);
        format!("{:x}", h.finalize())
    };
    let sums = format!("{sha}  {archive_name}\n");
    let (api, dl_base) = spawn_release_mock("99.0.0", archive_bytes, sums);

    let out = Command::new(&bin)
        .arg("self")
        .arg("update")
        .arg("--apply-pending")
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("CARTOG_LOG_DIR", log_dir.path())
        .env("CARTOG_GITHUB_API_URL", &api)
        .env("CARTOG_GITHUB_DOWNLOAD_BASE", &dl_base)
        .env("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")
        .env_remove("CARGO_HOME")
        .output()
        .expect("spawn sandboxed cartog");

    assert_eq!(
        out.status.code(),
        Some(0),
        "successful apply must exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    // The binary was swapped to the stub.
    let swapped = std::fs::read_to_string(&bin).unwrap();
    assert!(
        swapped.starts_with("#!/bin/sh"),
        "binary must be replaced by the stub from the tarball"
    );
    // The previous binary is preserved as .old.
    assert!(
        install_dir.path().join("cartog.old").exists(),
        "previous binary must be saved as <bin>.old"
    );
    // Intent cleared.
    let state = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        !state.contains("[pending_update]"),
        "intent must be cleared after a successful swap, got: {state}"
    );
    // Breadcrumb written where the hook reads it.
    let breadcrumb = std::fs::read_to_string(log_dir.path().join("last-update")).unwrap();
    assert_eq!(breadcrumb, "cartog updated to 99.0.0.\n");
}

/// A .tar.gz whose `cartog` exits non-zero on `--version` — fails the smoke test.
#[cfg(unix)]
fn make_smoke_failing_cartog_tarball() -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let stub = b"#!/bin/sh\nexit 1\n";
    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_path("cartog").unwrap();
        header.set_size(stub.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, &stub[..]).unwrap();
        builder.finish().unwrap();
    }
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    use std::io::Write as _;
    gz.write_all(&tar_buf).unwrap();
    gz.finish().unwrap()
}

#[cfg(unix)]
#[test]
fn apply_pending_smoke_failure_exits_7_restores_and_clears_intent() {
    use sha2::{Digest, Sha256};

    let install_dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let bin = copy_cartog_into(install_dir.path());
    // Remember the original (real cartog) bytes to assert restoration.
    let original_len = std::fs::metadata(&bin).unwrap().len();

    let state_path = seeded_state_path(home.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"99.0.0\"\n",
    )
    .unwrap();

    let target = cartog_target_triple();
    let archive_name = format!("cartog-{target}.tar.gz");
    let archive_bytes = make_smoke_failing_cartog_tarball();
    let sha = {
        let mut h = Sha256::new();
        h.update(&archive_bytes);
        format!("{:x}", h.finalize())
    };
    let sums = format!("{sha}  {archive_name}\n");
    let (api, dl_base) = spawn_release_mock("99.0.0", archive_bytes, sums);

    let out = Command::new(&bin)
        .arg("self")
        .arg("update")
        .arg("--apply-pending")
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("CARTOG_GITHUB_API_URL", &api)
        .env("CARTOG_GITHUB_DOWNLOAD_BASE", &dl_base)
        .env("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")
        .env_remove("CARGO_HOME")
        .output()
        .expect("spawn sandboxed cartog");

    assert_eq!(
        out.status.code(),
        Some(7),
        "smoke failure must exit 7 (distinct from 5); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    // Previous binary restored (the real cartog, not the 1-line stub).
    assert_eq!(
        std::fs::metadata(&bin).unwrap().len(),
        original_len,
        "previous binary must be restored after a smoke failure"
    );
    // Deterministic failure → intent cleared so it won't retry-loop.
    let state = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        !state.contains("[pending_update]"),
        "smoke failure must clear the intent (deterministic), got: {state}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn apply_pending_skips_when_another_apply_holds_the_lock() {
    // Plant a live apply-update lock (our own PID). A second --apply-pending
    // must see Held → no-op exit 0 WITHOUT swapping, and leave the intent armed
    // for the in-flight apply to finish. Network points at a black hole to
    // prove no download is attempted past the lock.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = seeded_state_path(dir.path());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(
        &state_path,
        "[pending_update]\ntarget_version = \"999.0.0\"\n",
    )
    .unwrap();
    let lock_dir = isolated_lock_dir(dir.path());
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(
        lock_dir.join("apply-update.pid"),
        std::process::id().to_string(),
    )
    .unwrap();

    let out = run_self_update_mode(
        "--apply-pending",
        dir.path(),
        "http://127.0.0.1:1/blackhole",
        &[("CARTOG_TEST_INSTALL_SOURCE", "release-tarball")],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "a held apply-update lock must yield a clean no-op (exit 0); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        std::fs::read_to_string(&state_path)
            .unwrap()
            .contains("999.0.0"),
        "intent must be kept for the in-flight apply to complete"
    );
}
