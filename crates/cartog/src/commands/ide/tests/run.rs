use crate::cli::{ClientKind, IdeScope};
use crate::commands::ide::catalogue::*;
use crate::commands::ide::rules::{apply_rule_plan, plan_rules, RuleSelection};
use crate::commands::ide::run::*;
use crate::commands::ide::{ClientSpec, Detection, FileKind, IdeStatus, MergeStrategy, Scope};
use std::fs;
use std::path::{Path, PathBuf};
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
fn install_steps_are_tagged_as_mcp_configs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let spec = project_spec(tmp.path().join("mcp.json"));
    let step = process_spec(&spec, tmp.path(), false, true, false);
    assert_eq!(step.file_kind, FileKind::McpConfig);
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
fn run_ide_with_no_matching_row_reports_no_step() {
    let tmp = tempfile::TempDir::new().unwrap();
    let report = run_ide(
        Some(ClientKind::Codex),
        IdeScope::Project,
        false,
        true,
        false,
        tmp.path(),
        &HomeDirs::default(),
    )
    .unwrap();
    assert_eq!(report.steps.len(), 0);
}

#[test]
fn a_rule_target_satisfies_the_no_match_check() {
    assert!(ensure_ide_targets(Some(ClientKind::Windsurf), IdeScope::Project, 0, 1).is_ok());
}

#[test]
fn rules_at_user_scope_are_rejected() {
    let err = check_rules_scope(true, IdeScope::User, Path::new("/repo"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("project files"), "got: {err}");
}

#[test]
fn user_scope_without_rules_is_accepted() {
    assert!(check_rules_scope(false, IdeScope::User, Path::new("/repo"), None).is_ok());
}

#[test]
fn rules_from_the_home_directory_are_refused() {
    let home = tempfile::TempDir::new().unwrap();
    let err = check_rules_scope(true, IdeScope::All, home.path(), Some(home.path()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("home directory"), "got: {err}");
}

#[test]
fn rules_from_a_project_under_home_are_accepted() {
    let home = tempfile::TempDir::new().unwrap();
    let project = home.path().join("code/app");
    fs::create_dir_all(&project).unwrap();
    assert!(check_rules_scope(true, IdeScope::All, &project, Some(home.path())).is_ok());
}

#[test]
fn the_home_directory_without_rules_is_accepted() {
    let home = tempfile::TempDir::new().unwrap();
    assert!(check_rules_scope(false, IdeScope::All, home.path(), Some(home.path())).is_ok());
}

fn rule_sandbox() -> (tempfile::TempDir, PathBuf, HomeDirs) {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    (tmp, cwd, HomeDirs::default())
}

#[test]
fn rule_steps_follow_the_mcp_steps() {
    let (_tmp, cwd, homes) = rule_sandbox();
    let named = [ClientKind::Cursor];
    let plan = plan_rules(Some(RuleSelection::Named(&named)), &cwd, &homes, None);
    let mut report = run_ide_for_clients(
        &[(ClientKind::Cursor, Scope::Project)],
        false,
        false,
        &cwd,
        &homes,
    )
    .unwrap();
    apply_rule_plan(&mut report, plan, &cwd, false);
    let kinds: Vec<FileKind> = report.steps.iter().map(|s| s.file_kind).collect();
    assert_eq!(
        (kinds, report.summary.created),
        (vec![FileKind::McpConfig, FileKind::Rule], 2)
    );
}

#[test]
fn ide_scope_mismatch_error_names_the_cli_slug() {
    let err = ensure_ide_targets(Some(ClientKind::Codex), IdeScope::Project, 0, 0)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("codex") && err.contains("project"),
        "got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn atomic_write_keeps_the_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    fs::write(&cfg, "a = 1\n").unwrap();
    fs::set_permissions(&cfg, fs::Permissions::from_mode(0o600)).unwrap();

    atomic_write(&cfg, "a = 2\n").unwrap();

    let mode = fs::metadata(&cfg).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn atomic_write_edits_a_symlinked_config_at_its_target() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dotfiles = tmp.path().join("dotfiles");
    let home = tmp.path().join("home");
    fs::create_dir_all(&dotfiles).unwrap();
    fs::create_dir_all(&home).unwrap();
    let target = dotfiles.join("settings.json");
    let link = home.join("settings.json");
    fs::write(&target, "{}\n").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    atomic_write(&link, "{\"a\": 1}\n").unwrap();

    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "{\"a\": 1}\n");
}

#[cfg(unix)]
#[test]
fn temp_file_is_created_with_the_final_mode_before_any_content() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("config.toml.cartog-1.tmp");
    let perms = fs::Permissions::from_mode(0o600);

    let file = create_tmp(&path, Some(&perms)).unwrap();

    assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(file.metadata().unwrap().len(), 0);
}

#[cfg(unix)]
#[test]
fn atomic_write_removes_its_temp_file_when_the_rename_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("config.json");
    // A non-empty directory at the target path makes the final rename fail.
    fs::create_dir_all(target.join("occupied")).unwrap();

    assert!(atomic_write(&target, "{}\n").is_err());

    let leftovers: Vec<String> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".cartog-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
