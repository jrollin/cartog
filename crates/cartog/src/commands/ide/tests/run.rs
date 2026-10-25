use crate::cli::{ClientKind, IdeScope};
use crate::commands::ide::catalogue::*;
use crate::commands::ide::run::*;
use crate::commands::ide::{ClientSpec, Detection, IdeStatus, MergeStrategy, Scope};
use std::fs;
use std::path::PathBuf;
#[test]
fn install_filter_single_client_returns_only_that_kind() {
    let chosen = filter_catalogue_by_clients(&[ClientKind::Windsurf], IdeScope::All);
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].0, ClientKind::Windsurf);
}

#[test]
fn install_filter_multiple_clients_returns_each_in_catalogue_order() {
    let chosen = filter_catalogue_by_clients(
        &[ClientKind::Cursor, ClientKind::Vscode, ClientKind::Codex],
        IdeScope::All,
    );
    // All three requested clients are present.
    let kinds: Vec<_> = chosen.iter().map(|(k, _)| *k).collect();
    assert!(kinds.contains(&ClientKind::Cursor));
    assert!(kinds.contains(&ClientKind::Vscode));
    assert!(kinds.contains(&ClientKind::Codex));
    // Cursor project+user (2) + VS Code project+user (2) + Codex (1) = 5.
    assert_eq!(chosen.len(), 5);
}

#[test]
fn install_filter_claude_code_returns_both_project_and_user_scopes() {
    let chosen = filter_catalogue_by_clients(&[ClientKind::ClaudeCode], IdeScope::All);
    // Claude Code has both Project and User entries; install must wire both.
    assert_eq!(chosen.len(), 2);
    let scopes: Vec<_> = chosen.iter().map(|(_, s)| *s).collect();
    assert!(scopes.contains(&Scope::Project));
    assert!(scopes.contains(&Scope::User));
}

#[test]
fn install_filter_respects_project_scope() {
    // Cursor exists only at project scope; codex only at user scope.
    // --scope project must drop the user-only entry.
    let chosen =
        filter_catalogue_by_clients(&[ClientKind::Cursor, ClientKind::Codex], IdeScope::Project);
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].0, ClientKind::Cursor);
    assert_eq!(chosen[0].1, Scope::Project);
}

#[test]
fn install_filter_empty_clients_returns_empty() {
    // Empty positional list is handled by the caller (falls back to
    // `run_ide(None, ...)`); the filter helper itself returns nothing.
    let chosen = filter_catalogue_by_clients(&[], IdeScope::All);
    assert!(chosen.is_empty());
}

#[test]
fn dedupe_drops_repeats_and_reports_them() {
    let (unique, dropped) = dedupe_preserving_order(vec![
        ClientKind::Cursor,
        ClientKind::Vscode,
        ClientKind::Cursor,
        ClientKind::Cursor,
        ClientKind::Codex,
    ]);
    assert_eq!(
        unique,
        vec![ClientKind::Cursor, ClientKind::Vscode, ClientKind::Codex]
    );
    assert_eq!(dropped, vec![ClientKind::Cursor, ClientKind::Cursor]);
}

#[test]
fn dedupe_preserves_first_occurrence_order() {
    let (unique, dropped) = dedupe_preserving_order(vec![
        ClientKind::Vscode,
        ClientKind::Cursor,
        ClientKind::Vscode,
    ]);
    assert_eq!(unique, vec![ClientKind::Vscode, ClientKind::Cursor]);
    assert_eq!(dropped, vec![ClientKind::Vscode]);
}

#[test]
fn dedupe_empty_input_returns_two_empty_vecs() {
    let (unique, dropped) = dedupe_preserving_order(Vec::new());
    assert!(unique.is_empty());
    assert!(dropped.is_empty());
}

#[test]
fn install_filter_user_only_client_at_project_scope_yields_empty() {
    // Reproduces the F2 review finding: `cartog install --scope project codex`
    // would silently succeed with "0 clients" before the bail was added.
    // Codex is user-only, so the filter yields an empty vec — cmd_install
    // bails with an error message instead of running.
    let chosen = filter_catalogue_by_clients(&[ClientKind::Codex], IdeScope::Project);
    assert!(
        chosen.is_empty(),
        "codex has no Project entry in the catalogue"
    );
}

#[test]
fn claude_code_args_include_watch_unless_no_watch() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let with = build_specs(
        Some(ClientKind::ClaudeCode),
        IdeScope::Project,
        false,
        &tmp,
        &homes,
    );
    let without = build_specs(
        Some(ClientKind::ClaudeCode),
        IdeScope::Project,
        true,
        &tmp,
        &homes,
    );
    assert_eq!(with[0].args, vec!["serve", "--watch"]);
    assert_eq!(without[0].args, vec!["serve"]);
}

#[test]
fn non_claude_clients_also_get_watch_by_default() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let with = build_specs(
        Some(ClientKind::Cursor),
        IdeScope::Project,
        false,
        &tmp,
        &homes,
    );
    let without = build_specs(
        Some(ClientKind::Cursor),
        IdeScope::Project,
        true,
        &tmp,
        &homes,
    );
    assert_eq!(with[0].args, vec!["serve", "--watch"]);
    assert_eq!(without[0].args, vec!["serve"]);
}

// ── process_spec: merge + write core (non-interactive) ────────────

fn project_spec(path: PathBuf) -> ClientSpec {
    ClientSpec {
        kind: ClientKind::Cursor,
        scope: Scope::Project,
        path,
        strategy: MergeStrategy::McpServers,
        args: vec!["serve".into()],
        detection: Detection::ParentDir,
        requires: None,
    }
}

#[test]
fn process_spec_creates_config_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("mcp.json");
    let spec = project_spec(cfg.clone());

    let step = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(step.status, IdeStatus::Created);
    assert!(cfg.exists(), "config file written to disk");
    let written = fs::read_to_string(&cfg).unwrap();
    assert!(written.contains("mcpServers"), "wrote the mcpServers entry");
    assert!(written.contains("cartog"), "wrote the cartog server");
}

#[test]
fn process_spec_unchanged_on_second_run() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("mcp.json");
    let spec = project_spec(cfg);

    let first = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(first.status, IdeStatus::Created);
    let second = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(
        second.status,
        IdeStatus::Unchanged,
        "re-running with identical config is a no-op"
    );
}

#[test]
fn process_spec_dry_run_returns_diff_without_writing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("mcp.json");
    let spec = project_spec(cfg.clone());

    let step = process_spec(&spec, tmp.path(), false, true, false);
    assert_eq!(step.status, IdeStatus::Created);
    assert!(step.diff.is_some(), "dry-run carries a before/after diff");
    assert!(!cfg.exists(), "dry-run must not write the file");
}

#[test]
fn process_spec_skips_undetected_user_client_in_auto_mode() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Cursor has no CLI binary, and this user-scoped parent dir is absent →
    // undetected. In auto mode (no explicit client) it is skipped.
    let cfg = tmp.path().join("no-such-dir").join("config.json");
    let mut spec = project_spec(cfg.clone());
    spec.scope = Scope::User;

    let step = process_spec(&spec, tmp.path(), false, false, true);
    assert_eq!(step.status, IdeStatus::Skipped);
    assert!(!cfg.exists(), "nothing written for a not-installed client");
}

#[test]
fn process_spec_wires_undetected_user_client_when_explicit() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Same undetected client, but auto_only = false (the user named it) →
    // wired regardless of detection. The config dir is absent and is
    // created from scratch: an explicit `cartog install X` materializes the
    // client's config even when X isn't installed yet (intended).
    let cfg = tmp.path().join("brand-new-dir").join("config.json");
    assert!(!cfg.parent().unwrap().exists(), "parent dir starts absent");
    let mut spec = project_spec(cfg.clone());
    spec.scope = Scope::User;

    let step = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(step.status, IdeStatus::Created);
    assert!(cfg.exists(), "explicitly-requested client is wired");
    assert!(
        cfg.parent().unwrap().exists(),
        "config dir created on demand"
    );
}

#[test]
fn process_spec_skips_when_existing_file_is_malformed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("mcp.json");
    fs::write(&cfg, "{ this is not valid json").unwrap();
    let spec = project_spec(cfg.clone());

    let step = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(
        step.status,
        IdeStatus::Skipped,
        "a malformed file is left untouched, not overwritten"
    );
    assert_eq!(
        fs::read_to_string(&cfg).unwrap(),
        "{ this is not valid json",
        "original content preserved"
    );
}

#[test]
fn process_spec_leaves_commented_jsonc_untouched_with_a_manual_hint() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("kilo.jsonc");
    let original = "// my Kilo config\n{ \"model\": \"x\" }\n";
    fs::write(&cfg, original).unwrap();
    let spec = ClientSpec {
        kind: ClientKind::Kilo,
        scope: Scope::Project,
        path: cfg.clone(),
        strategy: MergeStrategy::Mcp,
        args: vec!["serve".into()],
        detection: Detection::ParentDir,
        requires: None,
    };

    let step = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(step.status, IdeStatus::Skipped);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), original);
    assert!(step.message.contains("JSONC"), "got: {}", step.message);
    assert!(!step.message.contains("remove it"), "got: {}", step.message);
}

fn spec_at(path: PathBuf, strategy: MergeStrategy) -> ClientSpec {
    ClientSpec {
        kind: ClientKind::Roo,
        scope: Scope::User,
        path,
        strategy,
        args: vec!["serve".into()],
        detection: Detection::ParentDir,
        requires: None,
    }
}

#[test]
fn process_spec_skips_a_named_extension_row_without_the_vscode_user_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vscode_user = tmp.path().join("Code/User");
    let cfg =
        vscode_user.join("globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json");
    let mut spec = spec_at(cfg.clone(), MergeStrategy::McpServersToggle);
    spec.requires = Some(vscode_user);

    let step = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(step.status, IdeStatus::Skipped);
    assert!(
        !tmp.path().join("Code").exists(),
        "VS Code dir must not be created"
    );
}

#[test]
fn process_spec_writes_a_named_extension_row_when_vscode_user_dir_exists() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vscode_user = tmp.path().join("Code/User");
    fs::create_dir_all(&vscode_user).unwrap();
    let cfg =
        vscode_user.join("globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json");
    let mut spec = spec_at(cfg.clone(), MergeStrategy::McpServersToggle);
    spec.requires = Some(vscode_user);

    let step = process_spec(&spec, tmp.path(), false, false, false);
    assert_eq!(step.status, IdeStatus::Created);
    assert!(cfg.exists());
}

#[test]
fn process_spec_jsonc_with_a_wrong_shape_gets_the_generic_hint() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("kilo.jsonc");
    fs::write(&cfg, r#"{"mcp": "not-an-object"}"#).unwrap();
    let step = process_spec(
        &spec_at(cfg, MergeStrategy::Mcp),
        tmp.path(),
        false,
        false,
        false,
    );
    assert_eq!(step.status, IdeStatus::Skipped);
    assert!(!step.message.contains("JSONC"), "got: {}", step.message);
}

#[test]
fn process_spec_jsonc_with_a_trailing_comma_gets_the_jsonc_hint() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("kilo.jsonc");
    fs::write(&cfg, "{\"mcp\": {},}").unwrap();
    let step = process_spec(
        &spec_at(cfg, MergeStrategy::Mcp),
        tmp.path(),
        false,
        false,
        false,
    );
    assert!(step.message.contains("JSONC"), "got: {}", step.message);
}

#[test]
fn process_spec_notes_a_cartog_entry_disabled_in_the_client() {
    let tmp = tempfile::TempDir::new().unwrap();
    let toggle = tmp.path().join("cline.json");
    fs::write(
        &toggle,
        r#"{"mcpServers": {"cartog": {"command": "cartog", "args": ["serve"], "disabled": true}}}"#,
    )
    .unwrap();
    let step = process_spec(
        &spec_at(toggle, MergeStrategy::McpServersToggle),
        tmp.path(),
        false,
        false,
        false,
    );
    assert_eq!(step.status, IdeStatus::Unchanged);
    assert!(step.message.contains("disabled"), "got: {}", step.message);

    let mcp = tmp.path().join("kilo.json");
    fs::write(
        &mcp,
        r#"{"mcp": {"cartog": {"type": "local", "command": ["cartog", "serve"], "enabled": false}}}"#,
    )
    .unwrap();
    let step = process_spec(
        &spec_at(mcp, MergeStrategy::Mcp),
        tmp.path(),
        false,
        false,
        false,
    );
    assert!(step.message.contains("disabled"), "got: {}", step.message);
}

#[test]
fn process_spec_enabled_entry_has_no_disabled_note() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("cline.json");
    let step = process_spec(
        &spec_at(cfg, MergeStrategy::McpServersToggle),
        tmp.path(),
        false,
        false,
        false,
    );
    assert!(!step.message.contains("disabled"), "got: {}", step.message);
}

#[test]
fn install_scope_mismatch_error_names_the_cli_slugs() {
    let err = cmd_install(
        vec![ClientKind::CopilotCli],
        IdeScope::Project,
        true,
        false,
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("copilot-cli") && err.contains("project"),
        "got: {err}"
    );
}

#[test]
fn ide_scope_mismatch_error_names_the_cli_slug() {
    let tmp = tempfile::TempDir::new().unwrap();
    let err = run_ide(
        Some(ClientKind::Codex),
        IdeScope::Project,
        false,
        true,
        false,
        tmp.path(),
        &HomeDirs::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("codex") && err.contains("project"),
        "got: {err}"
    );
}
