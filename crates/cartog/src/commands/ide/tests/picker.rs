use crate::cli::ClientKind;
use crate::commands::ide::catalogue::*;
use crate::commands::ide::picker::*;
use crate::commands::ide::run::spec_for;
use crate::commands::ide::{Detection, Scope};
use std::path::{Path, PathBuf};
// ── Picker helpers ────────────────────────────────────────────────────

fn opt(scope: Scope, installed: bool, file_present: bool) -> ScopeOption {
    ScopeOption {
        scope,
        path: PathBuf::from("/tmp/foo"),
        installed,
        file_present,
    }
}

#[test]
fn scope_option_status_reports_not_installed_when_parent_missing() {
    assert_eq!(
        scope_option_status(&opt(Scope::User, false, false)),
        "not installed"
    );
}

#[test]
fn scope_option_status_reports_will_create_when_parent_exists_but_file_does_not() {
    assert_eq!(
        scope_option_status(&opt(Scope::Project, true, false)),
        "will create"
    );
}

#[test]
fn scope_option_status_reports_will_merge_when_file_exists() {
    assert_eq!(
        scope_option_status(&opt(Scope::User, true, true)),
        "present, will merge"
    );
}

#[test]
fn picker_items_groups_claude_code_into_two_scopes() {
    // Claude Code has Project + User entries; the picker collapses them
    // into one PickerItem with two ScopeOptions.
    let tmp = tempfile::tempdir().unwrap();
    let homes = HomeDirs::default();
    let items = picker_items(tmp.path(), &homes, None);
    let cc = items
        .iter()
        .find(|i| i.kind == ClientKind::ClaudeCode)
        .unwrap();
    assert_eq!(cc.scopes.len(), 2, "Claude Code should have 2 scopes");
    let scopes: Vec<Scope> = cc.scopes.iter().map(|s| s.scope).collect();
    assert!(scopes.contains(&Scope::Project));
    assert!(scopes.contains(&Scope::User));
}

#[test]
fn picker_items_other_clients_have_a_single_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let homes = HomeDirs::default();
    let items = picker_items(tmp.path(), &homes, None);
    // The dual-scope clients (project + user).
    let dual_scope = [
        ClientKind::ClaudeCode,
        ClientKind::Kiro,
        ClientKind::Vscode,
        ClientKind::Cursor,
        ClientKind::Qwen,
        ClientKind::Roo,
        ClientKind::Kilo,
    ];
    for item in &items {
        if !dual_scope.contains(&item.kind) {
            assert_eq!(
                item.scopes.len(),
                1,
                "{:?} should have exactly one scope, got {}",
                item.kind,
                item.scopes.len(),
            );
        }
    }
}

#[test]
fn picker_items_marks_project_clients_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let homes = HomeDirs::default();
    let items = picker_items(tmp.path(), &homes, None);
    let cursor = items.iter().find(|i| i.kind == ClientKind::Cursor).unwrap();
    // The original project rows always read as "installed": the repo is the evidence.
    assert!(cursor.scopes[0].installed);
    assert!(!cursor.scopes[0].file_present);
}

#[test]
fn picker_items_marks_no_cli_user_client_not_installed_when_parent_missing() {
    // Claude Desktop has no CLI binary, so detection relies solely on the
    // config-dir proxy — PATH-independent, unlike CLI clients (claude,
    // codex, ...) whose installed-ness now depends on the test host's PATH.
    let tmp = tempfile::tempdir().unwrap();
    let homes = HomeDirs {
        claude_desktop: tmp.path().join("does/not/exist/desktop.json"),
        ..HomeDirs::default()
    };
    let items = picker_items(tmp.path(), &homes, None);
    let desktop = items
        .iter()
        .find(|i| i.kind == ClientKind::ClaudeDesktop)
        .unwrap();
    assert!(
        !desktop.scopes[0].installed,
        "Claude Desktop with no config dir should be not-installed"
    );
}

#[test]
fn binary_in_finds_executable_on_path() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bin");
    std::fs::create_dir(&dir).unwrap();
    let exe = if cfg!(windows) {
        "mytool.exe"
    } else {
        "mytool"
    };
    std::fs::write(dir.join(exe), b"#!/bin/sh\n").unwrap();
    let paths = std::env::join_paths([dir.as_path()]).unwrap();
    assert!(binary_in(&paths, "mytool"), "executable on PATH is found");
    assert!(
        !binary_in(&paths, "absent"),
        "missing executable is not found"
    );
}

#[test]
fn client_installed_is_always_true_for_always_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope/mcp.json");
    assert!(client_installed(
        ClientKind::Cursor,
        &missing,
        &Detection::Always,
        None
    ));
}

#[test]
fn client_installed_no_cli_client_uses_dir_proxy() {
    let tmp = tempfile::tempdir().unwrap();
    let present = tmp.path().join("cfg.json"); // parent (tmp) exists
    let absent = tmp.path().join("nope/cfg.json"); // parent missing
                                                   // Claude Desktop has no CLI, so only the dir proxy applies.
    assert!(client_installed(
        ClientKind::ClaudeDesktop,
        &present,
        &Detection::ParentDir,
        None
    ));
    assert!(!client_installed(
        ClientKind::ClaudeDesktop,
        &absent,
        &Detection::ParentDir,
        None
    ));
}

#[test]
fn client_installed_detects_cli_only_via_injected_path() {
    // Codex (a CLI client) with a missing config dir is "installed" only
    // when its binary is on the injected PATH — deterministic, not the host's.
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    std::fs::create_dir(&bindir).unwrap();
    let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
    std::fs::write(bindir.join(exe), b"#!/bin/sh\n").unwrap();
    let with_codex = std::env::join_paths([bindir.as_path()]).unwrap();
    let empty = std::env::join_paths::<_, &Path>([]).unwrap();
    let cfg = tmp.path().join("nope/config.toml"); // parent missing

    assert!(
        client_installed(
            ClientKind::Codex,
            &cfg,
            &Detection::ParentDir,
            Some(&with_codex)
        ),
        "codex on the injected PATH counts as installed"
    );
    assert!(
        !client_installed(ClientKind::Codex, &cfg, &Detection::ParentDir, Some(&empty)),
        "no codex on PATH and no config dir → not installed"
    );
}

#[test]
fn client_installed_ignores_path_when_home_is_relative_fallback() {
    // HomeDirs::default() anchors user paths at "." (relative). A PATH match
    // must NOT count then — wiring would litter the cwd. The config path is
    // relative, so even with codex on PATH the client is "not installed".
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    std::fs::create_dir(&bindir).unwrap();
    let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
    std::fs::write(bindir.join(exe), b"#!/bin/sh\n").unwrap();
    let with_codex = std::env::join_paths([bindir.as_path()]).unwrap();
    // Relative path (the "." fallback shape), parent "." exists but isn't a home.
    let relative_cfg = Path::new("config.toml");

    assert!(
        !client_installed(
            ClientKind::Codex,
            relative_cfg,
            &Detection::ParentDir,
            Some(&with_codex)
        ),
        "PATH match must be ignored when the config path is relative (no real home)"
    );
}

#[test]
fn picker_items_marks_user_client_installed_when_parent_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join("Library/Application Support/Claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let homes = HomeDirs {
        claude_desktop: claude_dir.join("claude_desktop_config.json"),
        ..HomeDirs::default()
    };
    let items = picker_items(tmp.path(), &homes, None);
    let claude = items
        .iter()
        .find(|i| i.kind == ClientKind::ClaudeDesktop)
        .unwrap();
    assert!(claude.scopes[0].installed);
    assert!(!claude.scopes[0].file_present);
}

#[test]
fn format_picker_label_single_scope_includes_name_and_status() {
    let item = PickerItem {
        kind: ClientKind::Cursor,
        scopes: vec![opt(Scope::Project, true, false)],
    };
    let label = format_picker_label(&item);
    assert!(label.contains("Cursor"), "label missing name: {label}");
    assert!(
        label.contains("project"),
        "label missing scope hint: {label}"
    );
    assert!(
        label.contains("will create"),
        "label missing status: {label}"
    );
}

#[test]
fn format_picker_label_multi_scope_hints_at_choice() {
    let item = PickerItem {
        kind: ClientKind::ClaudeCode,
        scopes: vec![
            opt(Scope::Project, true, false),
            opt(Scope::User, true, true),
        ],
    };
    let label = format_picker_label(&item);
    assert!(label.contains("Claude Code"));
    assert!(
        label.contains("project + user available"),
        "multi-scope hint missing: {label}",
    );
}

#[test]
fn any_installed_true_when_at_least_one_scope_installed() {
    let item = PickerItem {
        kind: ClientKind::ClaudeCode,
        scopes: vec![
            opt(Scope::Project, true, false),
            opt(Scope::User, false, false),
        ],
    };
    assert!(item.any_installed());
}

#[test]
fn any_installed_false_when_no_scope_installed() {
    let item = PickerItem {
        kind: ClientKind::Zed,
        scopes: vec![opt(Scope::User, false, false)],
    };
    assert!(!item.any_installed());
}

#[test]
fn spec_for_returns_none_for_unknown_combination() {
    // Codex only exists at user scope; asking for project scope should
    // return None so the picker can't construct an impossible spec.
    let tmp = tempfile::tempdir().unwrap();
    let homes = HomeDirs::default();
    assert!(spec_for(ClientKind::Codex, Scope::Project, false, tmp.path(), &homes).is_none());
}

#[test]
fn spec_for_builds_claude_code_with_or_without_watch() {
    let tmp = tempfile::tempdir().unwrap();
    let homes = HomeDirs::default();
    let with = spec_for(
        ClientKind::ClaudeCode,
        Scope::Project,
        false,
        tmp.path(),
        &homes,
    )
    .unwrap();
    let without = spec_for(
        ClientKind::ClaudeCode,
        Scope::Project,
        true,
        tmp.path(),
        &homes,
    )
    .unwrap();
    assert_eq!(with.args, vec!["serve", "--watch"]);
    assert_eq!(without.args, vec!["serve"]);
}

#[test]
fn picker_marks_skills_only_qwen_home_as_not_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".qwen/skills")).unwrap();
    let homes = HomeDirs {
        qwen: home.join(".qwen/settings.json"),
        ..HomeDirs::default()
    };
    let empty = std::env::join_paths::<_, &Path>([]).unwrap();
    let items = picker_items(tmp.path(), &homes, Some(&empty));
    let qwen = items.iter().find(|i| i.kind == ClientKind::Qwen).unwrap();
    let user = qwen.scopes.iter().find(|o| o.scope == Scope::User).unwrap();
    assert!(
        !user.installed,
        "a skills-only ~/.qwen is not a Qwen install"
    );
}

#[test]
fn picker_pre_checks_new_project_rows_only_when_detected() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".roo")).unwrap();
    let empty = std::env::join_paths::<_, &Path>([]).unwrap();
    let items = picker_items(tmp.path(), &HomeDirs::default(), Some(&empty));
    let project = |kind: ClientKind| {
        items
            .iter()
            .find(|i| i.kind == kind)
            .and_then(|i| i.scopes.iter().find(|o| o.scope == Scope::Project))
            .map(|o| o.installed)
            .unwrap()
    };
    assert!(project(ClientKind::Roo), ".roo/ exists");
    assert!(!project(ClientKind::Qwen), "no .qwen/, nothing detected");
    assert!(!project(ClientKind::Kilo), "no .kilo/, nothing detected");
}

fn removal_spec(kind: ClientKind, path: PathBuf) -> crate::commands::ide::ClientSpec {
    crate::commands::ide::ClientSpec {
        kind,
        scope: Scope::User,
        path,
        strategy: crate::commands::ide::MergeStrategy::McpServers,
        args: vec!["serve".into()],
        detection: Detection::ParentDir,
        requires: None,
    }
}

#[test]
fn removal_plan_offers_only_files_that_hold_cartog() {
    let tmp = tempfile::tempdir().unwrap();
    let wired = tmp.path().join("wired.json");
    let unwired = tmp.path().join("unwired.json");
    std::fs::write(&wired, r#"{"mcpServers": {"cartog": {}}}"#).unwrap();
    std::fs::write(&unwired, r#"{"mcpServers": {"other": {}}}"#).unwrap();
    let specs = vec![
        removal_spec(ClientKind::Cursor, wired.clone()),
        removal_spec(ClientKind::Qwen, unwired),
        removal_spec(ClientKind::Kiro, tmp.path().join("absent.json")),
    ];
    let plan = removal_plan(&specs, tmp.path());
    let listed: Vec<_> = plan
        .items
        .iter()
        .map(|i| (i.kind, i.path.clone()))
        .collect();
    assert_eq!(listed, vec![(ClientKind::Cursor, wired)]);
    assert!(plan.skipped.is_empty());
}

#[test]
fn removal_plan_reports_unparseable_files_as_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("kilo.jsonc");
    std::fs::write(&bad, "// c\n{\"mcpServers\": {\"cartog\": {}}}").unwrap();
    let plan = removal_plan(&[removal_spec(ClientKind::Kilo, bad)], tmp.path());
    assert!(plan.items.is_empty());
    let statuses: Vec<_> = plan.skipped.iter().map(|s| s.status).collect();
    assert_eq!(statuses, vec![crate::commands::ide::IdeStatus::Skipped]);
}

#[test]
fn removal_plan_is_empty_when_nothing_is_wired() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = removal_plan(
        &[removal_spec(
            ClientKind::Cursor,
            tmp.path().join("absent.json"),
        )],
        tmp.path(),
    );
    assert!(plan.items.is_empty() && plan.skipped.is_empty());
}
