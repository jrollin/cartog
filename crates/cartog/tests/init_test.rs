//! Integration tests for `cartog init`.

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
        // Seed at least one source file so the index step has something to do.
        fs::write(
            repo.path().join("hello.py"),
            "def greet():\n    return 'hi'\n",
        )
        .unwrap();
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
fn init_runs_index_and_writes_project_mcp_files() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["init", "--yes"]);
    assert_success(&out);

    assert!(
        sb.read(".cartog.toml").is_some(),
        ".cartog.toml not scaffolded"
    );
    assert!(sb.read(".mcp.json").is_some(), ".mcp.json not written");
    assert!(
        sb.read(".cursor/mcp.json").is_some(),
        ".cursor/mcp.json not written"
    );
    // The index step should have produced a database under .cartog/.
    assert!(
        sb.repo().join(".cartog/db.sqlite").exists() || sb.repo().join(".cartog.db").exists(),
        "expected the index to create a database file"
    );
}

#[test]
fn init_scaffolds_cartog_toml_with_template() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init", "--yes", "--no-index"]));
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
fn init_preserves_existing_cartog_toml() {
    let sb = Sandbox::new();
    let original = "# user-written\n[database]\npath = \"custom.db\"\n";
    fs::write(sb.repo().join(".cartog.toml"), original).unwrap();
    assert_success(&sb.cmd(&["init", "--yes", "--no-index"]));
    assert_eq!(sb.read(".cartog.toml").unwrap(), original);
}

#[test]
fn init_no_index_skips_database_creation() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init", "--yes", "--no-index"]));
    assert!(
        !sb.repo().join(".cartog/db.sqlite").exists(),
        "--no-index should skip database creation"
    );
    // MCP files still written.
    assert!(sb.read(".mcp.json").is_some());
}

#[test]
fn init_dry_run_writes_nothing() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init", "--dry-run"]));
    assert!(sb.read(".cartog.toml").is_none());
    assert!(sb.read(".mcp.json").is_none());
    assert!(sb.read(".cursor/mcp.json").is_none());
    assert!(!sb.repo().join(".cartog/db.sqlite").exists());
}

#[test]
fn init_is_idempotent() {
    let sb = Sandbox::new();
    assert_success(&sb.cmd(&["init", "--yes"]));
    let toml = sb.read(".cartog.toml").unwrap();
    let mcp = sb.read(".mcp.json").unwrap();
    let cursor = sb.read(".cursor/mcp.json").unwrap();

    assert_success(&sb.cmd(&["init", "--yes"]));
    assert_eq!(sb.read(".cartog.toml").unwrap(), toml);
    assert_eq!(sb.read(".mcp.json").unwrap(), mcp);
    assert_eq!(sb.read(".cursor/mcp.json").unwrap(), cursor);
}
