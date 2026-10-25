use crate::commands::ide::unmerge::*;
use crate::commands::ide::MergeStrategy;
use serde_json::{json, Value};

fn removed(r: Removal) -> String {
    match r {
        Removal::Removed(s) => s,
        Removal::NotWired => panic!("expected Removed, got NotWired"),
    }
}

fn json_of(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn removes_cartog_from_mcp_servers_and_keeps_other_servers_in_order() {
    let existing = r#"{"theme": "dark", "mcpServers": {"cartog": {"command": "cartog"}, "a": {"command": "a"}, "b": {"command": "b"}}}"#;
    let out = removed(remove_entry(existing, MergeStrategy::McpServers, "").unwrap());
    let v = json_of(&out);
    let keys: Vec<_> = v["mcpServers"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec!["a", "b"]);
    assert_eq!(v["theme"], "dark");
}

#[test]
fn removes_cartog_from_the_opencode_and_kilo_mcp_map() {
    let existing = r#"{"mcp": {"cartog": {"type": "local", "command": ["cartog", "serve"]}}}"#;
    let out = removed(remove_entry(existing, MergeStrategy::Mcp, "").unwrap());
    assert_eq!(json_of(&out), json!({"mcp": {}}));
}

#[test]
fn removes_cartog_from_zed_context_servers() {
    let existing = r#"{"context_servers": {"cartog": {"command": "cartog"}}, "theme": "x"}"#;
    let out = removed(remove_entry(existing, MergeStrategy::ContextServers, "").unwrap());
    assert_eq!(json_of(&out), json!({"context_servers": {}, "theme": "x"}));
}

#[test]
fn removes_cartog_from_vscode_servers() {
    let existing = r#"{"servers": {"cartog": {"type": "stdio", "command": "cartog"}}}"#;
    let out = removed(remove_entry(existing, MergeStrategy::VsCodeServers, "").unwrap());
    assert_eq!(json_of(&out), json!({"servers": {}}));
}

#[test]
fn removes_a_user_edited_toggle_entry_with_approvals() {
    let existing = r#"{"mcpServers": {"cartog": {"command": "cartog", "args": ["serve", "--custom"], "disabled": true, "alwaysAllow": ["cartog_search"], "env": {"A": "1"}}}}"#;
    let out = removed(remove_entry(existing, MergeStrategy::McpServersToggle, "").unwrap());
    assert_eq!(json_of(&out), json!({"mcpServers": {}}));
}

#[test]
fn removed_json_is_pretty_printed_with_a_trailing_newline() {
    let existing = r#"{"mcpServers": {"cartog": {}, "a": {}}}"#;
    let out = removed(remove_entry(existing, MergeStrategy::CopilotLocal, "").unwrap());
    assert_eq!(out, "{\n  \"mcpServers\": {\n    \"a\": {}\n  }\n}\n");
}

#[test]
fn json_without_the_container_is_not_wired() {
    let r = remove_entry(r#"{"theme": "dark"}"#, MergeStrategy::McpServers, "").unwrap();
    assert_eq!(r, Removal::NotWired);
}

#[test]
fn json_without_a_cartog_entry_is_not_wired() {
    let r = remove_entry(
        r#"{"mcpServers": {"other": {}}}"#,
        MergeStrategy::McpServers,
        "",
    )
    .unwrap();
    assert_eq!(r, Removal::NotWired);
}

#[test]
fn empty_file_is_not_wired() {
    let r = remove_entry("  \n", MergeStrategy::McpServers, "").unwrap();
    assert_eq!(r, Removal::NotWired);
}

#[test]
fn invalid_json_is_an_error() {
    let err = remove_entry("// note\n{}", MergeStrategy::Mcp, "").unwrap_err();
    assert!(err.to_string().contains("valid JSON"), "got: {err}");
}

#[test]
fn non_object_json_root_is_an_error() {
    assert!(remove_entry("[1, 2]", MergeStrategy::McpServers, "").is_err());
}

#[test]
fn non_object_server_map_is_an_error() {
    let err =
        remove_entry(r#"{"mcpServers": "cartog"}"#, MergeStrategy::McpServers, "").unwrap_err();
    assert!(err.to_string().contains("refusing"), "got: {err}");
}

const CODEX: &str = r#"# my codex config
model = "o4"

# keep this server
[mcp_servers.other]
command = "other"

[mcp_servers.cartog-repo-abcd1234]
command = "cartog"
args = ["serve", "--watch"]

# second project
[mcp_servers.cartog-other-ffff0000]
command = "cartog"
args = ["serve"]
"#;

#[test]
fn codex_removes_only_the_given_section() {
    let out =
        removed(remove_entry(CODEX, MergeStrategy::CodexToml, "cartog-repo-abcd1234").unwrap());
    assert!(!out.contains("cartog-repo-abcd1234"), "{out}");
    assert!(out.contains("[mcp_servers.cartog-other-ffff0000]"), "{out}");
}

#[test]
fn codex_keeps_other_sections_and_comments_byte_identical() {
    let out =
        removed(remove_entry(CODEX, MergeStrategy::CodexToml, "cartog-repo-abcd1234").unwrap());
    assert!(out.starts_with("# my codex config\nmodel = \"o4\"\n\n# keep this server\n[mcp_servers.other]\ncommand = \"other\"\n"), "{out}");
    assert!(out.ends_with("# second project\n[mcp_servers.cartog-other-ffff0000]\ncommand = \"cartog\"\nargs = [\"serve\"]\n"), "{out}");
}

#[test]
fn codex_without_the_section_is_not_wired() {
    let r = remove_entry(CODEX, MergeStrategy::CodexToml, "cartog-missing-00000000").unwrap();
    assert_eq!(r, Removal::NotWired);
}

#[test]
fn codex_without_mcp_servers_is_not_wired() {
    let r = remove_entry(
        "model = \"o4\"\n",
        MergeStrategy::CodexToml,
        "cartog-repo-abcd1234",
    )
    .unwrap();
    assert_eq!(r, Removal::NotWired);
}

#[test]
fn invalid_toml_is_an_error() {
    let err = remove_entry("[mcp_servers", MergeStrategy::CodexToml, "x").unwrap_err();
    assert!(err.to_string().contains("valid TOML"), "got: {err}");
}

#[test]
fn hermes_removes_cartog_and_keeps_other_servers() {
    let existing = "model: x\nmcp_servers:\n  other:\n    command: other\n  cartog:\n    command: cartog\n    args:\n    - serve\n";
    let out = removed(remove_entry(existing, MergeStrategy::HermesYaml, "").unwrap());
    assert_eq!(
        out,
        "model: x\nmcp_servers:\n  other:\n    command: other\n"
    );
}

#[test]
fn hermes_without_cartog_is_not_wired() {
    let r = remove_entry(
        "mcp_servers:\n  other:\n    command: x\n",
        MergeStrategy::HermesYaml,
        "",
    )
    .unwrap();
    assert_eq!(r, Removal::NotWired);
}

#[test]
fn invalid_yaml_is_an_error() {
    let err = remove_entry("key: [unclosed", MergeStrategy::HermesYaml, "").unwrap_err();
    assert!(err.to_string().contains("valid YAML"), "got: {err}");
}

#[test]
fn codex_keeps_a_comment_written_above_the_removed_section() {
    let existing = "model = \"o4\"\n\n# ---- my MCP servers ----\n[mcp_servers.cartog-repo-abcd1234]\ncommand = \"cartog\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
    let out =
        removed(remove_entry(existing, MergeStrategy::CodexToml, "cartog-repo-abcd1234").unwrap());
    assert_eq!(
        out,
        "model = \"o4\"\n\n# ---- my MCP servers ----\n\n[mcp_servers.other]\ncommand = \"x\"\n"
    );
}

#[test]
fn codex_comment_above_the_last_section_moves_to_the_end() {
    let existing = "[mcp_servers.other]\ncommand = \"x\"\n\n# cartog for this repo\n[mcp_servers.cartog-repo-abcd1234]\ncommand = \"cartog\"\n";
    let out =
        removed(remove_entry(existing, MergeStrategy::CodexToml, "cartog-repo-abcd1234").unwrap());
    assert!(
        out.starts_with("[mcp_servers.other]\ncommand = \"x\"\n"),
        "{out}"
    );
    assert!(out.contains("# cartog for this repo"), "{out}");
    assert!(!out.contains("cartog-repo-abcd1234"), "{out}");
}
