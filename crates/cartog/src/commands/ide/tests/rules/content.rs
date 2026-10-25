use crate::cli::ClientKind;
use crate::commands::ide::rules::{
    is_cartog_owned, render, rule_format, RuleFormat, RULE_AGENTS, RULE_BODY,
};
use clap::ValueEnum;
use std::path::Path;

const MARKER: &str = "<!-- Managed by cartog: `cartog ide --rules` updates this file and `cartog uninstall` deletes it. Delete this line to keep your own edits. -->";

fn format(kind: ClientKind) -> RuleFormat {
    rule_format(kind).expect("rule agent has a format")
}

fn rendered(kind: ClientKind) -> String {
    render(&format(kind))
}

#[test]
fn rule_format_maps_each_rule_agent_to_its_file_and_no_other_client() {
    let paths: Vec<(ClientKind, Option<String>)> = ClientKind::value_variants()
        .iter()
        .map(|&kind| {
            let path = rule_format(kind).map(|f| f.path_in(Path::new("")).display().to_string());
            (kind, path)
        })
        .filter(|(_, path)| path.is_some())
        .collect();
    let expected = [
        (ClientKind::Antigravity, ".agents/rules/cartog.md"),
        (ClientKind::ClaudeCode, ".claude/rules/cartog.md"),
        (ClientKind::Cline, ".clinerules/cartog.md"),
        (ClientKind::Cursor, ".cursor/rules/cartog.mdc"),
        (ClientKind::Kiro, ".kiro/steering/cartog.md"),
        (ClientKind::Roo, ".roo/rules/cartog.md"),
        (
            ClientKind::Vscode,
            ".github/instructions/cartog.instructions.md",
        ),
        (ClientKind::Windsurf, ".windsurf/rules/cartog.md"),
    ]
    .map(|(kind, path)| (kind, Some(path.to_string())));
    assert_eq!(paths, expected);
}

#[test]
fn rule_agents_lists_every_client_with_a_format_in_report_order() {
    assert_eq!(
        RULE_AGENTS,
        [
            ClientKind::Cursor,
            ClientKind::Windsurf,
            ClientKind::Kiro,
            ClientKind::Cline,
            ClientKind::Roo,
            ClientKind::Antigravity,
            ClientKind::ClaudeCode,
            ClientKind::Vscode,
        ]
    );
}

#[test]
fn cursor_rule_always_applies() {
    let expected = format!(
        "---\ndescription: Prefer cartog MCP tools over grep for code navigation\n\
         globs:\nalwaysApply: true\n---\n{MARKER}\n\n"
    );
    assert!(rendered(ClientKind::Cursor).starts_with(&expected));
}

#[test]
fn windsurf_rule_is_always_on() {
    let expected = format!("---\ntrigger: always_on\n---\n{MARKER}\n\n");
    assert!(rendered(ClientKind::Windsurf).starts_with(&expected));
}

#[test]
fn antigravity_rule_declares_a_valid_trigger() {
    let expected = format!("---\ntrigger: always_on\n---\n{MARKER}\n\n");
    assert!(rendered(ClientKind::Antigravity).starts_with(&expected));
}

#[test]
fn kiro_steering_is_always_included() {
    let expected = format!("---\ninclusion: always\n---\n{MARKER}\n\n");
    assert!(rendered(ClientKind::Kiro).starts_with(&expected));
}

#[test]
fn vscode_instructions_apply_to_every_file() {
    let expected = format!("---\napplyTo: '**'\n---\n{MARKER}\n\n");
    assert!(rendered(ClientKind::Vscode).starts_with(&expected));
}

#[test]
fn plain_markdown_agents_start_with_the_marker() {
    let starts: Vec<bool> = [ClientKind::Cline, ClientKind::Roo, ClientKind::ClaudeCode]
        .map(|kind| rendered(kind).starts_with(&format!("{MARKER}\n\n")))
        .to_vec();
    assert_eq!(starts, vec![true, true, true]);
}

#[test]
fn every_rule_ends_with_the_shared_body() {
    assert!(RULE_AGENTS
        .iter()
        .all(|&kind| rendered(kind).ends_with(RULE_BODY)));
}

#[test]
fn every_rule_uses_lf_line_endings() {
    assert!(RULE_AGENTS
        .iter()
        .all(|&kind| !rendered(kind).contains('\r')));
}

#[test]
fn every_rule_fits_the_smallest_agent_cap() {
    // Windsurf rejects workspace rules over 12,000 characters, the lowest cap of the eight.
    let longest = RULE_AGENTS
        .iter()
        .map(|&kind| rendered(kind).chars().count())
        .max();
    assert!(longest < Some(12_000), "longest rule: {longest:?} chars");
}

#[test]
fn a_rendered_rule_is_owned_by_cartog() {
    assert!(is_cartog_owned(&rendered(ClientKind::Cursor)));
}

#[test]
fn the_marker_counts_anywhere_in_the_file() {
    let content = format!("# Team rules\n\nOur notes.\n  {MARKER}\n");
    assert!(is_cartog_owned(&content));
}

#[test]
fn a_hand_made_snippet_copy_is_not_owned() {
    assert!(!is_cartog_owned(RULE_BODY));
}

#[test]
fn an_empty_file_is_not_owned() {
    assert!(!is_cartog_owned(""));
}
