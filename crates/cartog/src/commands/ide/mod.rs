//! `cartog ide` / `cartog install` / `cartog uninstall`: wire `cartog serve` into
//! MCP-aware editor configs, or take it out again.
//!
//! Shared spec types live here; behavior is split across `catalogue` (client
//! specs + path resolution), `merge` / `unmerge` (adding / removing the entry
//! per config format), `picker` (interactive TUI), `run` (install entry points
//! + write), `remove` (uninstall entry points) and `report` (step report + rendering).

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
#[derive(Debug, Clone)]
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

mod catalogue;
mod merge;
mod picker;
mod remove;
mod report;
mod run;
mod unmerge;

pub use remove::cmd_uninstall;
pub use report::{DiffPair, IdeReport, IdeStatus, IdeStep, IdeSummary, Operation};
pub use run::{cmd_ide, cmd_install};

#[cfg(test)]
mod tests;
