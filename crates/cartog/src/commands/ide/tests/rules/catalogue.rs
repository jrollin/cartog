use super::{write, Sandbox};
use crate::cli::{ClientKind, IdeScope};
use crate::commands::ide::rules::{
    blocker, detected_rule_targets, named_rule_targets, removal_rule_targets, Blocker, RuleTarget,
    RULE_AGENTS,
};
use std::ffi::OsStr;
use std::fs;

fn kinds(targets: &[RuleTarget]) -> Vec<ClientKind> {
    targets.iter().map(|t| t.kind).collect()
}

fn detected(sb: &Sandbox, path_env: Option<&OsStr>) -> Vec<ClientKind> {
    kinds(&detected_rule_targets(&sb.cwd, &sb.homes(), path_env))
}

#[test]
fn an_agent_cli_on_path_is_evidence() {
    let sb = Sandbox::new();
    let bin = sb.home.join("bin");
    write(&bin.join("kiro"), "");
    assert_eq!(detected(&sb, Some(bin.as_os_str())), vec![ClientKind::Kiro]);
}

#[test]
fn a_user_config_location_is_evidence() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.home.join(".codeium/windsurf")).unwrap();
    assert_eq!(detected(&sb, None), vec![ClientKind::Windsurf]);
}

#[test]
fn a_rules_dir_already_in_the_repo_is_evidence() {
    let sb = Sandbox::new();
    sb.dir(".roo/rules");
    assert_eq!(detected(&sb, None), vec![ClientKind::Roo]);
}

#[test]
fn mcp_folders_cartog_creates_are_not_evidence() {
    let sb = Sandbox::new();
    for rel in [
        ".mcp.json",
        ".cursor/mcp.json",
        ".kiro/settings/mcp.json",
        ".roo/mcp.json",
        ".vscode/mcp.json",
    ] {
        sb.file(rel, "{}");
    }
    assert_eq!(detected(&sb, None), vec![]);
}

#[test]
fn no_evidence_selects_no_agent() {
    let sb = Sandbox::new();
    assert_eq!(detected(&sb, None), vec![]);
}

#[test]
fn detected_targets_follow_report_order() {
    let sb = Sandbox::new();
    sb.dir(".claude/rules");
    sb.dir(".cursor/rules");
    assert_eq!(
        detected(&sb, None),
        vec![ClientKind::Cursor, ClientKind::ClaudeCode]
    );
}

#[test]
fn a_target_points_at_the_agent_rule_file_in_the_repo() {
    let sb = Sandbox::new();
    sb.dir(".cursor/rules");
    let targets = detected_rule_targets(&sb.cwd, &sb.homes(), None);
    assert_eq!(targets[0].path, sb.cwd.join(".cursor/rules/cartog.mdc"));
}

#[test]
fn named_rule_agents_need_no_evidence() {
    let sb = Sandbox::new();
    let (targets, _) = named_rule_targets(&[ClientKind::ClaudeCode, ClientKind::Cursor], &sb.cwd);
    assert_eq!(
        kinds(&targets),
        vec![ClientKind::Cursor, ClientKind::ClaudeCode]
    );
}

#[test]
fn named_clients_without_a_rules_dir_are_returned_once_each() {
    let sb = Sandbox::new();
    let (targets, unsupported) = named_rule_targets(
        &[
            ClientKind::Zed,
            ClientKind::Cursor,
            ClientKind::Zed,
            ClientKind::Codex,
        ],
        &sb.cwd,
    );
    assert_eq!(
        (kinds(&targets), unsupported),
        (
            vec![ClientKind::Cursor],
            vec![ClientKind::Zed, ClientKind::Codex]
        )
    );
}

#[test]
fn a_client_named_twice_yields_one_target() {
    let sb = Sandbox::new();
    let (targets, _) = named_rule_targets(&[ClientKind::Cursor, ClientKind::Cursor], &sb.cwd);
    assert_eq!(kinds(&targets), vec![ClientKind::Cursor]);
}

#[test]
fn removal_targets_every_rule_agent_when_none_named() {
    let sb = Sandbox::new();
    let targets = removal_rule_targets(&[], IdeScope::All, &sb.cwd);
    assert_eq!(kinds(&targets), RULE_AGENTS.to_vec());
}

#[test]
fn removal_targets_only_named_rule_agents() {
    let sb = Sandbox::new();
    let targets = removal_rule_targets(
        &[ClientKind::Zed, ClientKind::Cursor],
        IdeScope::Project,
        &sb.cwd,
    );
    assert_eq!(kinds(&targets), vec![ClientKind::Cursor]);
}

#[test]
fn removal_at_user_scope_targets_no_rule_file() {
    let sb = Sandbox::new();
    let targets = removal_rule_targets(&[], IdeScope::User, &sb.cwd);
    assert_eq!(kinds(&targets), vec![]);
}

#[test]
fn roorules_with_no_roo_rules_dir_blocks_roo() {
    let sb = Sandbox::new();
    let fallback = sb.file(".roorules", "team rules");
    assert_eq!(
        blocker(ClientKind::Roo, &sb.cwd),
        Some(Blocker::FallbackFile(fallback))
    );
}

#[test]
fn a_clinerules_file_with_an_empty_roo_rules_dir_blocks_roo() {
    let sb = Sandbox::new();
    sb.dir(".roo/rules");
    let fallback = sb.file(".clinerules", "team rules");
    assert_eq!(
        blocker(ClientKind::Roo, &sb.cwd),
        Some(Blocker::FallbackFile(fallback))
    );
}

#[test]
fn another_file_in_roo_rules_lifts_the_roo_blocker() {
    let sb = Sandbox::new();
    sb.file(".roorules", "team rules");
    sb.file(".roo/rules/team.md", "more rules");
    assert_eq!(blocker(ClientKind::Roo, &sb.cwd), None);
}

#[test]
fn cartogs_own_rule_does_not_lift_the_roo_blocker() {
    let sb = Sandbox::new();
    let fallback = sb.file(".roorules", "team rules");
    sb.file(".roo/rules/cartog.md", "cartog rule");
    assert_eq!(
        blocker(ClientKind::Roo, &sb.cwd),
        Some(Blocker::FallbackFile(fallback))
    );
}

#[test]
fn a_hidden_file_does_not_lift_the_roo_blocker() {
    let sb = Sandbox::new();
    let fallback = sb.file(".roorules", "team rules");
    sb.file(".roo/rules/.DS_Store", "");
    assert_eq!(
        blocker(ClientKind::Roo, &sb.cwd),
        Some(Blocker::FallbackFile(fallback))
    );
}

#[test]
fn a_clinerules_file_blocks_cline() {
    let sb = Sandbox::new();
    let file = sb.file(".clinerules", "team rules");
    assert_eq!(
        blocker(ClientKind::Cline, &sb.cwd),
        Some(Blocker::NotADirectory(file))
    );
}

#[test]
fn a_clinerules_directory_does_not_block_cline() {
    let sb = Sandbox::new();
    sb.file(".clinerules/team.md", "team rules");
    assert_eq!(blocker(ClientKind::Cline, &sb.cwd), None);
}

#[test]
fn a_rules_dir_component_that_is_a_file_blocks_the_agent() {
    let sb = Sandbox::new();
    let file = sb.file(".cursor/rules", "");
    assert_eq!(
        blocker(ClientKind::Cursor, &sb.cwd),
        Some(Blocker::NotADirectory(file))
    );
}

#[test]
fn a_clean_repo_blocks_no_agent() {
    let sb = Sandbox::new();
    let blocked: Vec<ClientKind> = RULE_AGENTS
        .into_iter()
        .filter(|&kind| blocker(kind, &sb.cwd).is_some())
        .collect();
    assert_eq!(blocked, vec![]);
}

#[test]
fn a_subdirectory_does_not_lift_the_roo_blocker() {
    let sb = Sandbox::new();
    let fallback = sb.file(".roorules", "team rules");
    sb.file(".roo/rules/team/more.md", "more rules");
    assert_eq!(
        blocker(ClientKind::Roo, &sb.cwd),
        Some(Blocker::FallbackFile(fallback))
    );
}
