use super::{args, args_watch};
use crate::commands::ide::merge::*;
use crate::commands::ide::{Action, MergeStrategy};
use serde_json::{json, Value};
#[test]
fn merge_mcp_servers_empty_file_creates_entry() {
    let o = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
    assert_eq!(o.action, Action::Created);
    let v: Value = serde_json::from_str(&o.new_json).unwrap();
    assert_eq!(v["mcpServers"]["cartog"]["command"], "cartog");
    assert_eq!(v["mcpServers"]["cartog"]["args"], json!(["serve"]));
}

#[test]
fn merge_mcp_servers_preserves_other_servers() {
    let existing = r#"{"mcpServers": {"other": {"command": "x", "args": ["a"]}}}"#;
    let o = merge_entry(Some(existing), MergeStrategy::McpServers, &args()).unwrap();
    assert_eq!(o.action, Action::Updated);
    let v: Value = serde_json::from_str(&o.new_json).unwrap();
    assert_eq!(v["mcpServers"]["other"]["command"], "x");
    assert_eq!(v["mcpServers"]["cartog"]["command"], "cartog");
}

#[test]
fn merge_mcp_servers_idempotent() {
    let first = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
    let second = merge_entry(Some(&first.new_json), MergeStrategy::McpServers, &args()).unwrap();
    assert_eq!(second.action, Action::Unchanged);
    assert_eq!(first.new_json, second.new_json);
}

#[test]
fn merge_mcp_servers_updates_when_args_change() {
    let first = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
    let second = merge_entry(
        Some(&first.new_json),
        MergeStrategy::McpServers,
        &args_watch(),
    )
    .unwrap();
    assert_eq!(second.action, Action::Updated);
    let v: Value = serde_json::from_str(&second.new_json).unwrap();
    assert_eq!(
        v["mcpServers"]["cartog"]["args"],
        json!(["serve", "--watch"])
    );
}

#[test]
fn merge_opencode_strategy_uses_mcp_local_command_array() {
    let o = merge_entry(None, MergeStrategy::Mcp, &args()).unwrap();
    let v: Value = serde_json::from_str(&o.new_json).unwrap();
    assert_eq!(v["mcp"]["cartog"]["type"], "local");
    assert_eq!(v["mcp"]["cartog"]["enabled"], true);
    assert_eq!(v["mcp"]["cartog"]["command"], json!(["cartog", "serve"]));
}

#[test]
fn merge_zed_strategy_uses_context_servers_with_flat_command() {
    let o = merge_entry(None, MergeStrategy::ContextServers, &args()).unwrap();
    let v: Value = serde_json::from_str(&o.new_json).unwrap();
    assert_eq!(v["context_servers"]["cartog"]["command"], "cartog");
    assert_eq!(v["context_servers"]["cartog"]["args"], json!(["serve"]));
}

#[test]
fn merge_invalid_json_returns_parse_error() {
    let err = merge_entry(Some("{not json"), MergeStrategy::McpServers, &args()).unwrap_err();
    assert!(err.to_string().contains("valid JSON"));
}

#[test]
fn merge_refuses_when_top_level_key_is_string() {
    // User somehow set mcpServers to a string. Don't clobber.
    let existing = r#"{"mcpServers": "/etc/mcp/something.json"}"#;
    let err = merge_entry(Some(existing), MergeStrategy::McpServers, &args()).unwrap_err();
    assert!(
        err.to_string().contains("refusing to overwrite"),
        "expected refusal: {err}"
    );
}

#[test]
fn merge_refuses_when_top_level_key_is_array() {
    let existing = r#"{"context_servers": ["one", "two"]}"#;
    let err = merge_entry(Some(existing), MergeStrategy::ContextServers, &args()).unwrap_err();
    assert!(err.to_string().contains("refusing to overwrite"));
}

#[test]
fn merge_treats_null_top_level_key_as_absent() {
    // `null` is functionally an absent key; replace with an empty object.
    let existing = r#"{"servers": null}"#;
    let outcome = merge_entry(Some(existing), MergeStrategy::VsCodeServers, &args()).unwrap();
    let v: Value = serde_json::from_str(&outcome.new_json).unwrap();
    assert_eq!(v["servers"]["cartog"]["command"], "cartog");
}

#[test]
fn merge_preserves_user_key_order() {
    // With preserve_order, an existing custom key order survives unrelated
    // mutations. Insert cartog into an mcpServers that already has `zzz`
    // and `aaa` — they should keep their existing order after our merge.
    let existing = r#"{
  "mcpServers": {
    "zzz": {"command": "z"},
    "aaa": {"command": "a"}
  }
}"#;
    let outcome = merge_entry(Some(existing), MergeStrategy::McpServers, &args()).unwrap();
    let zzz_pos = outcome.new_json.find("\"zzz\"").unwrap();
    let aaa_pos = outcome.new_json.find("\"aaa\"").unwrap();
    assert!(
        zzz_pos < aaa_pos,
        "expected zzz before aaa (user-defined order), got:\n{}",
        outcome.new_json
    );
}

#[test]
fn merge_codex_toml_refuses_when_section_is_not_a_table() {
    let existing = "[mcp_servers]\ncartog-x = \"oops\"\n";
    let err = merge_codex_toml(Some(existing), &args(), "cartog-x").unwrap_err();
    assert!(err.to_string().contains("refusing to overwrite"));
}

#[test]
fn merge_codex_toml_refuses_when_mcp_servers_is_not_a_table() {
    let existing = "mcp_servers = \"not a table\"\n";
    let err = merge_codex_toml(Some(existing), &args(), "cartog-x").unwrap_err();
    assert!(err.to_string().contains("refusing to overwrite"));
}

#[test]
fn merge_vscode_strategy_uses_servers_key_and_stdio_type() {
    let o = merge_entry(None, MergeStrategy::VsCodeServers, &args()).unwrap();
    let v: Value = serde_json::from_str(&o.new_json).unwrap();
    assert_eq!(v["servers"]["cartog"]["type"], "stdio");
    assert_eq!(v["servers"]["cartog"]["command"], "cartog");
    assert!(v.get("mcpServers").is_none(), "must not write mcpServers");
}

#[test]
fn merge_codex_toml_creates_section_under_mcp_servers() {
    let outcome = merge_codex_toml(None, &args(), "cartog-myproj-deadbeef").unwrap();
    assert_eq!(outcome.action, Action::Created);
    assert!(outcome
        .new_json
        .contains("[mcp_servers.cartog-myproj-deadbeef]"));
    assert!(outcome.new_json.contains("command = \"cartog\""));
    assert!(outcome.new_json.contains("args = [\"serve\"]"));
}

#[test]
fn merge_codex_toml_preserves_other_servers_and_comments() {
    let existing = "# user-managed file\n\
            [mcp_servers.other]\n\
            command = \"other\"\n\
            args = [\"--flag\"]\n";
    let outcome = merge_codex_toml(Some(existing), &args(), "cartog-x").unwrap();
    assert_eq!(outcome.action, Action::Updated);
    assert!(outcome.new_json.contains("# user-managed file"));
    assert!(outcome.new_json.contains("[mcp_servers.other]"));
    assert!(outcome.new_json.contains("[mcp_servers.cartog-x]"));
}

#[test]
fn merge_codex_toml_idempotent_with_same_section() {
    let first = merge_codex_toml(None, &args(), "cartog-x").unwrap();
    let second = merge_codex_toml(Some(&first.new_json), &args(), "cartog-x").unwrap();
    assert_eq!(second.action, Action::Unchanged);
}

#[test]
fn merge_codex_toml_rejects_invalid_toml() {
    let err = merge_codex_toml(Some("[not [valid toml"), &args(), "cartog-x").unwrap_err();
    assert!(err.to_string().contains("valid TOML"));
}

#[test]
fn codex_section_name_is_deterministic_and_slug_safe() {
    let p = std::env::temp_dir();
    let a = codex_section_name(&p);
    let b = codex_section_name(&p);
    assert_eq!(a, b, "section name must be stable for the same dir");
    assert!(a.starts_with("cartog-"));
    // Slug body must only contain alphanumerics and hyphens (TOML bare-key safe).
    assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
}

#[test]
fn merge_empty_string_treated_as_empty_object() {
    let o = merge_entry(Some(""), MergeStrategy::McpServers, &args()).unwrap();
    assert_eq!(o.action, Action::Updated);
}

#[test]
fn merge_hermes_yaml_empty_file_creates_entry() {
    let o = merge_entry(None, MergeStrategy::HermesYaml, &args()).unwrap();
    assert_eq!(o.action, Action::Created);
    assert!(o.new_json.ends_with('\n'), "must terminate with a newline");
    let v: serde_norway::Value = serde_norway::from_str(&o.new_json).unwrap();
    assert_eq!(
        v["mcp_servers"]["cartog"]["command"].as_str(),
        Some("cartog")
    );
    assert_eq!(
        v["mcp_servers"]["cartog"]["args"][0].as_str(),
        Some("serve")
    );
}

#[test]
fn merge_hermes_yaml_preserves_other_servers_and_keys() {
    let existing = "model: hermes-4\n\
            mcp_servers:\n  \
              filesystem:\n    \
                command: npx\n    \
                args: [\"-y\", \"server-filesystem\"]\n";
    let o = merge_entry(Some(existing), MergeStrategy::HermesYaml, &args()).unwrap();
    assert_eq!(o.action, Action::Updated);
    let v: serde_norway::Value = serde_norway::from_str(&o.new_json).unwrap();
    assert_eq!(v["model"].as_str(), Some("hermes-4"));
    assert_eq!(
        v["mcp_servers"]["filesystem"]["command"].as_str(),
        Some("npx")
    );
    assert_eq!(
        v["mcp_servers"]["cartog"]["command"].as_str(),
        Some("cartog")
    );
}

#[test]
fn merge_hermes_yaml_idempotent() {
    let first = merge_entry(None, MergeStrategy::HermesYaml, &args()).unwrap();
    let second = merge_entry(Some(&first.new_json), MergeStrategy::HermesYaml, &args()).unwrap();
    assert_eq!(second.action, Action::Unchanged);
    assert_eq!(first.new_json, second.new_json);
}

#[test]
fn merge_hermes_yaml_refuses_when_mcp_servers_is_not_a_mapping() {
    let existing = "mcp_servers: not-a-mapping\n";
    let err = merge_entry(Some(existing), MergeStrategy::HermesYaml, &args()).unwrap_err();
    assert!(
        err.to_string().contains("refusing to overwrite"),
        "expected refusal: {err}"
    );
}

#[test]
fn merge_hermes_yaml_rejects_invalid_yaml() {
    let err = merge_entry(Some("key: [unclosed"), MergeStrategy::HermesYaml, &args()).unwrap_err();
    assert!(err.to_string().contains("valid YAML"), "got: {err}");
}
