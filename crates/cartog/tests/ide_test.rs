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
        self.cmd_with_bins(args, &[])
    }

    /// Run with a PATH holding only empty stand-ins for `bins`. A controlled PATH
    /// keeps client detection independent of what the developer has installed.
    fn cmd_with_bins(&self, args: &[&str], bins: &[&str]) -> std::process::Output {
        let bin_dir = self.home.path().join("fake-bin");
        fs::create_dir_all(&bin_dir).unwrap();
        for bin in bins {
            fs::write(bin_dir.join(bin), "").unwrap();
        }
        Command::new(cartog_bin())
            .args(args)
            .current_dir(self.repo.path())
            .env("PATH", &bin_dir)
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
        cursor.contains("--watch"),
        "cursor now gets --watch by default too: {cursor}"
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

    // Body must contain the canonical command + args (--watch now on for all clients).
    assert!(body.contains("command = \"cartog\""), "body: {body}");
    assert!(
        body.contains("args = [\"serve\", \"--watch\"]"),
        "body: {body}"
    );
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

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn install_named_new_clients_writes_their_project_files() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["install", "qwen", "kilo", "roo", "--scope", "project"]));

    let qwen = sb.read(".qwen/settings.json").expect(".qwen/settings.json");
    assert!(qwen.contains("\"mcpServers\""), "qwen: {qwen}");
    let kilo = sb.read(".kilo/kilo.json").expect(".kilo/kilo.json");
    assert!(
        kilo.contains("\"mcp\"") && kilo.contains("\"local\""),
        "kilo: {kilo}"
    );
    let roo = sb.read(".roo/mcp.json").expect(".roo/mcp.json");
    assert!(roo.contains("\"disabled\": false"), "roo: {roo}");
}

#[test]
fn install_cline_cli_keeps_other_servers_and_approvals() {
    let sb = Sandbox::new();
    let settings = sb.home.path().join(".cline/data/settings");
    fs::create_dir_all(&settings).unwrap();
    let cfg = settings.join("cline_mcp_settings.json");
    fs::write(
        &cfg,
        r#"{"mcpServers": {
            "other": {"command": "x", "args": []},
            "cartog": {"command": "cartog", "args": ["serve"], "alwaysAllow": ["cartog_search"]}
        }}"#,
    )
    .unwrap();

    assert_success(&sb.cmd(&["install", "cline-cli"]));
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["other"]["command"], "x");
    assert_eq!(
        v["mcpServers"]["cartog"]["args"],
        serde_json::json!(["serve", "--watch"])
    );
    assert_eq!(
        v["mcpServers"]["cartog"]["alwaysAllow"],
        serde_json::json!(["cartog_search"])
    );
}

#[test]
fn install_kilo_leaves_a_commented_jsonc_byte_identical() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.repo().join(".kilo")).unwrap();
    let original = "// team settings\n{ \"model\": \"x\" }\n";
    fs::write(sb.repo().join(".kilo/kilo.jsonc"), original).unwrap();

    let out = sb.cmd(&["install", "kilo", "--scope", "project"]);
    assert_success(&out);
    assert_eq!(sb.read(".kilo/kilo.jsonc").unwrap(), original);
    assert!(sb.read(".kilo/kilo.json").is_none(), "no sibling kilo.json");
    assert!(stdout(&out).contains("JSONC"), "stdout: {}", stdout(&out));
}

#[test]
fn install_without_client_skips_a_skills_only_qwen_home() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.home.path().join(".qwen/skills")).unwrap();
    assert_success(&sb.cmd(&["install"]));
    assert!(
        !sb.home.path().join(".qwen/settings.json").exists(),
        "a skills-only ~/.qwen is not a Qwen install"
    );
}

#[test]
fn ide_project_scope_in_a_bare_repo_seeds_no_new_client_folders() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["ide", "--scope", "project", "--yes"]));
    assert!(
        sb.read(".mcp.json").is_some(),
        "original rows still written"
    );
    for dir in [".qwen", ".roo", ".kilo"] {
        assert!(
            !sb.repo().join(dir).exists(),
            "{dir} created in a bare repo"
        );
    }
}

#[test]
fn dry_run_writes_nothing_for_new_clients() {
    let sb = Sandbox::new();
    let out = sb.cmd(&[
        "install",
        "qwen",
        "kilo",
        "roo",
        "cline-cli",
        "copilot-cli",
        "--dry-run",
    ]);
    assert_success(&out);
    for dir in [".qwen", ".roo", ".kilo"] {
        assert!(!sb.repo().join(dir).exists(), "{dir} written on --dry-run");
    }
    for rel in [".qwen", ".cline", ".copilot", ".config/kilo"] {
        assert!(
            !sb.home.path().join(rel).exists(),
            "~/{rel} written on --dry-run"
        );
    }
}

#[test]
fn install_copilot_cli_at_project_scope_errors_naming_client_and_scope() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["install", "copilot-cli", "--scope", "project"]);
    assert!(!out.status.success(), "expected failure");
    let err = stderr(&out);
    assert!(
        err.contains("copilot-cli") && err.contains("project"),
        "stderr: {err}"
    );
}

#[test]
fn ide_scope_mismatch_error_does_not_claim_cursor_is_project_only() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["ide", "--client", "codex", "--scope", "project"]);
    assert!(!out.status.success(), "expected failure");
    let err = stderr(&out);
    assert!(err.contains("codex"), "stderr: {err}");
    assert!(!err.contains("project-scoped only"), "stale hint: {err}");
}

#[test]
fn install_wires_qwen_user_config_when_qwen_is_on_path() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd_with_bins(&["install", "--scope", "user"], &["qwen"]));
    assert!(sb.home.path().join(".qwen/settings.json").exists());
}

#[test]
fn ide_project_scope_writes_the_qwen_project_file_when_qwen_is_on_path() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd_with_bins(&["ide", "--scope", "project", "--yes"], &["qwen"]));
    assert!(sb.read(".qwen/settings.json").is_some());
    assert!(!sb.repo().join(".roo").exists());
    assert!(!sb.repo().join(".kilo").exists());
}

#[test]
fn install_wires_cursor_user_config_when_the_cursor_app_dir_exists() {
    let sb = Sandbox::new();
    // Cursor's app data dir: macOS and Linux locations under the sandbox home.
    fs::create_dir_all(sb.home.path().join("Library/Application Support/Cursor")).unwrap();
    fs::create_dir_all(sb.home.path().join(".config/Cursor")).unwrap();
    assert_success(&sb.cmd(&["install", "--scope", "user"]));
    assert!(sb.home.path().join(".cursor/mcp.json").exists());
}

#[test]
fn install_ignores_a_lone_copilot_binary() {
    // AWS Copilot CLI ships a `copilot` binary too.
    let sb = Sandbox::new();
    assert_success(&sb.cmd_with_bins(&["install", "--scope", "user"], &["copilot"]));
    assert!(!sb.home.path().join(".copilot/mcp-config.json").exists());
}

fn json_file(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn tree(root: &Path) -> std::collections::BTreeSet<PathBuf> {
    let mut out = std::collections::BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.insert(path);
        }
    }
    out
}

#[test]
fn uninstall_after_install_removes_cartog_and_keeps_files_and_other_servers() {
    let sb = Sandbox::new();
    fs::write(
        sb.repo().join(".mcp.json"),
        r#"{"mcpServers": {"other": {"command": "x"}}}"#,
    )
    .unwrap();
    assert_success(&sb.cmd(&["install", "claude-code", "qwen", "--scope", "project"]));
    let out = sb.cmd(&["uninstall", "--scope", "project"]);
    assert_success(&out);
    assert!(stdout(&out).contains(" 2 removed,"), "{}", stdout(&out));

    let mcp = json_file(&sb.repo().join(".mcp.json"));
    assert_eq!(
        mcp["mcpServers"],
        serde_json::json!({"other": {"command": "x"}})
    );
    let qwen = json_file(&sb.repo().join(".qwen/settings.json"));
    assert_eq!(qwen["mcpServers"], serde_json::json!({}));
}

#[test]
fn uninstall_without_client_removes_a_wired_config_of_an_undetected_client() {
    let sb = Sandbox::new();
    let cfg = sb.home.path().join(".qwen/settings.json");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(&cfg, r#"{"mcpServers": {"cartog": {"command": "cartog"}}}"#).unwrap();
    assert_success(&sb.cmd(&["uninstall"]));
    assert_eq!(json_file(&cfg)["mcpServers"], serde_json::json!({}));
}

#[test]
fn uninstall_codex_keeps_another_projects_section() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.home.path().join(".codex")).unwrap();
    assert_success(&sb.cmd(&["install", "codex"]));
    let cfg = sb.home.path().join(".codex/config.toml");
    let mut body = fs::read_to_string(&cfg).unwrap();
    body.push_str("\n[mcp_servers.cartog-other-ffff0000]\ncommand = \"cartog\"\n");
    fs::write(&cfg, body).unwrap();

    assert_success(&sb.cmd(&["uninstall", "codex"]));
    let after = fs::read_to_string(&cfg).unwrap();
    assert_eq!(after.matches("[mcp_servers.cartog-").count(), 1, "{after}");
    assert!(after.contains("cartog-other-ffff0000"), "{after}");
}

#[test]
fn uninstall_dry_run_leaves_files_unchanged() {
    let sb = Sandbox::new();
    let wired = r#"{"mcpServers": {"cartog": {"command": "cartog"}}}"#;
    fs::write(sb.repo().join(".mcp.json"), wired).unwrap();
    let out = sb.cmd(&["uninstall", "--dry-run"]);
    assert_success(&out);
    assert_eq!(sb.read(".mcp.json").unwrap(), wired);
    assert!(stdout(&out).contains("would remove"), "{}", stdout(&out));
}

#[test]
fn uninstall_skips_a_commented_jsonc_and_leaves_it_byte_identical() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.repo().join(".kilo")).unwrap();
    let original = "// team\n{\"mcp\": {\"cartog\": {\"type\": \"local\"}}}\n";
    fs::write(sb.repo().join(".kilo/kilo.jsonc"), original).unwrap();
    let out = sb.cmd(&["uninstall", "kilo", "--scope", "project"]);
    assert_success(&out);
    assert_eq!(sb.read(".kilo/kilo.jsonc").unwrap(), original);
    assert!(stdout(&out).contains("not modified"), "{}", stdout(&out));
}

#[test]
fn ide_remove_yes_removes_every_wired_entry() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["install", "claude-code", "cursor", "--scope", "project"]));
    assert_success(&sb.cmd(&["ide", "--remove", "--yes"]));
    assert_eq!(
        json_file(&sb.repo().join(".mcp.json"))["mcpServers"],
        serde_json::json!({})
    );
    assert_eq!(
        json_file(&sb.repo().join(".cursor/mcp.json"))["mcpServers"],
        serde_json::json!({})
    );
}

#[test]
fn a_second_uninstall_reports_cartog_not_wired() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["install", "claude-code", "--scope", "project"]));
    assert_success(&sb.cmd(&["uninstall"]));
    let out = sb.cmd(&["uninstall"]);
    assert_success(&out);
    assert_eq!(
        stdout(&out),
        "cartog is not wired in any config in scope.\n"
    );
}

#[test]
fn uninstall_in_a_fresh_sandbox_creates_nothing() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.home.path().join("fake-bin")).unwrap();
    let before = (tree(sb.repo()), tree(sb.home.path()));
    assert_success(&sb.cmd(&["uninstall"]));
    assert_eq!((tree(sb.repo()), tree(sb.home.path())), before);
}

#[test]
fn ide_remove_off_a_terminal_without_yes_refuses_and_changes_nothing() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["install", "claude-code", "--scope", "project"]));
    let before = sb.read(".mcp.json").unwrap();
    let out = sb.cmd(&["ide", "--remove"]);
    assert!(!out.status.success(), "expected a refusal");
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert_eq!(sb.read(".mcp.json").unwrap(), before);
}
