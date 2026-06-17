use super::args;
use crate::cli::{ClientKind, IdeScope};
use crate::commands::ide::catalogue::*;
use crate::commands::ide::merge::merge_entry;
use crate::commands::ide::{MergeStrategy, Scope};
use serde_json::Value;
use std::path::Path;
#[test]
fn build_specs_default_covers_all_clients() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::detect();
    let specs = build_specs(None, IdeScope::All, false, &tmp, &homes);
    // Project: claude-code, cursor, vscode, kiro (4)
    // User: claude-code, claude-desktop, codex, gemini, opencode, windsurf, zed,
    //       antigravity, kiro, hermes, vscode (11)
    assert_eq!(specs.len(), 15);
}

#[test]
fn build_specs_project_scope_drops_user_clients() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(None, IdeScope::Project, false, &tmp, &homes);
    // claude-code, cursor, vscode, kiro
    assert_eq!(specs.len(), 4);
    assert!(specs.iter().all(|s| s.scope == Scope::Project));
}

#[test]
fn build_specs_claude_code_filter_returns_both_scopes() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(
        Some(ClientKind::ClaudeCode),
        IdeScope::All,
        false,
        &tmp,
        &homes,
    );
    assert_eq!(specs.len(), 2);
    let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
    assert!(scopes.contains(&Scope::Project));
    assert!(scopes.contains(&Scope::User));
}

#[test]
fn build_specs_vscode_filter_returns_project_and_user() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs {
        vscode: tmp.join("Code/User/mcp.json"),
        ..HomeDirs::default()
    };
    let specs = build_specs(
        Some(ClientKind::Vscode),
        IdeScope::All,
        false,
        tmp.as_path(),
        &homes,
    );
    let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
    assert!(
        scopes.contains(&Scope::Project),
        "vscode missing project scope"
    );
    assert!(scopes.contains(&Scope::User), "vscode missing user scope");
    let user = specs
        .iter()
        .find(|s| s.scope == Scope::User)
        .expect("vscode user spec");
    assert!(user.path.ends_with("Code/User/mcp.json"));
    assert_eq!(user.strategy, MergeStrategy::VsCodeServers);
}

#[test]
fn detect_vscode_user_path_under_config_dir() {
    // VS Code's user mcp.json lives at <config>/Code/User/mcp.json on every OS.
    let homes = HomeDirs::detect();
    assert!(
        homes.vscode.ends_with("Code/User/mcp.json"),
        "unexpected vscode user path: {}",
        homes.vscode.display()
    );
}

#[test]
fn build_specs_client_filter_picks_one() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(Some(ClientKind::Cursor), IdeScope::All, false, &tmp, &homes);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].kind, ClientKind::Cursor);
}

#[test]
fn build_specs_kiro_filter_returns_both_scopes() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(Some(ClientKind::Kiro), IdeScope::All, false, &tmp, &homes);
    assert_eq!(specs.len(), 2);
    let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
    assert!(scopes.contains(&Scope::Project));
    assert!(scopes.contains(&Scope::User));
}

#[test]
fn kiro_project_path_is_kiro_settings_mcp_json() {
    let cwd = Path::new("/tmp/proj");
    assert_eq!(
        project_path(ClientKind::Kiro, cwd),
        Some(cwd.join(".kiro").join("settings").join("mcp.json"))
    );
}

#[test]
fn antigravity_and_hermes_are_user_only() {
    let cwd = Path::new("/tmp/proj");
    assert_eq!(project_path(ClientKind::Antigravity, cwd), None);
    assert_eq!(project_path(ClientKind::Hermes, cwd), None);
}

#[test]
fn antigravity_and_kiro_use_mcp_servers_shape() {
    // Both reuse the existing McpServers JSON strategy.
    let o = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
    let v: Value = serde_json::from_str(&o.new_json).unwrap();
    assert_eq!(v["mcpServers"]["cartog"]["command"], "cartog");
}
