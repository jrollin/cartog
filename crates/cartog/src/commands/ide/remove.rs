//! `cartog uninstall` / `cartog ide --remove`: take the cartog MCP entry out of
//! client configs and delete cartog's own rule files. A removal never creates a file,
//! never deletes a directory, and deletes no file but a rule file holding cartog's marker.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::catalogue::{build_specs, client_name, HomeDirs};
use super::merge::codex_section_name;
use super::picker::{removal_picker, removal_plan, RemovalChoice, RemovalItem};
use super::report::push_step;
use super::rules::{removal_rule_targets, remove_rule, RuleTarget};
use super::run::{atomic_write, scope_slug};
use super::unmerge::{remove_entry, Removal};
use super::{
    ClientSpec, DiffPair, FileKind, IdeReport, IdeStatus, IdeStep, IdeSummary, MergeStrategy,
    Operation, Scope,
};
use crate::cli::{ClientKind, IdeScope};

/// `cartog uninstall [client ...]`: non-interactive removal, the mirror of
/// `cartog install`. No client named = every config in scope that holds cartog.
pub fn cmd_uninstall(
    clients: Vec<ClientKind>,
    scope: IdeScope,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let homes = HomeDirs::detect();
    let targets = uninstall_targets(&clients, scope, &cwd, &homes)?;
    let mut report = run_remove(&targets.configs, &cwd, dry_run);
    append_rule_removals(&mut report, &targets.rules, dry_run);
    print_removal(&report, json, dry_run)?;
    if report.has_errors() {
        std::process::exit(1);
    }
    Ok(())
}

/// `cartog ide --remove`: the picker on a TTY, otherwise every wired config in scope.
pub(super) fn cmd_ide_remove(
    client: Option<ClientKind>,
    scope: IdeScope,
    yes: bool,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let clients: Vec<ClientKind> = client.into_iter().collect();
    let tty = std::io::stdin().is_terminal();
    match removal_mode(yes, dry_run, json, !clients.is_empty(), tty) {
        RemovalMode::Direct => return cmd_uninstall(clients, scope, dry_run, json),
        RemovalMode::Refuse => anyhow::bail!(
            "`cartog ide --remove` needs a terminal to confirm. Pass --yes to remove \
             cartog from every config and rule file in scope, or use \
             `cartog uninstall [client ...]`."
        ),
        RemovalMode::Picker => {}
    }
    let cwd = std::env::current_dir()?;
    let homes = HomeDirs::detect();
    let targets = RemovalTargets {
        configs: removal_targets(&[], scope, &cwd, &homes),
        rules: removal_rule_targets(&[], scope, &cwd),
    };
    let plan = removal_plan(&targets.configs, &targets.rules, &cwd);
    let report = if plan.items.is_empty() {
        picker_report(plan.skipped, run_remove(&[], &cwd, false))
    } else {
        // Shown before the picker so the choice is made knowing what cannot be cleaned.
        let mut skipped = String::new();
        for step in &plan.skipped {
            push_step(&mut skipped, step);
        }
        print!("{skipped}");
        let RemovalChoice::Selected(chosen) = removal_picker(&plan.items)? else {
            return Ok(());
        };
        let chosen = chosen_targets(targets, &chosen);
        let mut removal = run_remove(&chosen.configs, &cwd, false);
        append_rule_removals(&mut removal, &chosen.rules, false);
        picker_report(plan.skipped, removal)
    };
    print_removal(&report, false, false)?;
    if report.has_errors() {
        std::process::exit(1);
    }
    Ok(())
}

/// The entry key to remove: Codex keeps one section per project, the rest `cartog`.
fn entry_section(spec: &ClientSpec, cwd: &Path) -> String {
    if spec.strategy == MergeStrategy::CodexToml {
        codex_section_name(cwd)
    } else {
        "cartog".to_string()
    }
}

/// The picker's final report: files that failed the preflight (unreadable or
/// unparseable) count in the summary and exit status, as they do for `uninstall`.
pub(super) fn picker_report(preflight: Vec<IdeStep>, mut removal: IdeReport) -> IdeReport {
    for step in &preflight {
        removal.summary.record(step.status);
    }
    removal.steps.splice(0..0, preflight);
    removal
}

/// How `cartog ide --remove` proceeds.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RemovalMode {
    /// Ask a person which configs to clean.
    Picker,
    /// The command line already says what to do.
    Direct,
    /// Nobody can confirm: refuse rather than strip every config silently.
    Refuse,
}

/// An agent's shell or CI has no terminal; without `--yes` (or a named client or a
/// dry run) removing cartog from every config there needs an explicit go-ahead.
pub(super) fn removal_mode(
    yes: bool,
    dry_run: bool,
    json: bool,
    client_named: bool,
    stdin_tty: bool,
) -> RemovalMode {
    if yes || dry_run || client_named {
        RemovalMode::Direct
    } else if stdin_tty && !json {
        RemovalMode::Picker
    } else {
        RemovalMode::Refuse
    }
}

/// What a removal looks at: MCP config files and cartog rule files.
#[derive(Debug)]
pub(super) struct RemovalTargets {
    pub(super) configs: Vec<ClientSpec>,
    pub(super) rules: Vec<RuleTarget>,
}

/// Targets for `cartog uninstall`: named clients with neither a config row nor a rule
/// file on disk at the scope are an error, so a typo'd scope does not silently report
/// "not wired". Every rule agent has a rule target, so only an existing file counts.
pub(super) fn uninstall_targets(
    clients: &[ClientKind],
    scope: IdeScope,
    cwd: &Path,
    homes: &HomeDirs,
) -> Result<RemovalTargets> {
    let targets = RemovalTargets {
        configs: removal_targets(clients, scope, cwd, homes),
        rules: removal_rule_targets(clients, scope, cwd),
    };
    let rule_on_disk = targets
        .rules
        .iter()
        .any(|t| fs::symlink_metadata(&t.path).is_ok());
    if !clients.is_empty() && targets.configs.is_empty() && !rule_on_disk {
        let requested: Vec<&str> = clients.iter().map(|c| client_name(*c)).collect();
        anyhow::bail!(
            "no catalogue entries match clients=[{}] at scope={}. \
             Try --scope all, or pick clients available at this scope.",
            requested.join(", "),
            scope_slug(scope),
        );
    }
    Ok(targets)
}

/// Every catalogue row in scope, narrowed to `clients` when any are named. Install
/// detection is ignored: a removal only shrinks files that already exist.
pub(super) fn removal_targets(
    clients: &[ClientKind],
    scope: IdeScope,
    cwd: &Path,
    homes: &HomeDirs,
) -> Vec<ClientSpec> {
    build_specs(None, scope, false, cwd, homes)
        .into_iter()
        .filter(|s| clients.is_empty() || clients.contains(&s.kind))
        .flat_map(every_config_file)
        .collect()
}

/// Kilo reads several file names from one dir and install writes to whichever
/// existed at the time, so by now the entry can sit in any of them.
fn every_config_file(spec: ClientSpec) -> Vec<ClientSpec> {
    let dir = match spec.path.parent() {
        Some(dir) if spec.kind == ClientKind::Kilo && spec.path.is_absolute() => dir,
        _ => return vec![spec],
    };
    let mut names = vec!["kilo.jsonc", "kilo.json"];
    if spec.scope == Scope::User {
        names.push("config.json");
    }
    let present: Vec<PathBuf> = names
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .collect();
    if present.is_empty() {
        return vec![spec];
    }
    present
        .into_iter()
        .map(|path| ClientSpec {
            path,
            ..spec.clone()
        })
        .collect()
}

/// The targets whose file was picked in `cartog ide --remove`.
pub(super) fn chosen_targets(targets: RemovalTargets, chosen: &[RemovalItem]) -> RemovalTargets {
    let picked = |path: &Path| chosen.iter().any(|c| c.path == path);
    RemovalTargets {
        configs: targets
            .configs
            .into_iter()
            .filter(|t| picked(&t.path))
            .collect(),
        rules: targets
            .rules
            .into_iter()
            .filter(|t| picked(&t.path))
            .collect(),
    }
}

/// Delete cartog's rule files after the config steps, counting each into the summary.
pub(super) fn append_rule_removals(report: &mut IdeReport, rules: &[RuleTarget], dry_run: bool) {
    for target in rules {
        let step = remove_rule(target, dry_run);
        report.summary.record(step.status);
        report.steps.push(step);
    }
}

/// Remove cartog from each target and collect one step per file.
pub(super) fn run_remove(specs: &[ClientSpec], cwd: &Path, dry_run: bool) -> IdeReport {
    let mut summary = IdeSummary::default();
    let steps = specs
        .iter()
        .map(|spec| {
            let step = process_removal(spec, cwd, dry_run);
            summary.record(step.status);
            step
        })
        .collect();
    IdeReport {
        operation: Operation::Remove,
        dry_run,
        steps,
        summary,
    }
}

/// Print a removal report as JSON or human text.
pub(super) fn print_removal(report: &IdeReport, json: bool, dry_run: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    print!("{}", report.render_human());
    if dry_run && report.summary.removed > 0 {
        println!("Dry run only. Re-run without --dry-run to apply.");
    }
    Ok(())
}

/// One config file: take the cartog entry out, or report why not.
pub(super) fn process_removal(spec: &ClientSpec, cwd: &Path, dry_run: bool) -> IdeStep {
    let scope = match spec.scope {
        Scope::Project => "project",
        Scope::User => "user",
    };
    let make = |status: IdeStatus, message: String, diff: Option<DiffPair>| IdeStep {
        client: client_name(spec.kind).to_string(),
        scope: scope.to_string(),
        path: spec.path.display().to_string(),
        file_kind: FileKind::McpConfig,
        status,
        message,
        diff,
    };

    // Without a home dir, user paths fall back to "." stubs that hold no config.
    // A relative override (`COPILOT_HOME=cop`) is a real path and is read below.
    if spec.path == Path::new(".") {
        return make(
            IdeStatus::Unchanged,
            "not wired (no home directory)".into(),
            None,
        );
    }
    let existing = match fs::read_to_string(&spec.path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return make(
                IdeStatus::Unchanged,
                "not wired (no config file)".into(),
                None,
            );
        }
        Err(e) => {
            return make(
                IdeStatus::Error,
                format!("could not read existing config: {e}"),
                None,
            );
        }
    };

    let without = match remove_entry(&existing, spec.strategy, &entry_section(spec, cwd)) {
        Ok(Removal::Removed(new)) => new,
        Ok(Removal::NotWired) => {
            return make(IdeStatus::Unchanged, "not wired".into(), None);
        }
        Err(e) => {
            return make(
                IdeStatus::Skipped,
                format!("not modified ({e}); remove the cartog entry by hand"),
                None,
            );
        }
    };

    if dry_run {
        return make(
            IdeStatus::Removed,
            "would remove".into(),
            Some(DiffPair {
                before: Some(existing),
                after: without,
            }),
        );
    }
    match atomic_write(&spec.path, &without) {
        Ok(()) => make(IdeStatus::Removed, "removed".into(), None),
        Err(e) => make(IdeStatus::Error, format!("write failed: {e}"), None),
    }
}
