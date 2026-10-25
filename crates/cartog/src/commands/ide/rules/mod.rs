//! Agent rule files written by `--rules`: one cartog-owned file per agent that reads
//! a rules directory, teaching it to prefer cartog's tools.

mod apply;
mod catalogue;
mod content;
mod plan;

pub(super) use apply::remove_rule;
pub(super) use catalogue::{removal_rule_targets, RuleTarget};
pub(super) use plan::{apply_rule_plan, plan_rules, rule_selection, RuleSelection};

// Reached directly only by the tests.
#[cfg(test)]
pub(super) use apply::{install_rule, unsupported_rule_step};
#[cfg(test)]
pub(super) use catalogue::{
    blocker, detected_rule_targets, named_rule_targets, rule_format, Blocker, RuleFormat,
    RULE_AGENTS,
};
#[cfg(test)]
pub(super) use content::{is_cartog_owned, render, RULE_BODY};
