//! Integration tests for `cartog projects` and the registry write hooks.
//!
//! The `Sandbox` harness (copied from `consent_gate_test.rs`) is **mandatory**
//! here, not incidental: the project registry is user-global, so without the
//! `HOME` + XDG overrides every one of these tests would write into the
//! developer's own registry. On Linux the state dir comes from
//! `XDG_STATE_HOME`; on macOS it derives from `HOME`. Both are overridden.

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
        fs::write(repo.path().join("a.rs"), "fn main() {}\n").unwrap();
        Self { repo, home }
    }

    /// A second project inside the same sandboxed home, so one run can observe
    /// two registered projects — the whole point of the registry.
    fn sibling(&self, name: &str) -> PathBuf {
        let dir = self.home.path().join("projects").join(name);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("lib.rs"), "pub fn helper() {}\n").unwrap();
        dir
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        self.cmd_in(self.repo.path(), args, &[])
    }

    fn cmd_env(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        self.cmd_in(self.repo.path(), args, env)
    }

    fn cmd_in(
        &self,
        dir: &std::path::Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> std::process::Output {
        let mut c = Command::new(cartog_bin());
        c.args(args)
            .current_dir(dir)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("XDG_DATA_HOME", self.home.path().join(".local/share"))
            .env("XDG_STATE_HOME", self.home.path().join(".local/state"))
            .env_remove("CARGO_HOME")
            .env_remove("CARTOG_AUTO_INIT")
            .env_remove("CARTOG_REGISTRY")
            .env_remove("CARTOG_DB");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("failed to spawn cartog")
    }

    /// Index the sandbox repo, opting in via `CARTOG_AUTO_INIT` (the documented
    /// consent path for a config-less tree).
    fn index(&self) -> std::process::Output {
        self.cmd_env(&["index", "--no-lsp", "."], &[("CARTOG_AUTO_INIT", "1")])
    }

    fn index_dir(&self, dir: &std::path::Path) -> std::process::Output {
        self.cmd_in(
            dir,
            &["index", "--no-lsp", "."],
            &[("CARTOG_AUTO_INIT", "1")],
        )
    }

    fn projects_json(&self) -> serde_json::Value {
        let out = self.cmd(&["projects", "list", "--json"]);
        assert!(
            out.status.success(),
            "projects list --json failed: {}",
            stderr(&out)
        );
        serde_json::from_slice(&out.stdout).expect("projects list must emit valid JSON")
    }

    fn rows(&self) -> Vec<serde_json::Value> {
        self.projects_json()["projects"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

/// Locate `projects.sqlite` anywhere under a sandboxed home.
///
/// The state directory differs per platform, and the registry must be found by
/// search rather than by a hardcoded path — a wrong guess makes a test that
/// asserts on the registry silently assert on nothing.
fn find_registry(home: &std::path::Path) -> Option<PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Option<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|n| n == "projects.sqlite") {
                *out = Some(path);
            }
        }
    }
    let mut found = None;
    walk(home, &mut found);
    found
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ── the listing is honest about its own absence ──

#[test]
fn a_machine_with_nothing_indexed_lists_no_projects_and_exits_zero() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["projects", "list"]);
    assert!(out.status.success(), "an empty registry is not an error");
    assert!(
        stdout(&out).contains("No project registry found"),
        "got: {}",
        stdout(&out)
    );
}

#[test]
fn an_absent_registry_reports_registry_available_false() {
    // An agent must be able to tell "no registry" from "no projects".
    let sb = Sandbox::new();
    assert_eq!(sb.projects_json()["registry_available"], false);
}

// ── the consent gate governs registration ──

#[test]
fn a_gated_write_with_no_consent_registers_nothing() {
    // The consent gate refuses the index, so there is nothing to register —
    // registration must never be what creates the registry for a project
    // cartog was not allowed to index.
    let sb = Sandbox::new();
    let out = sb.cmd(&["index", "--no-lsp", "."]);
    assert!(!out.status.success(), "the consent gate must refuse");
    assert!(
        sb.rows().is_empty(),
        "a refused index must register nothing"
    );
}

#[test]
fn a_read_command_registers_nothing() {
    let sb = Sandbox::new();
    let _ = sb.cmd(&["search", "main"]);
    assert!(sb.rows().is_empty());
}

// ── the index hook ──

#[test]
fn index_registers_the_project_and_list_shows_it() {
    let sb = Sandbox::new();
    let out = sb.index();
    assert!(out.status.success(), "index failed: {}", stderr(&out));

    let rows = sb.rows();
    assert_eq!(rows.len(), 1, "one index must register exactly one project");
    let row = &rows[0];
    assert!(row["symbol_count"].as_u64().unwrap() > 0);
    assert!(row["db_path"].as_str().unwrap().ends_with("db.sqlite"));
    assert_eq!(row["missing"], false);
    assert_eq!(row["live"], false, "no serve peer is running");
}

#[test]
fn the_registered_counts_agree_with_cartog_stats() {
    // The registry caches what `cartog stats` reports; if the two disagree the
    // cache is lying about a project a consumer cannot cheaply re-check.
    let sb = Sandbox::new();
    assert!(sb.index().status.success());

    let stats_out = sb.cmd(&["stats", "--json"]);
    assert!(stats_out.status.success(), "{}", stderr(&stats_out));
    let stats: serde_json::Value = serde_json::from_slice(&stats_out.stdout).unwrap();

    let row = &sb.rows()[0];
    assert_eq!(row["symbol_count"], stats["num_symbols"]);
    assert_eq!(row["file_count"], stats["num_files"]);
}

#[test]
fn the_registered_row_names_the_project_after_its_directory() {
    let sb = Sandbox::new();
    let dir = sb.sibling("svc-billing");
    assert!(sb.index_dir(&dir).status.success());

    let rows = sb.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "svc-billing");
}

#[test]
fn two_separately_indexed_projects_both_register() {
    // The motivating case: a session in one repo must be able to see the other.
    let sb = Sandbox::new();
    assert!(sb.index().status.success());
    let other = sb.sibling("svc-shipping");
    assert!(sb.index_dir(&other).status.success());

    let rows = sb.rows();
    assert_eq!(rows.len(), 2, "both projects must be discoverable");
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"svc-shipping"));
}

#[test]
fn re_indexing_updates_the_row_rather_than_adding_one() {
    let sb = Sandbox::new();
    assert!(sb.index().status.success());
    fs::write(sb.repo.path().join("b.rs"), "fn extra() {}\n").unwrap();
    assert!(sb.index().status.success());

    assert_eq!(sb.rows().len(), 1, "one project is always one row");
}

// ── the kill switch ──

#[test]
fn an_empty_registry_env_disables_both_the_write_and_the_read() {
    let sb = Sandbox::new();
    let out = sb.cmd_env(
        &["index", "--no-lsp", "."],
        &[("CARTOG_AUTO_INIT", "1"), ("CARTOG_REGISTRY", "")],
    );
    assert!(out.status.success(), "the index itself must still work");

    // Read with the switch on: nothing was written.
    assert!(sb.rows().is_empty(), "the write must be disabled");

    // Read with the switch set: the read is disabled too.
    let listed = sb.cmd_env(&["projects", "list", "--json"], &[("CARTOG_REGISTRY", "")]);
    let json: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(json["registry_available"], false);
}

#[test]
fn a_relative_registry_env_disables_the_registry_rather_than_going_per_cwd() {
    let sb = Sandbox::new();
    let out = sb.cmd_env(
        &["index", "--no-lsp", "."],
        &[
            ("CARTOG_AUTO_INIT", "1"),
            ("CARTOG_REGISTRY", "projects.sqlite"),
        ],
    );
    assert!(out.status.success());
    assert!(
        !sb.repo.path().join("projects.sqlite").exists(),
        "a relative override must not drop a registry in the project directory"
    );
}

// ── markers ──

#[test]
fn a_deleted_database_is_marked_missing_and_prune_reaps_it() {
    let sb = Sandbox::new();
    assert!(sb.index().status.success());
    let db_path = PathBuf::from(sb.rows()[0]["db_path"].as_str().unwrap().to_string());
    fs::remove_file(&db_path).unwrap();

    assert_eq!(sb.rows()[0]["missing"], true);

    let dry = sb.cmd(&["projects", "prune", "--dry-run"]);
    assert!(dry.status.success());
    assert!(stdout(&dry).contains("Would drop"));
    assert_eq!(sb.rows().len(), 1, "a dry run must change nothing");

    let pruned = sb.cmd(&["projects", "prune"]);
    assert!(pruned.status.success());
    assert!(sb.rows().is_empty(), "prune must reap the missing project");
}

#[test]
fn prune_keeps_a_project_whose_database_still_exists() {
    let sb = Sandbox::new();
    assert!(sb.index().status.success());

    let out = sb.cmd(&["projects", "prune"]);
    assert!(out.status.success());
    assert_eq!(sb.rows().len(), 1, "a live project must survive prune");
}

// ── forget ──

#[test]
fn forget_removes_the_row_and_leaves_the_index_on_disk() {
    let sb = Sandbox::new();
    assert!(sb.index().status.success());
    let db_path = PathBuf::from(sb.rows()[0]["db_path"].as_str().unwrap().to_string());

    let out = sb.cmd(&["projects", "forget", sb.rows()[0]["id"].as_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));

    assert!(sb.rows().is_empty(), "the row must be gone");
    assert!(
        db_path.exists(),
        "forget must never delete the project's index"
    );
    assert!(
        sb.repo.path().join(".cartog").exists(),
        "forget must never remove the .cartog directory"
    );
}

#[test]
fn maintenance_on_a_machine_with_no_registry_explains_rather_than_erroring() {
    // "could not be opened" reads like a fault the user should chase; the
    // actual situation is almost always "there is no registry yet".
    let sb = Sandbox::new();

    let forget = sb.cmd(&["projects", "forget", "anything"]);
    assert!(
        forget.status.success(),
        "an absent registry is not an error"
    );
    assert!(
        stdout(&forget).contains("No project registry on this machine"),
        "got: {}",
        stdout(&forget)
    );

    let prune = sb.cmd(&["projects", "prune"]);
    assert!(prune.status.success());
    assert!(
        stdout(&prune).contains("No project registry"),
        "got: {}",
        stdout(&prune)
    );
}

#[test]
fn forget_never_creates_the_registry_it_was_asked_to_remove_from() {
    // Regression: `forget` opened the registry read-write (which creates it)
    // before knowing whether anything matched, so forgetting on a machine with
    // no registry left an empty one behind.
    let sb = Sandbox::new();

    let _ = sb.cmd(&["projects", "forget", "nosuch"]);
    let _ = sb.cmd(&["projects", "prune"]);

    assert!(
        find_registry(sb.home.path()).is_none(),
        "no maintenance command may create the registry"
    );
}

#[test]
fn forget_an_unknown_target_reports_no_match_and_changes_nothing() {
    let sb = Sandbox::new();
    assert!(sb.index().status.success());

    let out = sb.cmd(&["projects", "forget", "no-such-project"]);
    assert!(out.status.success(), "a miss is not an error");
    assert!(stdout(&out).contains("No registered project matches"));
    assert_eq!(sb.rows().len(), 1);
}

#[test]
fn forget_by_an_ambiguous_name_drops_nothing_and_lists_the_candidates() {
    // Two workspaces each holding an `api` directory produce two rows named
    // `api`; deleting both from one argument would deregister a project the
    // user never named.
    let sb = Sandbox::new();
    for ws in ["w1", "w2"] {
        let dir = sb.home.path().join(ws).join("api");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("a.rs"), "fn f() {}\n").unwrap();
        assert!(sb.index_dir(&dir).status.success());
    }
    assert_eq!(sb.rows().len(), 2);

    let out = sb.cmd(&["projects", "forget", "api"]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("matches 2 projects"),
        "got: {}",
        stdout(&out)
    );
    assert_eq!(sb.rows().len(), 2, "an ambiguous forget must drop nothing");
}

// ── `projects` is a read command, not a gated write ──

#[test]
fn projects_list_works_from_a_directory_with_no_git_and_no_config() {
    // Proof that `projects` never touches db_path resolution or the consent
    // gate: a machine-global listing must work from anywhere.
    let sb = Sandbox::new();
    assert!(sb.index().status.success());

    let bare = sb.home.path().join("nowhere");
    fs::create_dir_all(&bare).unwrap();
    let out = sb.cmd_in(&bare, &["projects", "list", "--json"], &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["projects"].as_array().unwrap().len(), 1);
    assert!(
        !bare.join(".cartog").exists(),
        "a read command must never create .cartog"
    );
}

// ── corruption ──

#[test]
fn a_corrupt_registry_is_renamed_aside_rather_than_truncated() {
    let sb = Sandbox::new();
    assert!(sb.index().status.success());

    // Discover the registry rather than hardcoding a path: the state dir is
    // platform-specific (`$XDG_STATE_HOME/cartog` on Linux,
    // `~/Library/Application Support/io.cartog.cartog` on macOS) and a
    // hardcoded guess silently turns this into a no-op test.
    let registry = find_registry(sb.home.path()).expect("the index must have created a registry");
    let dir = registry.parent().unwrap().to_path_buf();
    fs::write(&registry, b"this is not a database").unwrap();

    let out = sb.cmd(&["projects", "list"]);
    assert!(out.status.success(), "a corrupt registry is not fatal");

    let quarantined: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            p.to_string_lossy().contains(".corrupt.").then_some(p)
        })
        .collect();
    assert_eq!(quarantined.len(), 1, "the corrupt file must be preserved");
    assert_eq!(
        fs::read(&quarantined[0]).unwrap(),
        b"this is not a database",
        "quarantine must never truncate — the bytes are the evidence"
    );
}

// ── rag index ──

#[test]
fn rag_index_does_not_claim_the_graph_was_re_indexed() {
    // Embedding is not a graph index pass. If `rag index` stamped
    // `last_indexed`, a stale graph would report as freshly indexed.
    let sb = Sandbox::new();
    assert!(sb.index().status.success());
    let before = sb.rows()[0]["last_indexed"].clone();

    // `rag index` needs a model; on a machine without one this fails, which is
    // fine — the assertion is that the row's last_indexed did not move either
    // way. Run it and ignore the status.
    let _ = sb.cmd(&["rag", "index", "."]);

    assert_eq!(
        sb.rows()[0]["last_indexed"],
        before,
        "rag index must never move last_indexed"
    );
}
