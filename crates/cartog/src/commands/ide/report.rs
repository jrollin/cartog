//! Step-level report of an `ide` / `install` / `uninstall` run and its rendering.

use serde::Serialize;

use super::Action;

/// Step-level status reported in `IdeReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IdeStatus {
    Created,
    Updated,
    Unchanged,
    Skipped,
    Error,
    /// The `cartog` entry was taken out of the file (`cartog uninstall`).
    Removed,
}

impl IdeStatus {
    fn icon(self) -> &'static str {
        match self {
            IdeStatus::Created | IdeStatus::Updated | IdeStatus::Unchanged => "+",
            IdeStatus::Skipped => "!",
            IdeStatus::Error => "x",
            IdeStatus::Removed => "-",
        }
    }

    pub(super) fn from_action(action: Action) -> Self {
        match action {
            Action::Created => IdeStatus::Created,
            Action::Updated => IdeStatus::Updated,
            Action::Unchanged => IdeStatus::Unchanged,
        }
    }
}

/// What kind of file a step touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// A client config holding the `cartog` MCP entry.
    McpConfig,
    /// A cartog-owned agent rule file written by `--rules`.
    Rule,
}

/// Before/after config content for one client, shown as a diff in `--json`.
#[derive(Debug, Serialize)]
pub struct DiffPair {
    pub before: Option<String>,
    pub after: String,
}

/// One file's outcome in an `IdeReport`: which client, where, the status, and the diff.
#[derive(Debug, Serialize)]
pub struct IdeStep {
    pub client: String,
    pub scope: String,
    /// Empty when the client has no file to touch (a named client without a rules dir).
    pub path: String,
    pub file_kind: FileKind,
    pub status: IdeStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffPair>,
}

/// Per-status tallies across all steps in an `IdeReport`.
#[derive(Debug, Serialize, Default)]
pub struct IdeSummary {
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub error: usize,
    pub removed: usize,
}

impl IdeSummary {
    /// Count one step's status into the tallies.
    pub fn record(&mut self, status: IdeStatus) {
        self.total += 1;
        let slot = match status {
            IdeStatus::Created => &mut self.created,
            IdeStatus::Updated => &mut self.updated,
            IdeStatus::Unchanged => &mut self.unchanged,
            IdeStatus::Skipped => &mut self.skipped,
            IdeStatus::Error => &mut self.error,
            IdeStatus::Removed => &mut self.removed,
        };
        *slot += 1;
    }
}

/// Whether a report wires cartog in (`ide`, `install`) or takes it out (`uninstall`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Install,
    Remove,
}

/// Full result of an `ide`/`install`/`uninstall` run: one step per config plus the summary.
#[derive(Debug, Serialize)]
pub struct IdeReport {
    pub operation: Operation,
    /// Planned changes only: nothing was written.
    pub dry_run: bool,
    pub steps: Vec<IdeStep>,
    pub summary: IdeSummary,
}

impl IdeReport {
    /// Returns `true` when at least one client step ended in `IdeStatus::Error`.
    /// Used by `cmd_ide` to decide the process exit code.
    pub fn has_errors(&self) -> bool {
        self.summary.error > 0
    }

    /// Render the report as a human-readable string: one line per step (status
    /// icon, client, scope, path, message), an optional before/after diff block
    /// per step, and a trailing summary line of the counts.
    pub fn render_human(&self) -> String {
        match self.operation {
            Operation::Install => self.render_install(),
            Operation::Remove => self.render_removal(),
        }
    }

    fn render_install(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            push_step(&mut out, step);
        }
        let s = &self.summary;
        out.push_str(&format!(
            "\n{} files: {} created, {} updated, {} unchanged, {} skipped, {} errors\n",
            s.total, s.created, s.updated, s.unchanged, s.skipped, s.error
        ));
        out
    }

    /// Lists only what changed: a removal visits every catalogue row in scope and
    /// most of those files are absent or hold no cartog entry.
    fn render_removal(&self) -> String {
        let s = &self.summary;
        if s.removed + s.skipped + s.error == 0 {
            return "cartog is not wired in any config in scope.\n".into();
        }
        let mut out = String::new();
        for step in self
            .steps
            .iter()
            .filter(|step| step.status != IdeStatus::Unchanged)
        {
            push_step(&mut out, step);
        }
        let removed = if self.dry_run {
            "would be removed"
        } else {
            "removed"
        };
        out.push_str(&format!(
            "\n{} files: {} {removed}, {} not wired, {} skipped, {} errors\n",
            s.total, s.removed, s.unchanged, s.skipped, s.error
        ));
        out
    }
}

pub(super) fn push_step(out: &mut String, step: &IdeStep) {
    let location = if step.path.is_empty() {
        step.scope.clone()
    } else {
        format!("{}, {}", step.scope, step.path)
    };
    out.push_str(&format!(
        "{} {} ({location}): {}\n",
        step.status.icon(),
        step.client,
        step.message,
    ));
    if let Some(diff) = &step.diff {
        if let Some(before) = &diff.before {
            out.push_str("  --- before ---\n");
            for line in before.lines() {
                out.push_str("    ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str("  --- after ---\n");
        for line in diff.after.lines() {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
}
