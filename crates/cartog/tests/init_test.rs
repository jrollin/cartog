//! Integration tests for `cartog init`.
//!
//! `cartog init` is config-only: it scaffolds `.cartog.toml` and prints a
//! next-steps hint pointing at `cartog ide` (for MCP wiring) and `cartog index`
//! (to build the graph). It must NOT touch MCP configs or the database.

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
fn init_scaffolds_cartog_toml_with_template() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init"]));
    let toml = sb.read(".cartog.toml").unwrap();
    assert!(
        toml.contains("[database]"),
        ".cartog.toml missing template: {toml}"
    );
    assert!(
        toml.contains("[embedding]"),
        ".cartog.toml missing template: {toml}"
    );
}

#[test]
fn init_does_not_write_mcp_files() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init"]));
    assert!(
        sb.read(".mcp.json").is_none(),
        "cartog init must not write .mcp.json; that is `cartog ide`'s job"
    );
    assert!(
        sb.read(".cursor/mcp.json").is_none(),
        "cartog init must not write .cursor/mcp.json"
    );
    assert!(
        sb.read(".vscode/mcp.json").is_none(),
        "cartog init must not write .vscode/mcp.json"
    );
}

#[test]
fn init_does_not_index() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init"]));
    assert!(
        !sb.repo().join(".cartog/db.sqlite").exists(),
        "cartog init must not create a database; that is `cartog index`'s job"
    );
}

#[test]
fn init_preserves_existing_cartog_toml() {
    let sb = Sandbox::new();
    let original = "# user-written\n[database]\npath = \"custom.db\"\n";
    fs::write(sb.repo().join(".cartog.toml"), original).unwrap();
    assert_success(&sb.cmd(&["init"]));
    assert_eq!(sb.read(".cartog.toml").unwrap(), original);
}

#[test]
fn init_dry_run_writes_nothing() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init", "--dry-run"]));
    assert!(sb.read(".cartog.toml").is_none());
}

#[test]
fn init_prints_next_steps_hint() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["init"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cartog ide"),
        "next-steps hint should mention `cartog ide`: {stdout}"
    );
    assert!(
        stdout.contains("cartog index"),
        "next-steps hint should mention `cartog index`: {stdout}"
    );
}

#[test]
fn init_is_idempotent() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init"]));
    let toml = sb.read(".cartog.toml").unwrap();

    assert_success(&sb.cmd(&["init"]));
    assert_eq!(sb.read(".cartog.toml").unwrap(), toml);
}
