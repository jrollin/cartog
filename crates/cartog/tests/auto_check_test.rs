//! Integration smoke tests for the auto-check predicate and background spawn.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant, SystemTime};

use cartog::auto_check::{
    maybe_spawn, run_check_once, should_check, spawn_check, CheckMode, CommandKind,
    MaybeSpawnInput, ShouldCheckInput,
};
use cartog::state::State;

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_067_200) // 2024-01-01T00:00:00Z
}

#[test]
fn should_check_first_run_with_tty_via_lib_facade() {
    let input = ShouldCheckInput {
        command_kind: CommandKind::Quick,
        stdout_is_tty: true,
        disabled_env: false,
        mode: CheckMode::Daily,
        last_check: None,
        now: now(),
    };
    assert!(should_check(&input));
}

#[test]
fn should_check_serve_command_blocked_via_lib_facade() {
    let input = ShouldCheckInput {
        command_kind: CommandKind::LongLived,
        stdout_is_tty: true,
        disabled_env: false,
        mode: CheckMode::Always,
        last_check: None,
        now: now(),
    };
    assert!(!should_check(&input), "serve/watch must never auto-check");
}

// ── spawn_check / run_check_once ──────────────────────────────────────

/// Localhost HTTP server serving one canned 200 OK then exiting.
fn spawn_canned_github_response(json_body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = json_body.as_bytes();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/")
}

/// Canned response that stalls `server_delay` before replying — lets a test
/// prove `spawn_check` returns before the network call could complete.
fn spawn_delayed_github_response(json_body: String, server_delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            std::thread::sleep(server_delay);
            let body = json_body.as_bytes();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/")
}

fn wait_for<F: FnMut() -> bool>(mut pred: F, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pred()
}

#[test]
fn spawn_check_run_once_writes_state_when_outdated() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let url = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());

    run_check_once(&url, Some(&state_path), env!("CARGO_PKG_VERSION")).expect("check ok");

    let state = State::load_from(&state_path);
    assert_eq!(
        state.last_known_latest.as_deref(),
        Some("999.0.0"),
        "state should record the fetched latest version"
    );
    assert!(state.last_known_outdated, "v999.0.0 must be newer");
    assert!(
        state.last_update_check.is_some(),
        "last_update_check must be populated"
    );
}

#[test]
fn spawn_check_run_once_marks_not_outdated_when_current() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let url = spawn_canned_github_response(format!(
        r#"{{"tag_name":"v{}"}}"#,
        env!("CARGO_PKG_VERSION")
    ));

    run_check_once(&url, Some(&state_path), env!("CARGO_PKG_VERSION")).expect("check ok");

    let state = State::load_from(&state_path);
    assert_eq!(
        state.last_known_latest.as_deref(),
        Some(env!("CARGO_PKG_VERSION")),
    );
    assert!(
        !state.last_known_outdated,
        "running version == latest must not be marked outdated"
    );
}

#[test]
fn spawn_check_returns_immediately_without_blocking() {
    // Server stalls 1s; a non-blocking spawn must return before that. The
    // full-second margin is load-proof, unlike a fixed sub-50ms ceiling.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let server_delay = Duration::from_secs(1);
    let url = spawn_delayed_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string(), server_delay);

    let start = Instant::now();
    spawn_check(
        url,
        Some(state_path.clone()),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    let spawn_elapsed = start.elapsed();

    // State absent on return proves the check did not run synchronously.
    assert!(
        !state_path.exists(),
        "spawn_check ran the check synchronously: state.toml already exists on return"
    );
    assert!(
        spawn_elapsed < server_delay,
        "spawn_check returned in {spawn_elapsed:?}, after the {server_delay:?} server delay; \
         it must return before the network call could complete"
    );

    // The worker should eventually finish and write the state file.
    assert!(
        wait_for(|| state_path.exists(), Duration::from_secs(5)),
        "worker thread should have produced state.toml after the server replied"
    );
}

#[test]
fn spawn_check_run_once_skips_state_save_when_path_is_none() {
    let url = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());
    // Should still succeed; just skips the state write.
    run_check_once(&url, None, env!("CARGO_PKG_VERSION")).expect("check with no state path");
}

// ── main_epilogue (maybe_spawn) ───────────────────────────────────────

fn epilogue_input<'a>(state_path: &'a std::path::Path, api_url: &'a str) -> MaybeSpawnInput<'a> {
    MaybeSpawnInput {
        command_kind: CommandKind::Quick,
        stdout_is_tty: true,
        disabled_env: None,
        mode_env: None,
        state_path: Some(state_path),
        api_url,
        current_version: env!("CARGO_PKG_VERSION"),
        now: now(),
    }
}

#[test]
fn main_epilogue_spawns_check_when_signals_allow() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let url = spawn_canned_github_response(r#"{"tag_name":"v999.0.0"}"#.to_string());

    let spawned = maybe_spawn(epilogue_input(&state_path, &url));

    assert!(spawned, "all signals agree: a check thread must be spawned");
    assert!(
        wait_for(|| state_path.exists(), Duration::from_secs(2)),
        "background thread should have produced state.toml"
    );
}

#[test]
fn main_epilogue_skips_for_long_lived_command() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.command_kind = CommandKind::LongLived;

    let spawned = maybe_spawn(input);

    assert!(!spawned, "serve/watch must never auto-check");
    assert!(
        !state_path.exists(),
        "no state file should be written when no spawn happens"
    );
}

#[test]
fn main_epilogue_skips_when_disabled_env_set() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.disabled_env = Some("1");

    let spawned = maybe_spawn(input);

    assert!(!spawned, "CARTOG_NO_UPDATE_CHECK=1 must suppress the spawn");
    assert!(!state_path.exists());
}

#[test]
fn main_epilogue_skips_when_stdout_is_not_tty() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.stdout_is_tty = false;

    let spawned = maybe_spawn(input);

    assert!(
        !spawned,
        "non-TTY stdout must suppress the spawn (CI, pipes)"
    );
    assert!(!state_path.exists());
}

#[test]
fn main_epilogue_skips_when_mode_is_never() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.mode_env = Some("never");

    let spawned = maybe_spawn(input);

    assert!(
        !spawned,
        "CARTOG_UPDATE_CHECK=never must suppress the spawn"
    );
    assert!(!state_path.exists());
}

#[test]
fn main_epilogue_skips_when_state_path_unavailable() {
    let mut input = epilogue_input(
        std::path::Path::new("/dev/null/unused"),
        "http://127.0.0.1:1/",
    );
    input.state_path = None;

    let spawned = maybe_spawn(input);

    assert!(!spawned, "no state path → no spawn (nothing to persist)");
}

#[test]
fn main_epilogue_skips_when_recent_check_within_24h() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    // last_update_check = 1h before `now` (2024-01-01T00:00:00Z fixed in helper).
    State {
        last_update_check: Some("2023-12-31T23:00:00Z".to_string()),
        ..Default::default()
    }
    .save_to(&state_path)
    .expect("seed state");
    let original = std::fs::read_to_string(&state_path).expect("read seeded state");

    let spawned = maybe_spawn(epilogue_input(&state_path, "http://127.0.0.1:1/"));

    assert!(
        !spawned,
        "daily mode must respect the 24h interval — no spawn so soon"
    );
    let after = std::fs::read_to_string(&state_path).expect("re-read state");
    assert_eq!(after, original, "no spawn must not mutate state");
}

#[test]
fn spawn_check_run_once_rejects_prerelease_tags() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let url = spawn_canned_github_response(r#"{"tag_name":"v0.14.0-rc.1"}"#.to_string());

    let err = run_check_once(&url, Some(&state_path), env!("CARGO_PKG_VERSION"))
        .expect_err("prerelease must not be treated as eligible");
    assert!(
        format!("{err}").contains("parse"),
        "expected a parse error, got: {err}"
    );
    assert!(
        !state_path.exists(),
        "state file must not be written on parse failure"
    );
}

// ── T-21: combined suppression signals ────────────────────────────────
//
// The single-signal `main_epilogue_skips_*` tests above cover each gate
// in isolation. These verify the contract still holds when *multiple*
// suppression signals are active simultaneously (e.g. non-TTY AND
// disabled, or LongLived AND a fresh state file). A regression that
// reorders the gating short-circuits would pass the single-signal
// tests but could fail one of these.

#[test]
fn suppression_holds_when_disabled_and_long_lived_combined() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.disabled_env = Some("1");
    input.command_kind = CommandKind::LongLived;
    assert!(
        !maybe_spawn(input),
        "any single signal suppresses; both must too"
    );
    assert!(!state_path.exists());
}

#[test]
fn suppression_holds_when_non_tty_and_always_mode_combined() {
    // CARTOG_UPDATE_CHECK=always overrides the daily interval but must
    // NOT override the TTY gate — interactive output is the whole point.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.stdout_is_tty = false;
    input.mode_env = Some("always");
    assert!(
        !maybe_spawn(input),
        "non-TTY must suppress even with mode=always (interactive-only contract)"
    );
}

#[test]
fn suppression_holds_when_long_lived_and_fresh_state() {
    // Even with an ancient last_check (24h gate would fire on its own), a
    // long-lived command (serve/watch) must never trigger an auto-check.
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    State {
        last_update_check: Some("2020-01-01T00:00:00Z".to_string()),
        ..Default::default()
    }
    .save_to(&state_path)
    .expect("seed state");
    let original = std::fs::read_to_string(&state_path).expect("read seeded state");

    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.command_kind = CommandKind::LongLived;
    assert!(!maybe_spawn(input), "long-lived must beat the 24h interval");
    let after = std::fs::read_to_string(&state_path).expect("re-read state");
    assert_eq!(after, original, "no spawn must not mutate state");
}

#[test]
fn suppression_priority_disabled_env_beats_always_mode() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.toml");
    let mut input = epilogue_input(&state_path, "http://127.0.0.1:1/");
    input.disabled_env = Some("1");
    input.mode_env = Some("always");
    assert!(
        !maybe_spawn(input),
        "CARTOG_NO_UPDATE_CHECK=1 must beat CARTOG_UPDATE_CHECK=always"
    );
}
