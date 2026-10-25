//! Per-agent rule file facts: where cartog's rule file lives, the header that makes
//! the agent load it on every session, which agents a run targets, and what blocks a write.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{ClientKind, IdeScope};
use crate::commands::ide::catalogue::{client_installed, detection, user_path, HomeDirs};
use crate::commands::ide::Scope;

/// Where an agent reads cartog's rule file, and the frontmatter it needs to load it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleFormat {
    /// Rules directory components, relative to the repo root.
    pub dir: &'static [&'static str],
    pub file: &'static str,
    /// Frontmatter lines without the `---` fences; `None` for agents that load plain Markdown.
    pub header: Option<&'static str>,
}

impl RuleFormat {
    /// The rules directory under `root`.
    pub fn dir_in(&self, root: &Path) -> PathBuf {
        self.dir
            .iter()
            .fold(root.to_path_buf(), |path, c| path.join(c))
    }

    /// The rule file under `root`.
    pub fn path_in(&self, root: &Path) -> PathBuf {
        self.dir_in(root).join(self.file)
    }
}

/// Clients that read a rules directory, in the order their steps are reported.
pub const RULE_AGENTS: [ClientKind; 8] = [
    ClientKind::Cursor,
    ClientKind::Windsurf,
    ClientKind::Kiro,
    ClientKind::Cline,
    ClientKind::Roo,
    ClientKind::Antigravity,
    ClientKind::ClaudeCode,
    ClientKind::Vscode,
];

/// The rule file format for `kind`, or `None` when the client has no rules directory.
pub fn rule_format(kind: ClientKind) -> Option<RuleFormat> {
    let (dir, file, header): (&'static [&'static str], _, _) = match kind {
        // A plain `.md` in `.cursor/rules/` is ignored: Cursor only loads `.mdc`.
        ClientKind::Cursor => (
            &[".cursor", "rules"],
            "cartog.mdc",
            Some(
                "description: Prefer cartog MCP tools over grep for code navigation\n\
                 globs:\nalwaysApply: true",
            ),
        ),
        ClientKind::Windsurf => (
            &[".windsurf", "rules"],
            "cartog.md",
            Some("trigger: always_on"),
        ),
        ClientKind::Kiro => (
            &[".kiro", "steering"],
            "cartog.md",
            Some("inclusion: always"),
        ),
        ClientKind::Cline => (&[".clinerules"], "cartog.md", None),
        ClientKind::Roo => (&[".roo", "rules"], "cartog.md", None),
        // Antigravity silently discards a rule without a valid `trigger`.
        ClientKind::Antigravity => (
            &[".agents", "rules"],
            "cartog.md",
            Some("trigger: always_on"),
        ),
        ClientKind::ClaudeCode => (&[".claude", "rules"], "cartog.md", None),
        ClientKind::Vscode => (
            &[".github", "instructions"],
            "cartog.instructions.md",
            Some("applyTo: '**'"),
        ),
        ClientKind::ClaudeDesktop
        | ClientKind::ClineCli
        | ClientKind::Codex
        | ClientKind::CopilotCli
        | ClientKind::Gemini
        | ClientKind::Hermes
        | ClientKind::Kilo
        | ClientKind::Opencode
        | ClientKind::Qwen
        | ClientKind::Zed => return None,
    };
    Some(RuleFormat { dir, file, header })
}

/// One agent's rule file in this repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleTarget {
    pub kind: ClientKind,
    pub format: RuleFormat,
    pub path: PathBuf,
}

impl RuleTarget {
    fn new(kind: ClientKind, format: RuleFormat, cwd: &Path) -> Self {
        Self {
            kind,
            format,
            path: format.path_in(cwd),
        }
    }
}

/// Why writing an agent's rule file would break rules already in the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// Roo Code reads this fallback file only while `.roo/rules/` has no file.
    FallbackFile(PathBuf),
    /// A component of the rules directory exists as a file (Cline's legacy `.clinerules`).
    NotADirectory(PathBuf),
}

/// Rule agents with evidence of use: detected for the user (CLI on PATH, or the user
/// config or app location install checks), or with their rules dir already in the repo.
/// Project-scope MCP folders don't count: cartog creates them itself.
pub fn detected_rule_targets(
    cwd: &Path,
    homes: &HomeDirs,
    path_env: Option<&OsStr>,
) -> Vec<RuleTarget> {
    RULE_AGENTS
        .into_iter()
        .filter_map(|kind| {
            let format = rule_format(kind)?;
            let in_use =
                format.dir_in(cwd).is_dir() || user_row_detected(kind, cwd, homes, path_env);
            in_use.then(|| RuleTarget::new(kind, format, cwd))
        })
        .collect()
}

fn user_row_detected(
    kind: ClientKind,
    cwd: &Path,
    homes: &HomeDirs,
    path_env: Option<&OsStr>,
) -> bool {
    user_path(kind, homes).is_some_and(|path| {
        client_installed(
            kind,
            &path,
            &detection(kind, Scope::User, cwd, homes),
            path_env,
        )
    })
}

/// Targets for clients named on the command line or picked, in report order, plus
/// the named clients that have no rules directory (each once, in the order given).
pub fn named_rule_targets(kinds: &[ClientKind], cwd: &Path) -> (Vec<RuleTarget>, Vec<ClientKind>) {
    let targets = RULE_AGENTS
        .into_iter()
        .filter(|kind| kinds.contains(kind))
        .filter_map(|kind| rule_format(kind).map(|format| RuleTarget::new(kind, format, cwd)))
        .collect();
    let mut unsupported = Vec::new();
    for &kind in kinds {
        if rule_format(kind).is_none() && !unsupported.contains(&kind) {
            unsupported.push(kind);
        }
    }
    (targets, unsupported)
}

/// Rule files an uninstall looks at: every rule agent, or the named ones. Rule files
/// are project files, so a user-scope removal targets none.
pub fn removal_rule_targets(kinds: &[ClientKind], scope: IdeScope, cwd: &Path) -> Vec<RuleTarget> {
    if scope == IdeScope::User {
        return Vec::new();
    }
    RULE_AGENTS
        .into_iter()
        .filter(|kind| kinds.is_empty() || kinds.contains(kind))
        .filter_map(|kind| rule_format(kind).map(|format| RuleTarget::new(kind, format, cwd)))
        .collect()
}

/// What stops cartog from writing `kind`'s rule file in `cwd`, if anything.
pub fn blocker(kind: ClientKind, cwd: &Path) -> Option<Blocker> {
    let format = rule_format(kind)?;
    let mut dir = cwd.to_path_buf();
    for component in format.dir {
        dir.push(component);
        if dir.exists() && !dir.is_dir() {
            return Some(Blocker::NotADirectory(dir));
        }
    }
    if kind == ClientKind::Roo && !has_other_rule_file(&dir, format.file) {
        return [".roorules", ".clinerules"]
            .into_iter()
            .map(|name| cwd.join(name))
            .find(|fallback| fallback.is_file())
            .map(Blocker::FallbackFile);
    }
    None
}

/// Whether `dir` holds a visible file other than cartog's. Subdirectories and dotfiles
/// don't count, so an unsure answer blocks the write rather than allowing it.
fn has_other_rule_file(dir: &Path, own: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        !name.starts_with('.') && name != own && entry.path().is_file()
    })
}
