use super::Sandbox;
use crate::cli::ClientKind;
use crate::commands::ide::rules::{apply_rule_plan, plan_rules, rule_selection, RuleSelection};
use crate::commands::ide::{IdeReport, IdeStatus, IdeSummary, Operation};

fn empty_report() -> IdeReport {
    IdeReport {
        operation: Operation::Install,
        dry_run: false,
        steps: Vec::new(),
        summary: IdeSummary::default(),
    }
}

#[test]
fn no_named_client_selects_by_detection() {
    assert_eq!(rule_selection(&[]), RuleSelection::Detected);
}

#[test]
fn a_named_client_selects_by_name() {
    let named = [ClientKind::Cursor];
    assert_eq!(rule_selection(&named), RuleSelection::Named(&named));
}

#[test]
fn without_rules_no_rule_file_is_planned() {
    let sb = Sandbox::new();
    for dir in [".cursor/rules", ".claude/rules", ".roo/rules"] {
        sb.dir(dir);
    }
    let plan = plan_rules(None, &sb.cwd, &sb.homes(), None);
    assert_eq!((plan.targets.len(), plan.unsupported.len()), (0, 0));
}

#[test]
fn detection_plans_the_agents_in_use() {
    let sb = Sandbox::new();
    sb.dir(".claude/rules");
    let plan = plan_rules(Some(RuleSelection::Detected), &sb.cwd, &sb.homes(), None);
    let kinds: Vec<ClientKind> = plan.targets.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, vec![ClientKind::ClaudeCode]);
}

#[test]
fn named_clients_plan_rule_files_and_a_skip_for_the_rest() {
    let sb = Sandbox::new();
    let named = [ClientKind::Zed, ClientKind::Cursor];
    let plan = plan_rules(
        Some(RuleSelection::Named(&named)),
        &sb.cwd,
        &sb.homes(),
        None,
    );
    let kinds: Vec<ClientKind> = plan.targets.iter().map(|t| t.kind).collect();
    let skipped: Vec<&str> = plan.unsupported.iter().map(|s| s.client.as_str()).collect();
    assert_eq!((kinds, skipped), (vec![ClientKind::Cursor], vec!["zed"]));
}

#[test]
fn a_skipped_rule_step_does_not_fail_the_run() {
    let sb = Sandbox::new();
    sb.file(".roorules", "team rules");
    let named = [ClientKind::Roo];
    let plan = plan_rules(
        Some(RuleSelection::Named(&named)),
        &sb.cwd,
        &sb.homes(),
        None,
    );
    let mut report = empty_report();
    apply_rule_plan(&mut report, plan, &sb.cwd, false);
    assert_eq!((report.summary.skipped, report.has_errors()), (1, false));
}

#[test]
fn a_blocked_agent_does_not_stop_the_others() {
    let sb = Sandbox::new();
    sb.file(".roorules", "team rules");
    let named = [ClientKind::Roo, ClientKind::Cursor];
    let plan = plan_rules(
        Some(RuleSelection::Named(&named)),
        &sb.cwd,
        &sb.homes(),
        None,
    );
    let mut report = empty_report();
    apply_rule_plan(&mut report, plan, &sb.cwd, false);
    let statuses: Vec<(String, IdeStatus)> = report
        .steps
        .iter()
        .map(|s| (s.client.clone(), s.status))
        .collect();
    assert_eq!(
        (statuses, sb.cwd.join(".cursor/rules/cartog.mdc").exists()),
        (
            vec![
                ("cursor".into(), IdeStatus::Created),
                ("roo".into(), IdeStatus::Skipped)
            ],
            true
        )
    );
}
