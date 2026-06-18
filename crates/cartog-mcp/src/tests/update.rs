//! Tests for cartog_update: arm-output reshaping and plugin-pin discovery.

use crate::*;

// ── parse_arm_output tests (cartog_update envelope reshaping) ──
//
// Unix-only: these construct a real `ExitStatus` by spawning `true`/`sh`.
// The parse logic itself is platform-independent.

/// Build an `Output` with the given stdout/stderr and a zero exit status.
/// parse_arm_output reads `status` only in the no-output branch, so a
/// success-shaped status is fine for the parse-path cases.
#[cfg(unix)]
fn output_ok(stdout: &str, stderr: &str) -> std::process::Output {
    std::process::Output {
        status: std::process::Command::new("true")
            .status()
            .expect("run true"),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn mcp_compact_defaults_on_and_parses_opt_out() {
    let _g = test_validate_call_counter::SERIAL.blocking_lock();
    let prev = std::env::var_os("CARTOG_MCP_COMPACT");

    std::env::remove_var("CARTOG_MCP_COMPACT");
    assert!(mcp_compact(), "compact is the default when unset");

    for off in ["0", "false", "no", "off", "OFF", " false "] {
        std::env::set_var("CARTOG_MCP_COMPACT", off);
        assert!(!mcp_compact(), "{off:?} must disable compact");
    }
    for on in ["1", "true", "yes", "anything"] {
        std::env::set_var("CARTOG_MCP_COMPACT", on);
        assert!(mcp_compact(), "{on:?} must keep compact on");
    }

    match prev {
        Some(v) => std::env::set_var("CARTOG_MCP_COMPACT", v),
        None => std::env::remove_var("CARTOG_MCP_COMPACT"),
    }
}

#[test]
fn rag_snippet_bounds_body_length() {
    // The MCP rag_search default snips bodies via rag::search::snippet.
    let long = "y".repeat(rag::search::SNIPPET_MAX_BYTES * 3);
    let s = rag::search::snippet(&long);
    assert!(s.len() <= rag::search::SNIPPET_MAX_BYTES);
}

// ── discover_plugin_pin tests (cartog_update arms the pin) ──
// Serialized via SERIAL because they mutate process-global env vars.

#[test]
fn discover_plugin_pin_reads_explicit_manifest() {
    let _g = test_validate_call_counter::SERIAL.blocking_lock();
    let prev_json = std::env::var_os("CARTOG_PLUGIN_JSON");
    let prev_root = std::env::var_os("CLAUDE_PLUGIN_ROOT");
    std::env::remove_var("CLAUDE_PLUGIN_ROOT");

    let dir = tempfile::TempDir::new().unwrap();
    let manifest = dir.path().join("plugin.json");
    std::fs::write(&manifest, r#"{"name":"cartog","version":"0.20.0"}"#).unwrap();
    std::env::set_var("CARTOG_PLUGIN_JSON", &manifest);
    assert_eq!(discover_plugin_pin().as_deref(), Some("0.20.0"));

    // Malformed (non-bare) version → None (fall back to latest).
    std::fs::write(&manifest, r#"{"version":"v0.20.0"}"#).unwrap();
    assert_eq!(
        discover_plugin_pin(),
        None,
        "non-bare-semver pin must be rejected"
    );

    // No manifest discoverable → None.
    std::env::remove_var("CARTOG_PLUGIN_JSON");
    assert_eq!(discover_plugin_pin(), None);

    match prev_json {
        Some(v) => std::env::set_var("CARTOG_PLUGIN_JSON", v),
        None => std::env::remove_var("CARTOG_PLUGIN_JSON"),
    }
    if let Some(v) = prev_root {
        std::env::set_var("CLAUDE_PLUGIN_ROOT", v);
    }
}

#[test]
fn discover_plugin_pin_reads_claude_plugin_root() {
    let _g = test_validate_call_counter::SERIAL.blocking_lock();
    let prev_json = std::env::var_os("CARTOG_PLUGIN_JSON");
    let prev_root = std::env::var_os("CLAUDE_PLUGIN_ROOT");
    std::env::remove_var("CARTOG_PLUGIN_JSON");

    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
    std::fs::write(
        dir.path().join(".claude-plugin").join("plugin.json"),
        r#"{"version":"0.21.0"}"#,
    )
    .unwrap();
    std::env::set_var("CLAUDE_PLUGIN_ROOT", dir.path());
    assert_eq!(discover_plugin_pin().as_deref(), Some("0.21.0"));

    match prev_json {
        Some(v) => std::env::set_var("CARTOG_PLUGIN_JSON", v),
        None => std::env::remove_var("CARTOG_PLUGIN_JSON"),
    }
    match prev_root {
        Some(v) => std::env::set_var("CLAUDE_PLUGIN_ROOT", v),
        None => std::env::remove_var("CLAUDE_PLUGIN_ROOT"),
    }
}

#[cfg(unix)]
#[test]
fn parse_arm_output_armed_maps_fields() {
    let r = parse_arm_output(&output_ok(
        r#"{"status":"armed","current":"0.19.0","target":"0.20.0","apply":"session-end-or-restart"}"#,
        "",
    ));
    assert_eq!(r.status, "armed");
    assert_eq!(r.target.as_deref(), Some("0.20.0"));
    assert_eq!(r.apply, "session-end-or-restart");
    assert!(r.message.contains("session ends"));
}

#[cfg(unix)]
#[test]
fn parse_arm_output_up_to_date_maps() {
    let r = parse_arm_output(&output_ok(
        r#"{"status":"up-to-date","current":"0.19.0","latest":"0.19.0"}"#,
        "",
    ));
    assert_eq!(r.status, "up-to-date");
    assert_eq!(r.apply, "n/a");
    assert!(r.target.is_none());
}

#[cfg(unix)]
#[test]
fn parse_arm_output_cargo_maps_to_cargo_refused() {
    let r = parse_arm_output(&output_ok(
        r#"{"status":"cargo","message":"cartog was installed via cargo. Run `cargo install cartog --force` to upgrade."}"#,
        "",
    ));
    assert_eq!(r.status, "cargo-refused");
    assert!(r.message.contains("cargo install cartog --force"));
}

#[cfg(unix)]
#[test]
fn parse_arm_output_foreign_status_echoes_message_as_error() {
    let r = parse_arm_output(&output_ok(
        r#"{"status":"fetch-failed","message":"GitHub API returned status 500"}"#,
        "",
    ));
    assert_eq!(r.status, "error");
    assert!(r.message.contains("GitHub API returned status 500"));
}

#[cfg(unix)]
#[test]
fn parse_arm_output_skips_log_lines_before_json() {
    // A daily-update-check hint or any noise line before the JSON must not
    // break the parse — the reverse scan finds the real object.
    let r = parse_arm_output(&output_ok(
        "cartog: a new version is available\n{\"status\":\"armed\",\"target\":\"0.20.0\"}\n",
        "",
    ));
    assert_eq!(r.status, "armed");
    assert_eq!(r.target.as_deref(), Some("0.20.0"));
}

#[cfg(unix)]
#[test]
fn parse_arm_output_ignores_trailing_bare_scalar() {
    // A trailing bare scalar that is valid JSON must NOT be picked over the
    // real status object earlier in the stream.
    let r = parse_arm_output(&output_ok(
        "{\"status\":\"armed\",\"target\":\"1.2.3\"}\n99\n",
        "",
    ));
    assert_eq!(r.status, "armed", "bare scalar must be skipped");
    assert_eq!(r.target.as_deref(), Some("1.2.3"));
}

#[cfg(unix)]
#[test]
fn parse_arm_output_empty_output_names_exit_and_next_action() {
    // Child produced nothing (e.g. SIGKILL). The message must still give a
    // next step rather than a dead-end empty string.
    let out = std::process::Output {
        status: std::process::Command::new("sh")
            .args(["-c", "exit 9"])
            .status()
            .expect("run sh"),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    let r = parse_arm_output(&out);
    assert_eq!(r.status, "error");
    assert!(
        r.message.contains("exit 9"),
        "must name the exit code: {}",
        r.message
    );
    assert!(
        r.message.contains("--defer") || r.message.contains("/cartog-install"),
        "must name a next action: {}",
        r.message
    );
}

#[cfg(unix)]
#[test]
fn parse_arm_output_nonjson_stderr_surfaces_detail() {
    let r = parse_arm_output(&output_ok("", "boom: something broke"));
    assert_eq!(r.status, "error");
    assert!(r.message.contains("boom: something broke"));
}
