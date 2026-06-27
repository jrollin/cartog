//! Integration tests for the consent gate: a config-less, un-indexed repo must
//! never get a `.cartog/` from cartog running on its own.
//!
//! - Write commands (`index` / `rag index` / `watch`) refuse without consent.
//! - Read commands return the empty-index hint and create nothing.
//! - Consent is granted by `cartog init`, an existing index, or
//!   `CARTOG_AUTO_INIT` (the env bypass writes no config file).
//!
//! The harness mirrors `init_test.rs`: the built binary in a temp git repo with
//! an isolated HOME so it can't read the developer's real config or state.

use std::fs;
use std::path::PathBuf;
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
        // A trivial source file so a permitted index has something to do.
        fs::write(repo.path().join("a.rs"), "fn main() {}\n").unwrap();
        Self { repo, home }
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        self.cmd_env(args, &[])
    }

    fn cmd_env(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let mut c = Command::new(cartog_bin());
        c.args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("XDG_DATA_HOME", self.home.path().join(".local/share"))
            .env("XDG_STATE_HOME", self.home.path().join(".local/state"))
            .env_remove("CARGO_HOME")
            // The gate keys on this env var; clear any ambient value so tests
            // are deterministic regardless of the developer's shell.
            .env_remove("CARTOG_AUTO_INIT");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("failed to spawn cartog")
    }

    fn has(&self, rel: &str) -> bool {
        self.repo.path().join(rel).exists()
    }
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn index_refuses_fresh_repo_without_config() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["index", "."]);
    assert!(
        !out.status.success(),
        "index must refuse a fresh repo: stdout={} stderr={}",
        stdout(&out),
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(
        err.contains(".cartog.toml") || err.contains("cartog init"),
        "refusal must name the fix: {err}"
    );
    assert!(
        !sb.has(".cartog"),
        ".cartog/ must NOT be created by a refused index"
    );
}

#[test]
fn rag_index_refuses_fresh_repo_without_config() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["rag", "index", "."]);
    assert!(!out.status.success(), "rag index must refuse a fresh repo");
    assert!(
        !sb.has(".cartog"),
        ".cartog/ must NOT be created by a refused rag index"
    );
}

#[test]
fn read_command_no_create_on_fresh_repo() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["search", "foo"]);
    // Read commands succeed (exit 0) and just report empty.
    assert!(
        out.status.success(),
        "search on a fresh repo should exit 0: stderr={}",
        stderr(&out)
    );
    // The empty-index hint guides the user from the empty result to opting in.
    let so = stdout(&out);
    assert!(
        so.contains("index is empty") && so.contains("cartog init"),
        "search must surface the empty-index hint pointing at cartog init: {so}"
    );
    assert!(
        !sb.has(".cartog"),
        ".cartog/ must NOT be created by a read command on a fresh repo"
    );
}

#[test]
fn index_allows_with_auto_init() {
    let sb = Sandbox::new();
    let out = sb.cmd_env(&["index", "."], &[("CARTOG_AUTO_INIT", "1")]);
    assert!(
        out.status.success(),
        "CARTOG_AUTO_INIT must permit indexing: stderr={}",
        stderr(&out)
    );
    assert!(
        sb.has(".cartog/db.sqlite"),
        "AUTO_INIT index must create the DB"
    );
    assert!(
        !sb.has(".cartog.toml"),
        "AUTO_INIT must NOT write a config file — only `cartog init` does"
    );
}

#[test]
fn index_allows_after_init() {
    let sb = Sandbox::new();
    // init writes config only…
    let init = sb.cmd(&["init"]);
    assert!(init.status.success(), "init failed: {}", stderr(&init));
    assert!(sb.has(".cartog.toml"));
    assert!(
        !sb.has(".cartog/db.sqlite"),
        "init must not create the DB (that's `index`'s job)"
    );
    // …then index is permitted by the now-present config.
    let out = sb.cmd(&["index", "."]);
    assert!(
        out.status.success(),
        "index after init must succeed: stderr={}",
        stderr(&out)
    );
    assert!(
        sb.has(".cartog/db.sqlite"),
        "index after init must build the DB"
    );
}

#[test]
fn serve_starts_degraded_without_creating_dir() {
    use std::process::Stdio;
    // `cartog serve` (no --watch) on a config-less, un-indexed repo must come
    // up degraded and create no `.cartog/`. Closed stdin → the rmcp stdio loop
    // sees EOF and the process exits; the exact exit code on immediate EOF is
    // transport-level (identical for a normally-opened serve), so we don't
    // assert it. The consent-gate guarantee is: nothing was materialized.
    let sb = Sandbox::new();
    let mut child = Command::new(cartog_bin())
        .args(["serve"])
        .current_dir(sb.repo.path())
        .env("HOME", sb.home.path())
        .env("XDG_CONFIG_HOME", sb.home.path().join(".config"))
        .env("XDG_DATA_HOME", sb.home.path().join(".local/share"))
        .env("XDG_STATE_HOME", sb.home.path().join(".local/state"))
        .env_remove("CARGO_HOME")
        .env_remove("CARTOG_AUTO_INIT")
        .stdin(Stdio::null()) // immediate EOF → server exits
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn cartog serve");
    child.wait().expect("serve did not exit");
    assert!(
        !sb.has(".cartog"),
        "degraded serve must NOT create .cartog/ for a config-less repo"
    );
}

#[test]
fn serve_with_config_creates_index_dir() {
    use std::process::Stdio;
    // Contrast: with a `.cartog.toml` present, serve IS consented and opens the
    // DB for real — creating `.cartog/db.sqlite`. This pins that the previous
    // test's "no .cartog/" is the consent gate at work, not serve never
    // creating anything.
    let sb = Sandbox::new();
    fs::write(sb.repo.path().join(".cartog.toml"), "[database]\n").unwrap();
    let mut child = Command::new(cartog_bin())
        .args(["serve"])
        .current_dir(sb.repo.path())
        .env("HOME", sb.home.path())
        .env("XDG_CONFIG_HOME", sb.home.path().join(".config"))
        .env("XDG_DATA_HOME", sb.home.path().join(".local/share"))
        .env("XDG_STATE_HOME", sb.home.path().join(".local/state"))
        .env_remove("CARGO_HOME")
        .env_remove("CARTOG_AUTO_INIT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn cartog serve");
    child.wait().expect("serve did not exit");
    assert!(
        sb.has(".cartog/db.sqlite"),
        "serve with a config present must open (create) the DB"
    );
}

#[test]
fn index_allows_when_db_already_exists_without_config() {
    // Branch 1: once an index exists the project is de-facto opted in, even
    // with no config and no env var — steady-state updates keep working.
    let sb = Sandbox::new();
    // Bootstrap a DB via AUTO_INIT (writes no config), then drop the env var.
    let boot = sb.cmd_env(&["index", "."], &[("CARTOG_AUTO_INIT", "1")]);
    assert!(boot.status.success());
    assert!(sb.has(".cartog/db.sqlite"));
    assert!(!sb.has(".cartog.toml"));

    // Re-index with no config and no env — the existing DB grants consent.
    let out = sb.cmd(&["index", "."]);
    assert!(
        out.status.success(),
        "re-indexing an existing DB without config must work: stderr={}",
        stderr(&out)
    );
}
