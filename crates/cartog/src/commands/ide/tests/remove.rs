use crate::cli::{ClientKind, IdeScope};
use crate::commands::ide::catalogue::HomeDirs;
use crate::commands::ide::merge::codex_section_name;
use crate::commands::ide::picker::RemovalItem;
use crate::commands::ide::remove::*;
use crate::commands::ide::{
    ClientSpec, Detection, FileKind, IdeReport, IdeStatus, IdeStep, IdeSummary, MergeStrategy,
    Operation, Scope,
};
use std::fs;
use std::path::{Path, PathBuf};

fn step(client: &str, status: IdeStatus, message: &str) -> IdeStep {
    IdeStep {
        client: client.into(),
        scope: "user".into(),
        path: format!("/home/u/{client}.json"),
        file_kind: FileKind::McpConfig,
        status,
        message: message.into(),
        diff: None,
    }
}

fn report(operation: Operation, steps: Vec<IdeStep>) -> IdeReport {
    let mut summary = IdeSummary::default();
    for s in &steps {
        summary.record(s.status);
    }
    IdeReport {
        operation,
        dry_run: false,
        steps,
        summary,
    }
}

#[test]
fn summary_record_counts_each_status() {
    let mut s = IdeSummary::default();
    for status in [
        IdeStatus::Created,
        IdeStatus::Updated,
        IdeStatus::Unchanged,
        IdeStatus::Skipped,
        IdeStatus::Error,
        IdeStatus::Removed,
        IdeStatus::Removed,
    ] {
        s.record(status);
    }
    assert_eq!(
        (
            s.total,
            s.created,
            s.updated,
            s.unchanged,
            s.skipped,
            s.error,
            s.removed
        ),
        (7, 1, 1, 1, 1, 1, 2)
    );
}

#[test]
fn remove_report_lists_only_changed_steps_and_a_removal_summary() {
    let r = report(
        Operation::Remove,
        vec![
            step("cursor", IdeStatus::Removed, "removed"),
            step("zed", IdeStatus::Unchanged, "not wired"),
            step("kilo", IdeStatus::Skipped, "not modified (bad)"),
        ],
    );
    assert_eq!(
        r.render_human(),
        "- cursor (user, /home/u/cursor.json): removed\n\
         ! kilo (user, /home/u/kilo.json): not modified (bad)\n\
         \n3 files: 1 removed, 1 not wired, 1 skipped, 0 errors\n"
    );
}

#[test]
fn remove_report_with_nothing_to_remove_says_not_wired() {
    let r = report(
        Operation::Remove,
        vec![step("zed", IdeStatus::Unchanged, "not wired")],
    );
    assert_eq!(
        r.render_human(),
        "cartog is not wired in any config in scope.\n"
    );
}

#[test]
fn install_report_renders_every_step_as_before() {
    let r = report(
        Operation::Install,
        vec![
            step("cursor", IdeStatus::Created, "created"),
            step("zed", IdeStatus::Unchanged, "already up to date"),
        ],
    );
    assert_eq!(
        r.render_human(),
        "+ cursor (user, /home/u/cursor.json): created\n\
         + zed (user, /home/u/zed.json): already up to date\n\
         \n2 files: 1 created, 0 updated, 1 unchanged, 0 skipped, 0 errors\n"
    );
}

#[test]
fn json_report_carries_the_operation_and_removed_count() {
    let r = report(
        Operation::Remove,
        vec![step("cursor", IdeStatus::Removed, "removed")],
    );
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["operation"], "remove");
    assert_eq!(v["summary"]["removed"], 1);
    assert_eq!(v["steps"][0]["status"], "removed");
}

#[test]
fn json_steps_say_an_mcp_config_is_one() {
    let r = report(
        Operation::Install,
        vec![step("cursor", IdeStatus::Created, "created")],
    );
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["steps"][0]["file_kind"], "mcp_config");
}

#[test]
fn json_steps_say_a_rule_file_is_one() {
    let mut rule = step("cursor", IdeStatus::Created, "created");
    rule.file_kind = FileKind::Rule;
    let v = serde_json::to_value(report(Operation::Install, vec![rule])).unwrap();
    assert_eq!(v["steps"][0]["file_kind"], "rule");
}

#[test]
fn a_step_without_a_path_renders_without_one() {
    let mut no_path = step("zed", IdeStatus::Skipped, "no rules directory");
    no_path.scope = "project".into();
    no_path.path = String::new();
    assert!(report(Operation::Install, vec![no_path])
        .render_human()
        .starts_with("! zed (project): no rules directory\n"));
}

#[test]
fn removal_steps_are_tagged_as_mcp_configs() {
    let dir = tempfile::TempDir::new().unwrap();
    let spec = spec_at(dir.path().join("absent.json"), MergeStrategy::McpServers);
    let step = process_removal(&spec, dir.path(), false);
    assert_eq!(step.file_kind, FileKind::McpConfig);
}

fn spec_at(path: PathBuf, strategy: MergeStrategy) -> ClientSpec {
    ClientSpec {
        kind: ClientKind::Qwen,
        scope: Scope::User,
        path,
        strategy,
        args: vec!["serve".into()],
        detection: Detection::Evidence(Vec::new()),
        requires: None,
    }
}

const WIRED: &str =
    r#"{"mcpServers": {"cartog": {"command": "cartog"}, "other": {"command": "x"}}}"#;

fn kinds_and_scopes(specs: &[ClientSpec]) -> Vec<(ClientKind, Scope)> {
    specs.iter().map(|s| (s.kind, s.scope)).collect()
}

#[test]
fn named_clients_target_every_row_of_those_kinds_in_scope() {
    let specs = removal_targets(
        &[ClientKind::Cursor, ClientKind::Qwen],
        IdeScope::All,
        Path::new("/tmp/proj"),
        &HomeDirs::default(),
    );
    assert_eq!(
        kinds_and_scopes(&specs),
        vec![
            (ClientKind::Cursor, Scope::Project),
            (ClientKind::Cursor, Scope::User),
            (ClientKind::Qwen, Scope::Project),
            (ClientKind::Qwen, Scope::User),
        ]
    );
}

#[test]
fn no_client_targets_every_row_in_scope() {
    let specs = removal_targets(
        &[],
        IdeScope::Project,
        Path::new("/tmp/proj"),
        &HomeDirs::default(),
    );
    assert_eq!(specs.len(), 7, "{:?}", kinds_and_scopes(&specs));
    assert!(specs.iter().all(|s| s.scope == Scope::Project));
}

#[test]
fn absent_file_is_not_wired_and_nothing_is_created() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("no-dir/settings.json");
    let step = process_removal(&spec_at(cfg, MergeStrategy::McpServers), tmp.path(), false);
    assert_eq!(step.status, IdeStatus::Unchanged);
    assert!(step.message.contains("not wired"), "{}", step.message);
    assert!(!tmp.path().join("no-dir").exists());
}

#[test]
fn file_without_an_entry_is_not_wired_and_not_rewritten() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    let original = "{\"mcpServers\":{\"other\":{}}}";
    fs::write(&cfg, original).unwrap();
    let step = process_removal(
        &spec_at(cfg.clone(), MergeStrategy::McpServers),
        tmp.path(),
        false,
    );
    assert_eq!(step.status, IdeStatus::Unchanged);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), original);
}

#[test]
fn wired_file_loses_cartog_and_keeps_other_servers() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    fs::write(&cfg, WIRED).unwrap();
    let step = process_removal(
        &spec_at(cfg.clone(), MergeStrategy::McpServers),
        tmp.path(),
        false,
    );
    assert_eq!(step.status, IdeStatus::Removed);
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"mcpServers": {"other": {"command": "x"}}})
    );
}

#[test]
fn unparseable_file_is_skipped_and_byte_identical() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("kilo.jsonc");
    let original = "// team\n{\"mcp\": {\"cartog\": {}}}\n";
    fs::write(&cfg, original).unwrap();
    let step = process_removal(&spec_at(cfg.clone(), MergeStrategy::Mcp), tmp.path(), false);
    assert_eq!(step.status, IdeStatus::Skipped);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), original);
}

#[test]
fn dry_run_plans_the_removal_with_a_diff_and_writes_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    fs::write(&cfg, WIRED).unwrap();
    let step = process_removal(
        &spec_at(cfg.clone(), MergeStrategy::McpServers),
        tmp.path(),
        true,
    );
    assert_eq!(step.status, IdeStatus::Removed);
    assert!(step.diff.is_some());
    assert_eq!(fs::read_to_string(&cfg).unwrap(), WIRED);
}

#[test]
fn codex_removal_takes_only_this_projects_section() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let ours = codex_section_name(&repo);
    let cfg = tmp.path().join("config.toml");
    fs::write(
        &cfg,
        format!(
            "[mcp_servers.{ours}]\ncommand = \"cartog\"\n\n[mcp_servers.cartog-other-ffff0000]\ncommand = \"cartog\"\n"
        ),
    )
    .unwrap();
    let mut spec = spec_at(cfg.clone(), MergeStrategy::CodexToml);
    spec.kind = ClientKind::Codex;
    let step = process_removal(&spec, &repo, false);
    assert_eq!(step.status, IdeStatus::Removed);
    let after = fs::read_to_string(&cfg).unwrap();
    assert!(!after.contains(&ours), "{after}");
    assert!(after.contains("cartog-other-ffff0000"), "{after}");
}

#[test]
fn removal_ignores_detection_for_a_wired_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let homes = HomeDirs {
        qwen: tmp.path().join(".qwen/settings.json"),
        ..HomeDirs::default()
    };
    fs::create_dir_all(tmp.path().join(".qwen")).unwrap();
    fs::write(&homes.qwen, WIRED).unwrap();
    let specs = removal_targets(&[], IdeScope::User, tmp.path(), &homes);
    let report = run_remove(&specs, tmp.path(), false);
    assert_eq!(report.summary.removed, 1);
    assert!(!fs::read_to_string(&homes.qwen).unwrap().contains("cartog"));
}

#[test]
fn run_remove_continues_past_a_skipped_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bad = tmp.path().join("bad.json");
    let good = tmp.path().join("good.json");
    fs::write(&bad, "{not json").unwrap();
    fs::write(&good, WIRED).unwrap();
    let specs = vec![
        spec_at(bad, MergeStrategy::McpServers),
        spec_at(good.clone(), MergeStrategy::McpServers),
    ];
    let report = run_remove(&specs, tmp.path(), false);
    assert_eq!((report.summary.skipped, report.summary.removed), (1, 1));
    assert_eq!(report.operation, Operation::Remove);
}

#[test]
fn a_second_run_finds_nothing_to_remove() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    fs::write(&cfg, WIRED).unwrap();
    let specs = vec![spec_at(cfg, MergeStrategy::McpServers)];
    run_remove(&specs, tmp.path(), false);
    let second = run_remove(&specs, tmp.path(), false);
    assert_eq!((second.summary.removed, second.summary.unchanged), (0, 1));
}

#[test]
fn uninstall_of_a_client_with_no_row_at_the_scope_names_the_slugs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let err = uninstall_targets(
        &[ClientKind::CopilotCli],
        IdeScope::Project,
        tmp.path(),
        &HomeDirs::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("copilot-cli") && err.contains("project"),
        "got: {err}"
    );
}

#[test]
fn no_home_fallback_path_is_not_wired_not_an_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let step = process_removal(
        &spec_at(PathBuf::from("."), MergeStrategy::McpServers),
        tmp.path(),
        false,
    );
    assert_eq!(step.status, IdeStatus::Unchanged);
}

#[test]
fn ide_remove_shows_the_picker_on_a_terminal_with_no_flag() {
    assert_eq!(
        removal_mode(false, false, false, false, true),
        RemovalMode::Picker
    );
}

#[test]
fn ide_remove_without_a_terminal_refuses_unless_told_what_to_do() {
    assert_eq!(
        removal_mode(false, false, false, false, false),
        RemovalMode::Refuse
    );
}

#[test]
fn ide_remove_with_json_and_no_yes_refuses() {
    assert_eq!(
        removal_mode(false, false, true, false, true),
        RemovalMode::Refuse
    );
}

#[test]
fn ide_remove_with_yes_removes_directly() {
    assert_eq!(
        removal_mode(true, false, false, false, false),
        RemovalMode::Direct
    );
}

#[test]
fn ide_remove_with_a_named_client_removes_directly() {
    assert_eq!(
        removal_mode(false, false, false, true, false),
        RemovalMode::Direct
    );
}

#[test]
fn ide_remove_dry_run_previews_directly() {
    assert_eq!(
        removal_mode(false, true, true, false, false),
        RemovalMode::Direct
    );
}

fn kilo_paths(specs: &[ClientSpec]) -> Vec<PathBuf> {
    specs.iter().map(|s| s.path.clone()).collect()
}

#[test]
fn kilo_project_removal_targets_every_existing_config_file() {
    let cwd = tempfile::TempDir::new().unwrap();
    let dir = cwd.path().join(".kilo");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("kilo.json"), "{}").unwrap();
    fs::write(dir.join("kilo.jsonc"), "{}").unwrap();
    let specs = removal_targets(
        &[ClientKind::Kilo],
        IdeScope::Project,
        cwd.path(),
        &HomeDirs::default(),
    );
    assert_eq!(
        kilo_paths(&specs),
        vec![dir.join("kilo.jsonc"), dir.join("kilo.json")]
    );
}

#[test]
fn kilo_user_removal_also_targets_config_json() {
    let cwd = tempfile::TempDir::new().unwrap();
    let dir = cwd.path().join("cfg/kilo");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("kilo.json"), "{}").unwrap();
    fs::write(dir.join("config.json"), "{}").unwrap();
    let homes = HomeDirs {
        kilo: dir.join("kilo.json"),
        ..HomeDirs::default()
    };
    let specs = removal_targets(&[ClientKind::Kilo], IdeScope::User, cwd.path(), &homes);
    assert_eq!(
        kilo_paths(&specs),
        vec![dir.join("kilo.json"), dir.join("config.json")]
    );
}

#[test]
fn kilo_with_no_config_file_keeps_its_single_target() {
    let cwd = tempfile::TempDir::new().unwrap();
    let specs = removal_targets(
        &[ClientKind::Kilo],
        IdeScope::Project,
        cwd.path(),
        &HomeDirs::default(),
    );
    assert_eq!(kilo_paths(&specs), vec![cwd.path().join(".kilo/kilo.json")]);
}

#[test]
fn kilo_entry_left_in_kilo_json_is_removed_after_a_jsonc_appears() {
    let cwd = tempfile::TempDir::new().unwrap();
    let dir = cwd.path().join(".kilo");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("kilo.json"),
        r#"{"mcp": {"cartog": {"type": "local"}}}"#,
    )
    .unwrap();
    fs::write(dir.join("kilo.jsonc"), r#"{"theme": "x"}"#).unwrap();
    let specs = removal_targets(
        &[ClientKind::Kilo],
        IdeScope::Project,
        cwd.path(),
        &HomeDirs::default(),
    );
    let report = run_remove(&specs, cwd.path(), false);
    assert_eq!(report.summary.removed, 1);
    assert!(!fs::read_to_string(dir.join("kilo.json"))
        .unwrap()
        .contains("cartog"));
}

#[test]
fn a_relative_override_path_is_read_not_mistaken_for_the_no_home_stub() {
    let tmp = tempfile::TempDir::new().unwrap();
    let step = process_removal(
        &spec_at(
            PathBuf::from("relative-copilot-home/mcp-config.json"),
            MergeStrategy::CopilotLocal,
        ),
        tmp.path(),
        false,
    );
    assert_eq!(step.message, "not wired (no config file)");
}

#[test]
fn a_dry_run_removal_summary_says_would_be_removed() {
    let mut r = report(
        Operation::Remove,
        vec![step("cursor", IdeStatus::Removed, "would remove")],
    );
    r.dry_run = true;
    assert!(
        r.render_human()
            .ends_with("\n1 files: 1 would be removed, 0 not wired, 0 skipped, 0 errors\n"),
        "{}",
        r.render_human()
    );
}

#[test]
fn json_report_marks_a_dry_run() {
    let mut r = report(Operation::Remove, vec![]);
    r.dry_run = true;
    assert_eq!(serde_json::to_value(&r).unwrap()["dry_run"], true);
}

#[test]
fn run_remove_marks_a_dry_run_report() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    fs::write(&cfg, WIRED).unwrap();
    let report = run_remove(&[spec_at(cfg, MergeStrategy::McpServers)], tmp.path(), true);
    assert!(report.dry_run);
}

#[test]
fn picker_report_counts_preflight_failures_in_summary_and_exit_status() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    fs::write(&cfg, WIRED).unwrap();
    let removal = run_remove(
        &[spec_at(cfg, MergeStrategy::McpServers)],
        tmp.path(),
        false,
    );
    let preflight = vec![
        step("kilo", IdeStatus::Skipped, "not modified (bad)"),
        step("codex", IdeStatus::Error, "could not read existing config"),
    ];

    let report = picker_report(preflight, removal);

    assert_eq!(
        (
            report.summary.total,
            report.summary.removed,
            report.summary.skipped,
            report.summary.error
        ),
        (3, 1, 1, 1)
    );
    assert!(report.has_errors());
}

#[test]
fn picker_report_with_only_preflight_failures_is_not_called_not_wired() {
    let tmp = tempfile::TempDir::new().unwrap();
    let report = picker_report(
        vec![step("kilo", IdeStatus::Skipped, "not modified (bad)")],
        run_remove(&[], tmp.path(), false),
    );
    assert!(!report.render_human().contains("not wired in any config"));
}

const MARKED: &str = "<!-- Managed by cartog: `cartog ide --rules` updates this file and `cartog uninstall` deletes it. Delete this line to keep your own edits. -->\n\nrule\n";

fn rule_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    (tmp, cwd)
}

fn put(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn uninstall(clients: &[ClientKind], scope: IdeScope, cwd: &Path) -> IdeReport {
    let targets = uninstall_targets(clients, scope, cwd, &HomeDirs::default()).unwrap();
    let mut report = run_remove(&targets.configs, cwd, false);
    append_rule_removals(&mut report, &targets.rules, false);
    report
}

#[test]
fn uninstall_targets_every_rule_file_when_no_client_is_named() {
    let (_tmp, cwd) = rule_repo();
    let targets = uninstall_targets(&[], IdeScope::All, &cwd, &HomeDirs::default()).unwrap();
    assert_eq!(targets.rules.len(), 8);
}

#[test]
fn uninstall_accepts_a_client_whose_only_project_file_is_its_rule() {
    let (_tmp, cwd) = rule_repo();
    put(&cwd.join(".windsurf/rules/cartog.md"), MARKED);
    let targets = uninstall_targets(
        &[ClientKind::Windsurf],
        IdeScope::Project,
        &cwd,
        &HomeDirs::default(),
    )
    .unwrap();
    assert_eq!((targets.configs.len(), targets.rules.len()), (0, 1));
}

#[test]
fn uninstall_deletes_every_cartog_rule_file() {
    let (_tmp, cwd) = rule_repo();
    let cursor = cwd.join(".cursor/rules/cartog.mdc");
    let claude = cwd.join(".claude/rules/cartog.md");
    put(&cursor, MARKED);
    put(&claude, MARKED);
    let report = uninstall(&[], IdeScope::All, &cwd);
    assert_eq!(
        (cursor.exists(), claude.exists(), report.summary.removed),
        (false, false, 2)
    );
}

#[test]
fn uninstall_of_one_client_deletes_only_its_rule_file() {
    let (_tmp, cwd) = rule_repo();
    let cursor = cwd.join(".cursor/rules/cartog.mdc");
    let claude = cwd.join(".claude/rules/cartog.md");
    put(&cursor, MARKED);
    put(&claude, MARKED);
    uninstall(&[ClientKind::Cursor], IdeScope::All, &cwd);
    assert_eq!((cursor.exists(), claude.exists()), (false, true));
}

#[test]
fn uninstall_at_user_scope_keeps_rule_files() {
    let (_tmp, cwd) = rule_repo();
    let cursor = cwd.join(".cursor/rules/cartog.mdc");
    put(&cursor, MARKED);
    uninstall(&[], IdeScope::User, &cwd);
    assert!(cursor.exists());
}

#[test]
fn uninstall_keeps_a_rule_file_cartog_did_not_write() {
    let (_tmp, cwd) = rule_repo();
    let cursor = cwd.join(".cursor/rules/cartog.mdc");
    put(&cursor, "my rules\n");
    let report = uninstall(&[ClientKind::Cursor], IdeScope::Project, &cwd);
    assert_eq!((cursor.exists(), report.summary.skipped), (true, 1));
}

#[test]
fn removal_render_hides_absent_rule_files_and_counts_them_not_wired() {
    let (_tmp, cwd) = rule_repo();
    let cursor = cwd.join(".cursor/rules/cartog.mdc");
    put(&cursor, MARKED);
    let mut report = run_remove(&[], &cwd, false);
    let targets = uninstall_targets(
        &[ClientKind::Cursor, ClientKind::ClaudeCode],
        IdeScope::Project,
        &cwd,
        &HomeDirs::default(),
    )
    .unwrap();
    append_rule_removals(&mut report, &targets.rules, false);
    assert_eq!(
        report.render_human(),
        format!(
            "- cursor (project, {}): removed\n\n2 files: 1 removed, 1 not wired, 0 skipped, 0 errors\n",
            cursor.display()
        )
    );
}

#[test]
fn a_picked_rule_row_keeps_only_that_rule_target() {
    let (_tmp, cwd) = rule_repo();
    let targets = uninstall_targets(&[], IdeScope::Project, &cwd, &HomeDirs::default()).unwrap();
    let picked = RemovalItem {
        kind: ClientKind::Cursor,
        scope: Scope::Project,
        path: cwd.join(".cursor/rules/cartog.mdc"),
    };
    let chosen = chosen_targets(targets, &[picked]);
    let rules: Vec<ClientKind> = chosen.rules.iter().map(|t| t.kind).collect();
    assert_eq!((chosen.configs.len(), rules), (0, vec![ClientKind::Cursor]));
}

#[test]
fn uninstall_of_a_client_with_no_file_in_scope_still_names_the_scope() {
    let (_tmp, cwd) = rule_repo();
    let err = uninstall_targets(
        &[ClientKind::Windsurf],
        IdeScope::Project,
        &cwd,
        &HomeDirs::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("windsurf") && err.contains("project"),
        "got: {err}"
    );
}
