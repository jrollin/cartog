//! `cartog ide` — wire `cartog serve` into MCP-compatible editor config files.
//!
//! Supports nine clients across four config shapes:
//! - `mcpServers` JSON: Claude Code, Claude Desktop, Cursor, Windsurf, Gemini CLI
//! - `mcp` JSON (OpenCode): `type: "local"`, `command` array, `enabled: true`
//! - `context_servers` JSON: Zed
//! - `servers` JSON (VS Code Copilot): `type: "stdio"`, flat command + args
//! - `[mcp_servers.<section>]` TOML: Codex CLI (per-project sections)
//!
//! The JSON branches round-trip through `serde_json::Value`; the Codex TOML
//! branch uses `toml_edit` so comments and ordering survive. On parse failure
//! the file is left untouched and reported as skipped (no JSONC clobbering).

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::cli::{ClientKind, IdeScope};

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
    /// `{"mcp": {"cartog": {"type":"local","command":["cartog","serve"],"enabled":true}}}` — OpenCode.
    /// OpenCode uses a single `command` array (program + args) and `type: "local"` for stdio servers.
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
}

/// Resolved target for one MCP client: where to write and how to merge.
#[derive(Debug)]
pub struct ClientSpec {
    pub kind: ClientKind,
    pub scope: Scope,
    pub path: PathBuf,
    pub strategy: MergeStrategy,
    pub args: Vec<String>,
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

#[derive(Debug, Serialize)]
pub struct DiffPair {
    pub before: Option<String>,
    pub after: String,
}

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

#[derive(Debug, Serialize, Default)]
pub struct IdeSummary {
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub error: usize,
}

#[derive(Debug, Serialize)]
pub struct IdeReport {
    pub steps: Vec<IdeStep>,
    pub summary: IdeSummary,
}

impl IdeReport {
    pub fn has_errors(&self) -> bool {
        self.summary.error > 0
    }

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

/// Merge a `cartog` server entry into a client's config file (JSON or TOML).
///
/// Dispatches to the per-format implementation based on `strategy`. The Codex
/// TOML branch uses `toml_edit` for round-trip-safe edits that preserve
/// formatting and comments in the rest of the file. JSON branches use
/// `serde_json::Value` and pretty-print.
pub fn merge_entry(
    existing: Option<&str>,
    strategy: MergeStrategy,
    args: &[String],
) -> Result<MergeOutcome> {
    if strategy == MergeStrategy::CodexToml {
        return merge_codex_toml(existing, args, "cartog");
    }
    let trimmed = existing.map(str::trim).unwrap_or("");
    let parsed_prev: Option<Value> = if trimmed.is_empty() {
        None
    } else {
        let v = serde_json::from_str(trimmed).context("config file is not valid JSON")?;
        match &v {
            Value::Object(_) => Some(v),
            _ => anyhow::bail!("config root must be a JSON object"),
        }
    };

    let mut root = match &parsed_prev {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    apply_strategy(&mut root, strategy, args)?;
    let new_json = format!("{}\n", serde_json::to_string_pretty(&Value::Object(root))?);

    let action = match (existing, parsed_prev) {
        (None, _) => Action::Created,
        (Some(_), Some(prev)) => {
            let canonical_prev = format!("{}\n", serde_json::to_string_pretty(&prev)?);
            if canonical_prev == new_json {
                Action::Unchanged
            } else {
                Action::Updated
            }
        }
        // Empty / whitespace-only file: behave like Created.
        (Some(_), None) => Action::Updated,
    };

    Ok(MergeOutcome { new_json, action })
}

fn apply_strategy(
    root: &mut Map<String, Value>,
    strategy: MergeStrategy,
    args: &[String],
) -> Result<()> {
    match strategy {
        MergeStrategy::McpServers => {
            let servers = ensure_object(root, "mcpServers")?;
            servers.insert(
                "cartog".into(),
                json!({ "command": "cartog", "args": args }),
            );
        }
        MergeStrategy::Mcp => {
            let mcp = ensure_object(root, "mcp")?;
            let mut command_array: Vec<Value> = Vec::with_capacity(1 + args.len());
            command_array.push(Value::String("cartog".into()));
            command_array.extend(args.iter().cloned().map(Value::String));
            mcp.insert(
                "cartog".into(),
                json!({
                    "type": "local",
                    "command": Value::Array(command_array),
                    "enabled": true,
                }),
            );
        }
        MergeStrategy::ContextServers => {
            let servers = ensure_object(root, "context_servers")?;
            servers.insert(
                "cartog".into(),
                json!({ "command": "cartog", "args": args }),
            );
        }
        MergeStrategy::VsCodeServers => {
            let servers = ensure_object(root, "servers")?;
            servers.insert(
                "cartog".into(),
                json!({ "type": "stdio", "command": "cartog", "args": args }),
            );
        }
        // Dispatched out of `merge_entry` before this function is called;
        // return an error rather than panic if the dispatch is ever bypassed.
        MergeStrategy::CodexToml => {
            anyhow::bail!("internal: CodexToml must go through merge_codex_toml")
        }
    }
    Ok(())
}

/// Merge a `[mcp_servers.<section>]` table into a Codex `config.toml` document.
///
/// Uses `toml_edit` so the rest of the user's Codex config (other servers,
/// hooks, comments, formatting) survives untouched. The section name is the
/// caller-supplied `section` (typically `cartog-<project-slug>-<hash8>` so
/// multiple cartog projects coexist in `~/.codex/config.toml`).
fn merge_codex_toml(
    existing: Option<&str>,
    args: &[String],
    section: &str,
) -> Result<MergeOutcome> {
    use toml_edit::{value, Array, DocumentMut, Item, Table};

    let mut doc: DocumentMut = match existing {
        None => DocumentMut::new(),
        Some(s) if s.trim().is_empty() => DocumentMut::new(),
        Some(s) => s
            .parse::<DocumentMut>()
            .context("config file is not valid TOML")?,
    };

    let canonical_prev = doc.to_string();

    // Ensure top-level `mcp_servers` is a table (dotted-key syntax). Refuse
    // to overwrite if the user put something else (string, array, etc.) there.
    if !doc.contains_key("mcp_servers") {
        let mut t = Table::new();
        t.set_implicit(true);
        doc["mcp_servers"] = Item::Table(t);
    }
    let mcp_servers = doc["mcp_servers"].as_table_mut().ok_or_else(|| {
        anyhow::anyhow!("top-level `mcp_servers` is not a TOML table; refusing to overwrite")
    })?;

    // If our section already exists, refuse to clobber a non-table value
    // (some past hand-edit). A pre-existing table is fine — we replace it.
    if let Some(existing_section) = mcp_servers.get(section) {
        if !existing_section.is_table() {
            anyhow::bail!(
                "section `[mcp_servers.{section}]` exists but is not a table; refusing to overwrite"
            );
        }
    }

    let mut entry = Table::new();
    entry["command"] = value("cartog");
    let mut arr = Array::new();
    for a in args {
        arr.push(a.clone());
    }
    entry["args"] = value(arr);
    mcp_servers.insert(section, Item::Table(entry));

    let new_json = doc.to_string();

    let action = match existing {
        None => Action::Created,
        Some(_) => {
            if new_json == canonical_prev {
                Action::Unchanged
            } else {
                Action::Updated
            }
        }
    };

    Ok(MergeOutcome { new_json, action })
}

/// Build a Codex section name unique to this project. Codex shares one TOML
/// file across all projects, so we suffix with a short hash of the absolute
/// path to keep multiple cartog setups from clobbering each other.
fn codex_section_name(project_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let abs = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let project_slug = abs
        .file_name()
        .and_then(|s| s.to_str())
        .map(slugify)
        .unwrap_or_else(|| "project".to_string());
    let mut h = Sha256::new();
    h.update(abs.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    let hash_short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("cartog-{project_slug}-{hash_short}")
}

fn slugify(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Borrow (and lazily create) the top-level Object at `key`. If the slot is
/// occupied by a non-Object value (string, array, number, bool) we refuse to
/// overwrite it — that would silently destroy whatever the user had. The
/// error surfaces as `Skipped` at the caller, so the file is left untouched.
/// `null` is treated like an absent key and replaced with an empty object.
fn ensure_object<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    // Reject non-Object, non-Null values up-front so we never touch them.
    if let Some(existing) = root.get(key) {
        if !matches!(existing, Value::Object(_) | Value::Null) {
            anyhow::bail!(
                "top-level `{key}` is a {} (expected object); refusing to overwrite",
                value_kind(existing),
            );
        }
    }
    // Replace Null with an empty object; leave an existing Object alone.
    match root.get(key) {
        None | Some(Value::Null) => {
            root.insert(key.into(), Value::Object(Map::new()));
        }
        _ => {}
    }
    // SAFETY: prior arms ensure the slot is now `Value::Object`. Map back via
    // `get_mut` + `as_object_mut` so a future maintainer doesn't have to think
    // about an `unreachable!` branch.
    let slot = root
        .get_mut(key)
        .expect("just inserted or kept an existing entry");
    slot.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("internal: ensure_object slot is not Object after fixup"))
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Static catalogue of every client cartog knows about: kind, scope, JSON/TOML
/// shape, and which set of serve-args to register. Adding a new client = one
/// row in this table + matching path resolution in [`HomeDirs::detect`] (for
/// User-scope rows) or a `cwd.join(...)` in [`project_path`] (for Project rows).
///
/// `args_kind` is decoupled from the on-disk format so `--no-watch` can affect
/// only Claude Code without per-row plumbing.
const CLIENT_CATALOGUE: &[CatalogueEntry] = &[
    CatalogueEntry {
        kind: ClientKind::ClaudeCode,
        scope: Scope::Project,
        strategy: MergeStrategy::McpServers,
        args_kind: ArgsKind::ServeWithWatch,
    },
    CatalogueEntry {
        kind: ClientKind::ClaudeCode,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
        args_kind: ArgsKind::ServeWithWatch,
    },
    CatalogueEntry {
        kind: ClientKind::Cursor,
        scope: Scope::Project,
        strategy: MergeStrategy::McpServers,
        args_kind: ArgsKind::Serve,
    },
    CatalogueEntry {
        kind: ClientKind::Vscode,
        scope: Scope::Project,
        strategy: MergeStrategy::VsCodeServers,
        args_kind: ArgsKind::Serve,
    },
    CatalogueEntry {
        kind: ClientKind::ClaudeDesktop,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
        args_kind: ArgsKind::Serve,
    },
    CatalogueEntry {
        kind: ClientKind::Windsurf,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
        args_kind: ArgsKind::Serve,
    },
    CatalogueEntry {
        kind: ClientKind::Opencode,
        scope: Scope::User,
        strategy: MergeStrategy::Mcp,
        args_kind: ArgsKind::Serve,
    },
    CatalogueEntry {
        kind: ClientKind::Zed,
        scope: Scope::User,
        strategy: MergeStrategy::ContextServers,
        args_kind: ArgsKind::Serve,
    },
    CatalogueEntry {
        kind: ClientKind::Codex,
        scope: Scope::User,
        strategy: MergeStrategy::CodexToml,
        args_kind: ArgsKind::Serve,
    },
    CatalogueEntry {
        kind: ClientKind::Gemini,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
        args_kind: ArgsKind::Serve,
    },
];

struct CatalogueEntry {
    kind: ClientKind,
    scope: Scope,
    strategy: MergeStrategy,
    args_kind: ArgsKind,
}

#[derive(Clone, Copy)]
enum ArgsKind {
    /// `["serve"]`
    Serve,
    /// `["serve", "--watch"]` unless `--no-watch` was given.
    ServeWithWatch,
}

impl ArgsKind {
    fn resolve(self, no_watch: bool) -> Vec<String> {
        match self {
            ArgsKind::Serve => vec!["serve".into()],
            ArgsKind::ServeWithWatch if no_watch => vec!["serve".into()],
            ArgsKind::ServeWithWatch => vec!["serve".into(), "--watch".into()],
        }
    }
}

/// Resolve the on-disk config path for a project-scoped client. Mirrors the
/// per-platform user paths assembled in [`HomeDirs::detect`].
fn project_path(kind: ClientKind, cwd: &Path) -> Option<PathBuf> {
    match kind {
        ClientKind::ClaudeCode => Some(cwd.join(".mcp.json")),
        ClientKind::Cursor => Some(cwd.join(".cursor").join("mcp.json")),
        ClientKind::Vscode => Some(cwd.join(".vscode").join("mcp.json")),
        // User-only clients have no project-scope analogue.
        _ => None,
    }
}

/// Look up the user-scope path for a client; mirror of `project_path` for the
/// User branch of the catalogue.
fn user_path(kind: ClientKind, home: &HomeDirs) -> Option<PathBuf> {
    match kind {
        ClientKind::ClaudeCode => Some(home.claude_code.clone()),
        ClientKind::ClaudeDesktop => Some(home.claude_desktop.clone()),
        ClientKind::Codex => Some(home.codex.clone()),
        ClientKind::Gemini => Some(home.gemini.clone()),
        ClientKind::Opencode => Some(home.opencode.clone()),
        ClientKind::Windsurf => Some(home.windsurf.clone()),
        ClientKind::Zed => Some(home.zed.clone()),
        // Project-only clients have no user-scope analogue.
        ClientKind::Cursor | ClientKind::Vscode => None,
    }
}

/// Resolve the set of `ClientSpec`s to operate on, filtered by `client` and `scope`.
///
/// Override the resolution roots via `cwd` (project files) and `home_dirs`
/// (user-scoped files) for test sandboxing.
pub fn build_specs(
    client: Option<ClientKind>,
    scope: IdeScope,
    no_watch: bool,
    cwd: &Path,
    home_dirs: &HomeDirs,
) -> Vec<ClientSpec> {
    CLIENT_CATALOGUE
        .iter()
        .filter(|e| match scope {
            IdeScope::Project => e.scope == Scope::Project,
            IdeScope::User => e.scope == Scope::User,
            IdeScope::All => true,
        })
        .filter(|e| client.map_or(true, |c| c == e.kind))
        .filter_map(|e| {
            let path = match e.scope {
                Scope::Project => project_path(e.kind, cwd)?,
                Scope::User => user_path(e.kind, home_dirs)?,
            };
            Some(ClientSpec {
                kind: e.kind,
                scope: e.scope,
                path,
                strategy: e.strategy,
                args: e.args_kind.resolve(no_watch),
            })
        })
        .collect()
}

/// User-scope config file paths. Resolution is unconditional: each client gets a
/// canonical path per platform. Whether to actually write is decided at runtime
/// by checking whether the parent directory exists (proxy for "client installed").
#[derive(Debug, Clone)]
pub struct HomeDirs {
    pub claude_code: PathBuf,
    pub claude_desktop: PathBuf,
    pub codex: PathBuf,
    pub gemini: PathBuf,
    pub windsurf: PathBuf,
    pub opencode: PathBuf,
    pub zed: PathBuf,
}

impl Default for HomeDirs {
    /// Fallback used when `directories::BaseDirs::new()` returns `None` (no
    /// resolvable home dir). All paths are anchored at `"."`, which guarantees
    /// the parent-dir check at write time returns "not installed" for every user
    /// client and we degrade gracefully to project-scope work.
    fn default() -> Self {
        let stub = PathBuf::from(".");
        HomeDirs {
            claude_code: stub.clone(),
            claude_desktop: stub.clone(),
            codex: stub.clone(),
            gemini: stub.clone(),
            windsurf: stub.clone(),
            opencode: stub.clone(),
            zed: stub,
        }
    }
}

impl HomeDirs {
    /// Resolve config paths from the current environment.
    pub fn detect() -> Self {
        let Some(base) = directories::BaseDirs::new() else {
            return Self::default();
        };
        let home = base.home_dir().to_path_buf();

        // Zed and OpenCode store config under `~/.config/` on every platform, so
        // honour `XDG_CONFIG_HOME` first and fall back to a plain `~/.config`,
        // rather than `BaseDirs::config_dir()` (which is `~/Library/Application Support`
        // on macOS and would point at the wrong location).
        let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));

        let claude_desktop = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Claude/claude_desktop_config.json")
        } else if cfg!(target_os = "windows") {
            base.config_dir().join("Claude/claude_desktop_config.json")
        } else {
            // Unofficial Linux builds (claude-desktop-debian and similar) store
            // config under XDG_CONFIG_HOME/Claude. Still gated by the parent-dir
            // existence check at write time, so users without it get a clean Skipped.
            xdg_config.join("Claude/claude_desktop_config.json")
        };

        HomeDirs {
            claude_code: home.join(".claude/settings.json"),
            claude_desktop,
            codex: home.join(".codex/config.toml"),
            gemini: home.join(".gemini/settings.json"),
            windsurf: home.join(".codeium/windsurf/mcp_config.json"),
            opencode: xdg_config.join("opencode/opencode.json"),
            zed: xdg_config.join("zed/settings.json"),
        }
    }
}

/// Public CLI entry point.
pub fn cmd_ide(
    client: Option<ClientKind>,
    scope: IdeScope,
    yes: bool,
    dry_run: bool,
    no_watch: bool,
    json: bool,
) -> Result<()> {
    let interactive =
        !yes && !dry_run && !json && client.is_none() && std::io::stdin().is_terminal();
    let cwd = std::env::current_dir()?;
    let homes = HomeDirs::detect();
    let report = run_ide(client, scope, interactive, dry_run, no_watch, &cwd, &homes)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render_human());
        print_next_steps(&report, dry_run);
    }

    if report.has_errors() {
        std::process::exit(1);
    }
    Ok(())
}

fn print_next_steps(report: &IdeReport, dry_run: bool) {
    if dry_run {
        println!("Dry run only. Re-run without --dry-run to apply.");
        return;
    }
    let s = &report.summary;
    if s.created + s.updated > 0 {
        println!(
            "Next: open your editor and try a cartog tool (e.g. `search`, `refs`). \
             Re-run `cartog ide` after installing more editors."
        );
    } else if s.skipped > 0 && s.created + s.updated + s.unchanged == 0 {
        println!(
            "No MCP clients were configured. Install an MCP-aware editor \
             (Claude Code, Cursor, Windsurf, Zed) and re-run `cartog ide`."
        );
    }
}

/// Library entry point used by both `cmd_ide` and `cmd_init`.
pub fn run_ide(
    client: Option<ClientKind>,
    scope: IdeScope,
    interactive: bool,
    dry_run: bool,
    no_watch: bool,
    cwd: &Path,
    homes: &HomeDirs,
) -> Result<IdeReport> {
    let specs = build_specs(client, scope, no_watch, cwd, homes);
    if specs.is_empty() {
        anyhow::bail!(
            "no clients match the requested filter ({:?} + scope={:?}). \
             Check `--client` and `--scope` are compatible \
             (e.g. claude-code and cursor are project-scoped only).",
            client,
            scope,
        );
    }
    let mut steps = Vec::with_capacity(specs.len());
    let mut summary = IdeSummary::default();

    for spec in specs {
        let step = process_spec(&spec, cwd, interactive, dry_run);
        match step.status {
            IdeStatus::Created => summary.created += 1,
            IdeStatus::Updated => summary.updated += 1,
            IdeStatus::Unchanged => summary.unchanged += 1,
            IdeStatus::Skipped => summary.skipped += 1,
            IdeStatus::Error => summary.error += 1,
        }
        summary.total += 1;
        steps.push(step);
    }

    Ok(IdeReport { steps, summary })
}

fn process_spec(spec: &ClientSpec, cwd: &Path, interactive: bool, dry_run: bool) -> IdeStep {
    let client = client_name(spec.kind).to_string();
    let scope = match spec.scope {
        Scope::Project => "project",
        Scope::User => "user",
    }
    .to_string();
    let path = spec.path.display().to_string();

    let make = |status: IdeStatus, message: String, diff: Option<DiffPair>| IdeStep {
        client: client.clone(),
        scope: scope.clone(),
        path: path.clone(),
        status,
        message,
        diff,
    };

    if spec.scope == Scope::User {
        let parent_missing = spec.path.parent().map(|p| !p.exists()).unwrap_or(true);
        if parent_missing {
            return make(
                IdeStatus::Skipped,
                "config directory not found (client likely not installed)".into(),
                None,
            );
        }
    }

    let existing = match fs::read_to_string(&spec.path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return make(
                IdeStatus::Error,
                format!("could not read existing config: {e}"),
                None,
            );
        }
    };

    let merge_result = if spec.strategy == MergeStrategy::CodexToml {
        let section = codex_section_name(cwd);
        merge_codex_toml(existing.as_deref(), &spec.args, &section)
    } else {
        merge_entry(existing.as_deref(), spec.strategy, &spec.args)
    };
    let outcome = match merge_result {
        Ok(o) => o,
        Err(e) => {
            return make(
                IdeStatus::Skipped,
                format!("not modified ({e}); fix the file or remove it and re-run"),
                None,
            );
        }
    };

    // Split `Action::Unchanged` off into its terminal branch so the rest of the
    // function only deals with the two write-causing variants. The bound
    // `write_action` lets us produce planned/done strings without an
    // `unreachable!()` arm later.
    let write_action = match outcome.action {
        Action::Unchanged => {
            return make(IdeStatus::Unchanged, "already up to date".into(), None);
        }
        Action::Created => WriteAction::Create,
        Action::Updated => WriteAction::Update,
    };
    let status = IdeStatus::from_action(outcome.action);

    if interactive {
        let prompt = format!("→ {client} ({path}): {}", write_action.planned());
        match confirm(&prompt) {
            Ok(false) => return make(IdeStatus::Skipped, "declined by user".into(), None),
            Err(e) => {
                return make(IdeStatus::Error, format!("prompt failed: {e}"), None);
            }
            Ok(true) => {}
        }
    }

    if dry_run {
        return make(
            status,
            write_action.planned().into(),
            Some(DiffPair {
                before: existing,
                after: outcome.new_json,
            }),
        );
    }

    if let Err(e) = atomic_write(&spec.path, &outcome.new_json) {
        return make(IdeStatus::Error, format!("write failed: {e}"), None);
    }

    make(status, write_action.done().into(), None)
}

/// Outcome that actually causes a write — projection of `Action` that excludes
/// `Unchanged`. Keeps the message-building match arms total without
/// `unreachable!`.
#[derive(Clone, Copy)]
enum WriteAction {
    Create,
    Update,
}

impl WriteAction {
    fn planned(self) -> &'static str {
        match self {
            WriteAction::Create => "would create",
            WriteAction::Update => "would update",
        }
    }
    fn done(self) -> &'static str {
        match self {
            WriteAction::Create => "created",
            WriteAction::Update => "updated",
        }
    }
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
    }
    // Per-process suffix so two `cartog ide` invocations racing on the same
    // file do not corrupt each other's tmp.
    let pid = std::process::id();
    let file_name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let mut tmp_name = file_name;
    tmp_name.push(format!(".cartog-{pid}.tmp"));
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, contents).with_context(|| format!("could not write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("could not move tmp file into {}", path.display()))?;
    Ok(())
}

fn client_name(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::ClaudeCode => "claude-code",
        ClientKind::ClaudeDesktop => "claude-desktop",
        ClientKind::Codex => "codex",
        ClientKind::Cursor => "cursor",
        ClientKind::Gemini => "gemini",
        ClientKind::Opencode => "opencode",
        ClientKind::Vscode => "vscode",
        ClientKind::Windsurf => "windsurf",
        ClientKind::Zed => "zed",
    }
}

/// Read one Y/n answer from stdin. Returns false on EOF — a closed stdin
/// must NOT be treated as consent (a script that pipes nothing would
/// otherwise silently apply every change). The caller is responsible for
/// gating interactive mode behind `stdin.is_terminal()` before invoking.
fn confirm(label: &str) -> Result<bool> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{label}\n  Apply? [Y/n] ")?;
    stderr.flush()?;
    drop(stderr);

    let stdin = std::io::stdin();
    let mut line = String::new();
    let n = stdin.lock().read_line(&mut line)?;
    if n == 0 {
        return Ok(false);
    }
    let answer = line.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "" | "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Vec<String> {
        vec!["serve".into()]
    }

    fn args_watch() -> Vec<String> {
        vec!["serve".into(), "--watch".into()]
    }

    #[test]
    fn merge_mcp_servers_empty_file_creates_entry() {
        let o = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
        assert_eq!(o.action, Action::Created);
        let v: Value = serde_json::from_str(&o.new_json).unwrap();
        assert_eq!(v["mcpServers"]["cartog"]["command"], "cartog");
        assert_eq!(v["mcpServers"]["cartog"]["args"], json!(["serve"]));
    }

    #[test]
    fn merge_mcp_servers_preserves_other_servers() {
        let existing = r#"{"mcpServers": {"other": {"command": "x", "args": ["a"]}}}"#;
        let o = merge_entry(Some(existing), MergeStrategy::McpServers, &args()).unwrap();
        assert_eq!(o.action, Action::Updated);
        let v: Value = serde_json::from_str(&o.new_json).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["cartog"]["command"], "cartog");
    }

    #[test]
    fn merge_mcp_servers_idempotent() {
        let first = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
        let second =
            merge_entry(Some(&first.new_json), MergeStrategy::McpServers, &args()).unwrap();
        assert_eq!(second.action, Action::Unchanged);
        assert_eq!(first.new_json, second.new_json);
    }

    #[test]
    fn merge_mcp_servers_updates_when_args_change() {
        let first = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
        let second = merge_entry(
            Some(&first.new_json),
            MergeStrategy::McpServers,
            &args_watch(),
        )
        .unwrap();
        assert_eq!(second.action, Action::Updated);
        let v: Value = serde_json::from_str(&second.new_json).unwrap();
        assert_eq!(
            v["mcpServers"]["cartog"]["args"],
            json!(["serve", "--watch"])
        );
    }

    #[test]
    fn merge_opencode_strategy_uses_mcp_local_command_array() {
        let o = merge_entry(None, MergeStrategy::Mcp, &args()).unwrap();
        let v: Value = serde_json::from_str(&o.new_json).unwrap();
        assert_eq!(v["mcp"]["cartog"]["type"], "local");
        assert_eq!(v["mcp"]["cartog"]["enabled"], true);
        assert_eq!(v["mcp"]["cartog"]["command"], json!(["cartog", "serve"]));
    }

    #[test]
    fn merge_zed_strategy_uses_context_servers_with_flat_command() {
        let o = merge_entry(None, MergeStrategy::ContextServers, &args()).unwrap();
        let v: Value = serde_json::from_str(&o.new_json).unwrap();
        assert_eq!(v["context_servers"]["cartog"]["command"], "cartog");
        assert_eq!(v["context_servers"]["cartog"]["args"], json!(["serve"]));
    }

    #[test]
    fn merge_invalid_json_returns_parse_error() {
        let err = merge_entry(Some("{not json"), MergeStrategy::McpServers, &args()).unwrap_err();
        assert!(err.to_string().contains("valid JSON"));
    }

    #[test]
    fn merge_refuses_when_top_level_key_is_string() {
        // User somehow set mcpServers to a string. Don't clobber.
        let existing = r#"{"mcpServers": "/etc/mcp/something.json"}"#;
        let err = merge_entry(Some(existing), MergeStrategy::McpServers, &args()).unwrap_err();
        assert!(
            err.to_string().contains("refusing to overwrite"),
            "expected refusal: {err}"
        );
    }

    #[test]
    fn merge_refuses_when_top_level_key_is_array() {
        let existing = r#"{"context_servers": ["one", "two"]}"#;
        let err = merge_entry(Some(existing), MergeStrategy::ContextServers, &args()).unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
    }

    #[test]
    fn merge_treats_null_top_level_key_as_absent() {
        // `null` is functionally an absent key; replace with an empty object.
        let existing = r#"{"servers": null}"#;
        let outcome = merge_entry(Some(existing), MergeStrategy::VsCodeServers, &args()).unwrap();
        let v: Value = serde_json::from_str(&outcome.new_json).unwrap();
        assert_eq!(v["servers"]["cartog"]["command"], "cartog");
    }

    #[test]
    fn merge_preserves_user_key_order() {
        // With preserve_order, an existing custom key order survives unrelated
        // mutations. Insert cartog into an mcpServers that already has `zzz`
        // and `aaa` — they should keep their existing order after our merge.
        let existing = r#"{
  "mcpServers": {
    "zzz": {"command": "z"},
    "aaa": {"command": "a"}
  }
}"#;
        let outcome = merge_entry(Some(existing), MergeStrategy::McpServers, &args()).unwrap();
        let zzz_pos = outcome.new_json.find("\"zzz\"").unwrap();
        let aaa_pos = outcome.new_json.find("\"aaa\"").unwrap();
        assert!(
            zzz_pos < aaa_pos,
            "expected zzz before aaa (user-defined order), got:\n{}",
            outcome.new_json
        );
    }

    #[test]
    fn merge_codex_toml_refuses_when_section_is_not_a_table() {
        let existing = "[mcp_servers]\ncartog-x = \"oops\"\n";
        let err = merge_codex_toml(Some(existing), &args(), "cartog-x").unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
    }

    #[test]
    fn merge_codex_toml_refuses_when_mcp_servers_is_not_a_table() {
        let existing = "mcp_servers = \"not a table\"\n";
        let err = merge_codex_toml(Some(existing), &args(), "cartog-x").unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
    }

    #[test]
    fn merge_vscode_strategy_uses_servers_key_and_stdio_type() {
        let o = merge_entry(None, MergeStrategy::VsCodeServers, &args()).unwrap();
        let v: Value = serde_json::from_str(&o.new_json).unwrap();
        assert_eq!(v["servers"]["cartog"]["type"], "stdio");
        assert_eq!(v["servers"]["cartog"]["command"], "cartog");
        assert!(v.get("mcpServers").is_none(), "must not write mcpServers");
    }

    #[test]
    fn merge_codex_toml_creates_section_under_mcp_servers() {
        let outcome = merge_codex_toml(None, &args(), "cartog-myproj-deadbeef").unwrap();
        assert_eq!(outcome.action, Action::Created);
        assert!(outcome
            .new_json
            .contains("[mcp_servers.cartog-myproj-deadbeef]"));
        assert!(outcome.new_json.contains("command = \"cartog\""));
        assert!(outcome.new_json.contains("args = [\"serve\"]"));
    }

    #[test]
    fn merge_codex_toml_preserves_other_servers_and_comments() {
        let existing = "# user-managed file\n\
            [mcp_servers.other]\n\
            command = \"other\"\n\
            args = [\"--flag\"]\n";
        let outcome = merge_codex_toml(Some(existing), &args(), "cartog-x").unwrap();
        assert_eq!(outcome.action, Action::Updated);
        assert!(outcome.new_json.contains("# user-managed file"));
        assert!(outcome.new_json.contains("[mcp_servers.other]"));
        assert!(outcome.new_json.contains("[mcp_servers.cartog-x]"));
    }

    #[test]
    fn merge_codex_toml_idempotent_with_same_section() {
        let first = merge_codex_toml(None, &args(), "cartog-x").unwrap();
        let second = merge_codex_toml(Some(&first.new_json), &args(), "cartog-x").unwrap();
        assert_eq!(second.action, Action::Unchanged);
    }

    #[test]
    fn merge_codex_toml_rejects_invalid_toml() {
        let err = merge_codex_toml(Some("[not [valid toml"), &args(), "cartog-x").unwrap_err();
        assert!(err.to_string().contains("valid TOML"));
    }

    #[test]
    fn codex_section_name_is_deterministic_and_slug_safe() {
        let p = std::env::temp_dir();
        let a = codex_section_name(&p);
        let b = codex_section_name(&p);
        assert_eq!(a, b, "section name must be stable for the same dir");
        assert!(a.starts_with("cartog-"));
        // Slug body must only contain alphanumerics and hyphens (TOML bare-key safe).
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn merge_empty_string_treated_as_empty_object() {
        let o = merge_entry(Some(""), MergeStrategy::McpServers, &args()).unwrap();
        assert_eq!(o.action, Action::Updated);
    }

    #[test]
    fn build_specs_default_covers_all_clients() {
        let tmp = std::env::temp_dir();
        let homes = HomeDirs::detect();
        let specs = build_specs(None, IdeScope::All, false, &tmp, &homes);
        // Project: claude-code, cursor, vscode (3)
        // User: claude-code, claude-desktop, codex, gemini, opencode, windsurf, zed (7)
        assert_eq!(specs.len(), 10);
    }

    #[test]
    fn build_specs_project_scope_drops_user_clients() {
        let tmp = std::env::temp_dir();
        let homes = HomeDirs::default();
        let specs = build_specs(None, IdeScope::Project, false, &tmp, &homes);
        assert_eq!(specs.len(), 3);
        assert!(specs.iter().all(|s| s.scope == Scope::Project));
    }

    #[test]
    fn build_specs_claude_code_filter_returns_both_scopes() {
        let tmp = std::env::temp_dir();
        let homes = HomeDirs::default();
        let specs = build_specs(
            Some(ClientKind::ClaudeCode),
            IdeScope::All,
            false,
            &tmp,
            &homes,
        );
        assert_eq!(specs.len(), 2);
        let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
        assert!(scopes.contains(&Scope::Project));
        assert!(scopes.contains(&Scope::User));
    }

    #[test]
    fn build_specs_client_filter_picks_one() {
        let tmp = std::env::temp_dir();
        let homes = HomeDirs::default();
        let specs = build_specs(Some(ClientKind::Cursor), IdeScope::All, false, &tmp, &homes);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].kind, ClientKind::Cursor);
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
}
