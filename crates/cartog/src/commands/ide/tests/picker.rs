use crate::cli::ClientKind;
use crate::commands::ide::catalogue::*;
use crate::commands::ide::picker::*;
use crate::commands::ide::run::spec_for;
use crate::commands::ide::Scope;
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
    let items = picker_items(tmp.path(), &homes);
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
    let items = picker_items(tmp.path(), &homes);
    // Claude Code, Kiro and VS Code are the dual-scope clients (project + user).
    let dual_scope = [ClientKind::ClaudeCode, ClientKind::Kiro, ClientKind::Vscode];
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
    let items = picker_items(tmp.path(), &homes);
    let cursor = items.iter().find(|i| i.kind == ClientKind::Cursor).unwrap();
    // Project-scoped rows always read as "installed" — the repo IS the parent.
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
    let items = picker_items(tmp.path(), &homes);
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
fn client_installed_is_always_true_for_project_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope/mcp.json");
    assert!(client_installed(
        ClientKind::Cursor,
        Scope::Project,
        &missing,
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
        Scope::User,
        &present,
        None
    ));
    assert!(!client_installed(
        ClientKind::ClaudeDesktop,
        Scope::User,
        &absent,
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
        client_installed(ClientKind::Codex, Scope::User, &cfg, Some(&with_codex)),
        "codex on the injected PATH counts as installed"
    );
    assert!(
        !client_installed(ClientKind::Codex, Scope::User, &cfg, Some(&empty)),
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
            Scope::User,
            relative_cfg,
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
    let items = picker_items(tmp.path(), &homes);
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
    // Cursor only exists at project scope; asking for user scope should
    // return None so the picker can't construct an impossible spec.
    let tmp = tempfile::tempdir().unwrap();
    let homes = HomeDirs::default();
    assert!(spec_for(ClientKind::Cursor, Scope::User, false, tmp.path(), &homes).is_none());
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
