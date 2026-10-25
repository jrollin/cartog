//! `cartog ide` / `cartog install`: wire `cartog serve` into MCP-aware editor configs.
//!
//! Shared types live here; behavior is split across [`merge`] (config-file
//! merging), [`catalogue`] (client specs + path resolution), [`picker`]
//! (interactive TUI), and [`run`] (command entry + reporting + write).

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::ClientKind;

/// ASCII banner printed at the top of the interactive `cartog ide` flow.
/// Figlet "Standard" font + tagline. Kept short so it doesn't dominate the
/// terminal before the picker draws.
const CARTOG_LOGO: &str = r"
   ___           _
  / __\__ _ _ __| |_ ___   __ _
 / /  / _` | '__| __/ _ \ / _` |
/ /__| (_| | |  | || (_) | (_| |
\____/\__,_|_|   \__\___/ \__, |
                          |___/
  code graph indexer · MCP wiring
";

/// Whether a client's config file lives inside the project or in the user's home dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Project,
    User,
}

/// How the `cartog` entry slots into a client's JSON shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// `{"mcpServers": {"cartog": {"command","args"}}}` — Claude Code, Claude Desktop,
    /// Cursor, Windsurf, Gemini CLI.
    McpServers,
    /// `{"mcp": {"cartog": {"type":"local","command":["cartog","serve"],"enabled":true}}}` — OpenCode,
    /// Kilo Code. A single `command` array (program + args) and `type: "local"` for stdio
    /// servers. Only `type`/`command` are rewritten on an existing entry, so `enabled`,
    /// `environment` and `timeout` set in the client survive.
    Mcp,
    /// `{"context_servers": {"cartog": {"command","args"}}}` — Zed.
    ContextServers,
    /// `{"servers": {"cartog": {"type":"stdio","command","args"}}}` — VS Code Copilot.
    /// Top-level key is `servers` (NOT `mcpServers`) per VS Code docs.
    VsCodeServers,
    /// `[mcp_servers.<name>] command = "cartog" args = [...]` — Codex CLI.
    /// Codex reads MCP from `~/.codex/config.toml` only (no per-project file),
    /// so cartog writes one section per project named `cartog-<dir>-<hash8>` to
    /// keep multi-project setups coexisting in the same file.
    CodexToml,
    /// `mcp_servers: {cartog: {command, args}}` YAML — Hermes Agent (`~/.hermes/config.yaml`).
    /// Shared config file; only the `cartog` entry is upserted, other keys preserved.
    HermesYaml,
    /// `{"mcpServers": {"cartog": {"type":"local","command","args","tools":["*"]}}}` — GitHub Copilot CLI.
    /// Only `type`/`command`/`args` are rewritten on an existing entry: a narrowed `tools`
    /// list and `env` survive.
    CopilotLocal,
    /// `{"mcpServers": {"cartog": {"command","args","disabled":false}}}` — Cline, Cline CLI, Roo Code.
    /// Only `command`/`args` are rewritten on an existing entry: the client's UI stores
    /// `disabled` and tool approvals (`alwaysAllow`/`autoApprove`) in the same object.
    McpServersToggle,
}

/// How auto mode (and the picker's pre-check) decides that a client is in use.
/// Except for `Always`, a client binary on `PATH` also counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// Always wired: the original project rows (the repo is the evidence).
    Always,
    /// The config file's parent dir exists: right when only the client creates it.
    ParentDir,
    /// The config file itself, or one of these paths that only the client creates.
    Evidence(Vec<PathBuf>),
}

/// Resolved target for one MCP client: where to write and how to merge.
#[derive(Debug)]
pub struct ClientSpec {
    pub kind: ClientKind,
    pub scope: Scope,
    pub path: PathBuf,
    pub strategy: MergeStrategy,
    pub args: Vec<String>,
    pub detection: Detection,
    /// A dir that must already exist, even when the client is named: writing would
    /// create it and fake an install signal for another client.
    pub requires: Option<PathBuf>,
}

/// Pure outcome of merging a `cartog` entry into a (possibly empty) JSON document.
#[derive(Debug, PartialEq, Eq)]
pub struct MergeOutcome {
    pub new_json: String,
    pub action: Action,
}

/// What the planned merge would do to the on-disk file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Created,
    Updated,
    Unchanged,
}

/// Step-level status reported in `IdeReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IdeStatus {
    Created,
    Updated,
    Unchanged,
    Skipped,
    Error,
}

impl IdeStatus {
    fn icon(self) -> &'static str {
        match self {
            IdeStatus::Created | IdeStatus::Updated | IdeStatus::Unchanged => "+",
            IdeStatus::Skipped => "!",
            IdeStatus::Error => "x",
        }
    }

    fn from_action(action: Action) -> Self {
        match action {
            Action::Created => IdeStatus::Created,
            Action::Updated => IdeStatus::Updated,
            Action::Unchanged => IdeStatus::Unchanged,
        }
    }
}

/// Before/after config content for one client, shown as a diff in `--json`.
#[derive(Debug, Serialize)]
pub struct DiffPair {
    pub before: Option<String>,
    pub after: String,
}

/// One client's outcome in an `IdeReport`: where it wrote, the status, and the diff.
#[derive(Debug, Serialize)]
pub struct IdeStep {
    pub client: String,
    pub scope: String,
    pub path: String,
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
}

/// Full result of an `ide`/`install` run: one step per client plus the summary.
#[derive(Debug, Serialize)]
pub struct IdeReport {
    pub steps: Vec<IdeStep>,
    pub summary: IdeSummary,
}

impl IdeReport {
    /// Returns `true` when at least one client step ended in `IdeStatus::Error`.
    /// Used by `cmd_ide` to decide the process exit code.
    pub fn has_errors(&self) -> bool {
        self.summary.error > 0
    }

    /// Render the report as a human-readable string: one line per client step
    /// (status icon, client, scope, path, message), an optional before/after
    /// diff block per step, and a trailing summary line of the counts.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            out.push_str(&format!(
                "{} {} ({}, {}): {}\n",
                step.status.icon(),
                step.client,
                step.scope,
                step.path,
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
        let s = &self.summary;
        out.push_str(&format!(
            "\n{} clients: {} created, {} updated, {} unchanged, {} skipped, {} errors\n",
            s.total, s.created, s.updated, s.unchanged, s.skipped, s.error
        ));
        out
    }
}

mod catalogue;
mod merge;
mod picker;
mod run;

pub use run::{cmd_ide, cmd_install};

#[cfg(test)]
mod tests;
