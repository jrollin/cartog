//! Integration tests for `cartog ide`.
//!
//! Each test runs the real binary in a `TempDir` with `HOME` / `XDG_CONFIG_HOME`
//! pointed at the same tempdir so user-scope writes are sandboxed and cannot
//! touch the developer machine.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cartog_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cartog"))
}

struct Sandbox {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let repo = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        // Mark the repo as a project root so cartog does not walk up out of the sandbox.
        fs::create_dir_all(repo.path().join(".git")).unwrap();
        Self { repo, home }
    }

    fn repo(&self) -> &Path {
        self.repo.path()
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        Command::new(cartog_bin())
            .args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("XDG_DATA_HOME", self.home.path().join(".local/share"))
            .env("XDG_STATE_HOME", self.home.path().join(".local/state"))
            .env_remove("CARGO_HOME")
            .output()
            .expect("failed to spawn cartog")
    }

    fn read(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.repo.path().join(rel)).ok()
    }
}

fn assert_success(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "cartog exited non-zero: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn ide_creates_project_mcp_json_and_cursor_dir() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["ide", "--scope", "project", "--yes"]);
    assert_success(&out);

    let mcp = sb.read(".mcp.json").expect(".mcp.json should exist");
    assert!(mcp.contains("\"cartog\""), "mcp body: {mcp}");
    assert!(mcp.contains("\"--watch\""), "expected --watch by default");

    let cursor = sb
        .read(".cursor/mcp.json")
        .expect("cursor mcp.json should exist");
    assert!(cursor.contains("\"cartog\""), "cursor body: {cursor}");
    assert!(
        !cursor.contains("--watch"),
        "cursor args should be plain serve"
    );
}

#[test]
fn ide_no_watch_drops_watch_for_claude_code() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["ide", "--scope", "project", "--yes", "--no-watch"]);
    assert_success(&out);
    let mcp = sb.read(".mcp.json").unwrap();
    assert!(
        !mcp.contains("--watch"),
        "mcp body should not contain --watch: {mcp}"
    );
}

#[test]
fn ide_is_idempotent() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["ide", "--scope", "project", "--yes"]));
    let first = sb.read(".mcp.json").unwrap();
    assert_success(&sb.cmd(&["ide", "--scope", "project", "--yes"]));
    let second = sb.read(".mcp.json").unwrap();
    assert_eq!(first, second, "expected no diff between runs");
}

#[test]
fn ide_preserves_existing_unrelated_server() {
    let sb = Sandbox::new();
    let existing = r#"{
  "mcpServers": {
    "other": {
      "command": "x",
      "args": ["a"]
    }
  }
}
"#;
    fs::write(sb.repo().join(".mcp.json"), existing).unwrap();
    assert_success(&sb.cmd(&["ide", "--client", "claude-code"]));
    let body = sb.read(".mcp.json").unwrap();
    assert!(body.contains("\"other\""), "other server dropped: {body}");
    assert!(body.contains("\"cartog\""), "cartog not added: {body}");
}

#[test]
fn ide_dry_run_does_not_write_files() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["ide", "--scope", "project", "--dry-run"]);
    assert_success(&out);
    assert!(sb.read(".mcp.json").is_none(), ".mcp.json should not exist");
    assert!(
        sb.read(".cursor/mcp.json").is_none(),
        ".cursor/mcp.json should not exist"
    );
}

#[test]
fn ide_filters_by_client_flag() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["ide", "--client", "cursor"]);
    assert_success(&out);
    assert!(sb.read(".cursor/mcp.json").is_some());
    assert!(
        sb.read(".mcp.json").is_none(),
        "claude-code should not be written"
    );
}

#[test]
fn ide_skipped_when_invalid_json_left_untouched() {
    let sb = Sandbox::new();
    let garbage = "{not json";
    fs::write(sb.repo().join(".mcp.json"), garbage).unwrap();
    let out = sb.cmd(&["ide", "--client", "claude-code", "--yes"]);
    assert_success(&out);
    let body = sb.read(".mcp.json").unwrap();
    assert_eq!(body, garbage, "invalid JSON file should not be modified");
}

#[test]
fn ide_skips_user_scope_when_parent_missing() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["ide", "--scope", "user", "--yes", "--json"]);
    assert_success(&out);
    let stdout = String::from_utf8(out.stdout).unwrap();
    // Every user-scope client should be skipped in a fresh sandbox.
    assert!(
        stdout.contains("\"skipped\""),
        "expected user-scope clients to be skipped: {stdout}"
    );
}

#[test]
fn ide_writes_user_scope_when_parent_exists() {
    let sb = Sandbox::new();
    // Seed the Zed config dir so it counts as "installed".
    let zed_dir = sb.home.path().join(".config/zed");
    fs::create_dir_all(&zed_dir).unwrap();
    let out = sb.cmd(&["ide", "--client", "zed", "--yes"]);
    assert_success(&out);
    let body = fs::read_to_string(zed_dir.join("settings.json")).unwrap();
    assert!(body.contains("\"context_servers\""), "zed body: {body}");
    assert!(body.contains("\"cartog\""), "zed body: {body}");
}

#[test]
fn ide_json_output_is_non_interactive() {
    let sb = Sandbox::new();
    // No --yes: --json should still bypass any prompts.
    let out = sb.cmd(&["--json", "ide", "--scope", "project"]);
    assert_success(&out);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("\"steps\""),
        "expected json report: {stdout}"
    );
}

#[test]
fn ide_writes_codex_toml_with_project_section() {
    let sb = Sandbox::new();
    // Seed ~/.codex so it counts as "installed".
    let codex_dir = sb.home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let out = sb.cmd(&["ide", "--client", "codex", "--yes"]);
    assert_success(&out);

    let body = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    // Section header must start with the per-project slug and end with an 8-hex hash.
    let header_re = regex_lite_match(&body, "[mcp_servers.cartog-", "]");
    assert!(
        header_re.is_some(),
        "expected [mcp_servers.cartog-<slug>-<hash>] header, got:\n{body}"
    );
    let header = header_re.unwrap();
    // header is "[mcp_servers.cartog-<slug>-<hash>]"; trim the closing bracket
    // before splitting so the last segment is just the hex hash.
    let inner = header.trim_start_matches('[').trim_end_matches(']');
    let hash_suffix = inner.rsplit('-').next().unwrap();
    assert_eq!(
        hash_suffix.len(),
        8,
        "expected 8-char hash suffix, got '{hash_suffix}' in {header}"
    );
    assert!(hash_suffix.chars().all(|c| c.is_ascii_hexdigit()));

    // Body must contain the canonical command + args.
    assert!(body.contains("command = \"cartog\""), "body: {body}");
    assert!(body.contains("args = [\"serve\"]"), "body: {body}");
}

#[test]
fn ide_codex_toml_preserves_existing_sections_and_comments() {
    let sb = Sandbox::new();
    let codex_dir = sb.home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let original = "# user-managed\n\
        [features]\n\
        codex_hooks = true\n\
        \n\
        [mcp_servers.other]\n\
        command = \"other\"\n\
        args = [\"--keep\"]\n";
    fs::write(codex_dir.join("config.toml"), original).unwrap();

    let out = sb.cmd(&["ide", "--client", "codex", "--yes"]);
    assert_success(&out);

    let body = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(body.contains("# user-managed"), "comment dropped: {body}");
    assert!(
        body.contains("codex_hooks = true"),
        "feature dropped: {body}"
    );
    assert!(
        body.contains("[mcp_servers.other]"),
        "other section dropped: {body}"
    );
    assert!(
        body.contains("[mcp_servers.cartog-"),
        "cartog section not added: {body}"
    );
}

#[test]
fn ide_codex_idempotent() {
    let sb = Sandbox::new();
    let codex_dir = sb.home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();

    assert_success(&sb.cmd(&["ide", "--client", "codex", "--yes"]));
    let first = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert_success(&sb.cmd(&["ide", "--client", "codex", "--yes"]));
    let second = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert_eq!(first, second, "re-run should produce identical bytes");
}

/// Minimal substring locator: returns the first occurrence of `start..end`
/// inclusive of both markers. Used to extract the Codex TOML section header
/// without pulling in a regex dep.
fn regex_lite_match(haystack: &str, start: &str, end: &str) -> Option<String> {
    let s = haystack.find(start)?;
    let rest = &haystack[s..];
    let e = rest.find(end)?;
    Some(rest[..=e].to_string())
}
