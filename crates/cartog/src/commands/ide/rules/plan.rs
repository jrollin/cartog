//! Which rule files an install run touches, and appending their steps to the report.

use std::ffi::OsStr;
use std::path::Path;

use super::apply::{install_rule, unsupported_rule_step};
use super::catalogue::{detected_rule_targets, named_rule_targets, RuleTarget};
use crate::cli::ClientKind;
use crate::commands::ide::catalogue::HomeDirs;
use crate::commands::ide::{IdeReport, IdeStep};

/// Which agents a `--rules` run writes a rule file for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSelection<'a> {
    /// No client named: agents with evidence of use.
    Detected,
    /// Clients named on the command line or picked in the TUI.
    Named(&'a [ClientKind]),
}

/// Detection when no client is named, the named clients otherwise.
pub fn rule_selection(named: &[ClientKind]) -> RuleSelection<'_> {
    if named.is_empty() {
        RuleSelection::Detected
    } else {
        RuleSelection::Named(named)
    }
}

/// Rule files a run writes, and the skipped steps for named clients without a rules dir.
#[derive(Debug, Default)]
pub struct RulePlan {
    pub targets: Vec<RuleTarget>,
    pub unsupported: Vec<IdeStep>,
}

/// Plan the rule files for a run; `None` (no `--rules`) plans nothing.
pub fn plan_rules(
    selection: Option<RuleSelection>,
    cwd: &Path,
    homes: &HomeDirs,
    path_env: Option<&OsStr>,
) -> RulePlan {
    match selection {
        None => RulePlan::default(),
        Some(RuleSelection::Detected) => RulePlan {
            targets: detected_rule_targets(cwd, homes, path_env),
            unsupported: Vec::new(),
        },
        Some(RuleSelection::Named(kinds)) => {
            let (targets, unsupported) = named_rule_targets(kinds, cwd);
            RulePlan {
                targets,
                unsupported: unsupported.into_iter().map(unsupported_rule_step).collect(),
            }
        }
    }
}

/// Write the planned rule files after the MCP steps, counting each into the summary.
pub fn apply_rule_plan(report: &mut IdeReport, plan: RulePlan, cwd: &Path, dry_run: bool) {
    let written = plan
        .targets
        .iter()
        .map(|target| install_rule(target, cwd, dry_run));
    for step in written.chain(plan.unsupported) {
        report.summary.record(step.status);
        report.steps.push(step);
    }
}
