//! End-to-end: does the Claude Code plugin's pin actually reach `cartog serve`?
//!
//! The bug these guard against was a *wiring* bug, not a logic bug: the MCP
//! server read `CLAUDE_PLUGIN_ROOT` from its own environment, but Claude Code
//! expands that variable inside `plugin.json` without exporting it, so the
//! manifest was never found and `cartog_update` armed "latest" instead of the
//! pin. Unit tests over the pure helpers cannot catch that: they call the
//! helpers directly, with the arguments already correct. These drive the real
//! binary over real stdio instead, so a swapped argument, a dropped `env`
//! block, or a lost `ServerOptions` field fails here.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn cartog_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cartog"))
}

/// A cartog-configured project plus a plugin manifest pinning `pin`.
fn fixture(pin: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = dir.path();
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    // A `.cartog.toml` is the consent signal, so the server is not degraded
    // (a degraded server suppresses every drift surface by design).
    std::fs::write(project.join(".cartog.toml"), "").expect("write config");
    // Manifest laid out as a plugin root, so `CLAUDE_PLUGIN_ROOT` resolves it.
    let plugin_root = root.join("plugin");
    std::fs::create_dir_all(plugin_root.join(".claude-plugin")).expect("plugin dir");
    std::fs::write(
        plugin_root.join(".claude-plugin").join("plugin.json"),
        format!(r#"{{"name":"cartog","version":"{pin}"}}"#),
    )
    .expect("write manifest");
    (dir, project, plugin_root)
}

/// Run one `initialize` handshake against `cartog serve` and return the
/// server's `instructions` string.
fn serve_instructions(project: &Path, envs: &[(&str, &str)], home: &Path) -> String {
    let mut cmd = Command::new(cartog_bin());
    cmd.arg("serve")
        .current_dir(project)
        // Single-writer election off: these spawn a server per test and must not
        // attach read-only to a peer left by a sibling test.
        .env("CARTOG_SINGLE_WRITER", "0")
        // User-global isolation: the registry, state dir and update state all
        // key off HOME/XDG, and a leak would write the developer's own files.
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("CARTOG_REGISTRY", "")
        .env_remove("CARTOG_PLUGIN_JSON")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("CARTOG_NO_UPDATE_CHECK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Captured, not discarded: when the handshake yields nothing the server's
        // own startup error is the only thing that explains why.
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn cartog serve");

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pin-test","version":"0"}}}"#;
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{init}").expect("write initialize");
        stdin.flush().expect("flush");
    }
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("read initialize response");
    let mut stderr = String::new();
    let _ = child.kill();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }
    let _ = child.wait();

    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|e| {
        panic!("initialize response was not JSON ({e}): {line:?}\nserver stderr: {stderr}")
    });
    parsed["result"]["instructions"]
        .as_str()
        .unwrap_or_else(|| panic!("no instructions in response: {line}"))
        .to_string()
}

/// The pin reaches the server through `CLAUDE_PLUGIN_ROOT`, the variable the
/// plugin manifest forwards via `mcpServers.cartog.env`. A regression here
/// means the server is blind to the pin again.
#[test]
fn a_pin_behind_the_binary_reaches_serve_via_claude_plugin_root() {
    let (dir, project, plugin_root) = fixture("99.9.9");
    let text = serve_instructions(
        &project,
        &[("CLAUDE_PLUGIN_ROOT", plugin_root.to_str().unwrap())],
        dir.path(),
    );
    assert!(
        text.contains("99.9.9"),
        "instructions must name the pin the plugin root resolved to: {text}"
    );
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "instructions must name the running version: {text}"
    );
}

/// The explicit-path override resolves the same manifest.
#[test]
fn a_pin_behind_the_binary_reaches_serve_via_cartog_plugin_json() {
    let (dir, project, plugin_root) = fixture("99.9.9");
    let manifest = plugin_root.join(".claude-plugin").join("plugin.json");
    let text = serve_instructions(
        &project,
        &[("CARTOG_PLUGIN_JSON", manifest.to_str().unwrap())],
        dir.path(),
    );
    assert!(
        text.contains("99.9.9"),
        "instructions must name the pin the explicit path resolved to: {text}"
    );
}

/// A pin at or below the running version is not drift, so nothing is said.
#[test]
fn a_pin_the_binary_already_satisfies_says_nothing() {
    let (dir, project, plugin_root) = fixture("0.0.1");
    let text = serve_instructions(
        &project,
        &[("CLAUDE_PLUGIN_ROOT", plugin_root.to_str().unwrap())],
        dir.path(),
    );
    assert!(
        !text.contains("0.0.1") && !text.contains("NOTE:"),
        "a binary ahead of the pin must not report drift: {text}"
    );
}

/// `CARTOG_NO_UPDATE_CHECK` silences the MCP drift surfaces, the same way it
/// silences `cartog doctor`'s version row and the SessionStart notice. Without
/// this the documented kill switch misses the loudest surface of the three:
/// the instructions tell the model to raise the drift with the user.
#[test]
fn the_update_check_kill_switch_silences_the_drift_sentence() {
    let (dir, project, plugin_root) = fixture("99.9.9");
    let text = serve_instructions(
        &project,
        &[
            ("CLAUDE_PLUGIN_ROOT", plugin_root.to_str().unwrap()),
            ("CARTOG_NO_UPDATE_CHECK", "1"),
        ],
        dir.path(),
    );
    assert!(
        !text.contains("99.9.9") && !text.contains("NOTE:"),
        "CARTOG_NO_UPDATE_CHECK must silence the drift sentence: {text}"
    );
}

/// No manifest anywhere is the ordinary non-plugin case (manual MCP wiring).
#[test]
fn no_manifest_leaves_the_instructions_untouched() {
    let (dir, project, _plugin_root) = fixture("99.9.9");
    let text = serve_instructions(&project, &[], dir.path());
    assert!(
        !text.contains("NOTE:"),
        "a server with no discoverable pin must not report drift: {text}"
    );
}
