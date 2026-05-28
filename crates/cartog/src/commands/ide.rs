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

use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect};

use crate::cli::{ClientKind, IdeScope};

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

    // Interactive picker: shown when the user invokes `cartog ide` with no
    // explicit filter and a real TTY. Skipped under --yes/--dry-run/--json,
    // when --client is set, or when stdin is piped (CI/scripts).
    let report = if interactive {
        let items = picker_items(&cwd, &homes);
        match interactive_picker(&items)? {
            PickerOutcome::Cancelled => return Ok(()),
            PickerOutcome::Selected(chosen) => {
                run_ide_for_clients(&chosen, dry_run, no_watch, &cwd, &homes)?
            }
        }
    } else {
        run_ide(client, scope, interactive, dry_run, no_watch, &cwd, &homes)?
    };

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

/// Strip duplicate `ClientKind`s from a positional argument list while
/// preserving first-occurrence order. Returns `(unique, dropped)` so the
/// caller can warn about the duplicates instead of silently swallowing them.
/// Pulled out so cmd_install can call eprintln! while keeping the dedup
/// logic itself unit-testable.
fn dedupe_preserving_order(clients: Vec<ClientKind>) -> (Vec<ClientKind>, Vec<ClientKind>) {
    // ClientKind isn't Hash and there are only 9 variants, so a linear
    // membership check is cheaper than deriving Hash just for this.
    let mut unique = Vec::with_capacity(clients.len());
    let mut dropped = Vec::new();
    for c in clients {
        if unique.contains(&c) {
            dropped.push(c);
        } else {
            unique.push(c);
        }
    }
    (unique, dropped)
}

/// Filter the static `CLIENT_CATALOGUE` by a positional `clients` list and a
/// scope. Returns `(ClientKind, Scope)` pairs in catalogue order. Pulled out
/// of `cmd_install` so the selection logic is unit-testable without spinning
/// up the filesystem-touching dispatch.
fn filter_catalogue_by_clients(
    clients: &[ClientKind],
    scope: IdeScope,
) -> Vec<(ClientKind, Scope)> {
    CLIENT_CATALOGUE
        .iter()
        .filter(|e| match scope {
            IdeScope::Project => e.scope == Scope::Project,
            IdeScope::User => e.scope == Scope::User,
            IdeScope::All => true,
        })
        .filter(|e| clients.contains(&e.kind))
        .map(|e| (e.kind, e.scope))
        .collect()
}

/// `cartog install [client ...]` — friendlier shape of `cmd_ide` that takes
/// editors as positional args (matches brew/npm/pip convention). Always
/// non-interactive. Empty `clients` = install into every detected editor in
/// scope. Multiple positional clients = install each.
pub fn cmd_install(
    clients: Vec<ClientKind>,
    scope: IdeScope,
    dry_run: bool,
    no_watch: bool,
    json: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let homes = HomeDirs::detect();

    let report = if clients.is_empty() {
        // No positional clients → install into every detected editor in scope,
        // exactly like `cartog ide --yes`. The picker is skipped (yes implied).
        run_ide(None, scope, false, dry_run, no_watch, &cwd, &homes)?
    } else {
        // Dedupe so `cartog install cursor cursor` doesn't waste cycles; warn
        // so a script bug that produces dupes is visible.
        let (clients, duplicates) = dedupe_preserving_order(clients);
        if !duplicates.is_empty() {
            eprintln!(
                "note: duplicate clients ignored: {}",
                duplicates
                    .iter()
                    .map(|c| format!("{c:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // Build the (kind, scope) pairs from the catalogue, filtered by the
        // requested clients + scope. Reuses the picker's aggregation path so
        // multiple positional clients produce a single combined report.
        let chosen = filter_catalogue_by_clients(&clients, scope);
        if chosen.is_empty() {
            // User asked for clients that don't exist at the requested scope
            // (e.g. `--scope project codex` — codex is user-only). Bail with
            // an actionable error rather than silently printing "0 clients".
            let requested: Vec<String> = clients.iter().map(|c| format!("{c:?}")).collect();
            anyhow::bail!(
                "no catalogue entries match clients={:?} at scope={:?}. \
                 Try --scope all, or pick clients available at this scope.",
                requested,
                scope,
            );
        }
        run_ide_for_clients(&chosen, dry_run, no_watch, &cwd, &homes)?
    };

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

/// Per-scope detection state, attached to a `PickerItem`.
#[derive(Debug, Clone)]
pub struct ScopeOption {
    pub scope: Scope,
    pub path: PathBuf,
    /// Parent directory of `path` exists — proxy for "client appears installed".
    pub installed: bool,
    /// Config file exists — distinguishes "will create" vs "will merge".
    pub file_present: bool,
}

/// One picker row: a client and every scope it supports. The catalogue has 10
/// entries (Claude Code is the only client with both project + user rows) but
/// the picker shows one row per *kind* and uses a follow-up `Select` to
/// resolve scope only when there's a real choice to make.
#[derive(Debug, Clone)]
pub struct PickerItem {
    pub kind: ClientKind,
    pub scopes: Vec<ScopeOption>,
}

impl PickerItem {
    /// Picker default: pre-check the row if any of its scopes look installed.
    /// For project-scoped-only clients, "installed" is always true (the repo
    /// is the parent), so the row defaults to checked unless filtered out.
    pub fn any_installed(&self) -> bool {
        self.scopes.iter().any(|s| s.installed)
    }

    /// When a client supports multiple scopes, pre-select the first one that
    /// reports `installed`. Falls back to the first scope if none qualify.
    fn default_scope(&self) -> Scope {
        self.scopes
            .iter()
            .find(|s| s.installed)
            .map(|s| s.scope)
            .unwrap_or(self.scopes[0].scope)
    }
}

/// Outcome of the picker. `Selected(empty)` means "applied nothing"; this is
/// distinct from `Cancelled` (user hit Esc / answered No to the confirm).
pub enum PickerOutcome {
    Selected(Vec<(ClientKind, Scope)>),
    Cancelled,
}

/// Build the picker rows from the static catalogue. Pure: no I/O beyond
/// `Path::exists`, so it can be unit-tested with a `TempDir`-backed
/// `HomeDirs` and a sandbox `cwd`.
///
/// Returns one row per unique `ClientKind`, with one `ScopeOption` per
/// (kind, scope) entry in the catalogue. Claude Code ends up with two scope
/// options; every other client has one.
pub fn picker_items(cwd: &Path, homes: &HomeDirs) -> Vec<PickerItem> {
    let mut by_kind: Vec<PickerItem> = Vec::new();
    for entry in CLIENT_CATALOGUE.iter() {
        let path = match entry.scope {
            Scope::Project => match project_path(entry.kind, cwd) {
                Some(p) => p,
                None => continue,
            },
            Scope::User => match user_path(entry.kind, homes) {
                Some(p) => p,
                None => continue,
            },
        };
        let installed = match entry.scope {
            // Project parents always exist (it's the repo).
            Scope::Project => true,
            // Missing parent dir = client not installed on this machine.
            Scope::User => path.parent().is_some_and(Path::exists),
        };
        let opt = ScopeOption {
            scope: entry.scope,
            file_present: path.exists(),
            installed,
            path,
        };
        match by_kind.iter_mut().find(|i| i.kind == entry.kind) {
            Some(existing) => existing.scopes.push(opt),
            None => by_kind.push(PickerItem {
                kind: entry.kind,
                scopes: vec![opt],
            }),
        }
    }
    by_kind
}

/// Label for the per-client MultiSelect row. Shows how many scopes are
/// available so users know a follow-up scope prompt will appear.
pub fn format_picker_label(item: &PickerItem) -> String {
    let scope_hint = if item.scopes.len() > 1 {
        "project + user available".to_string()
    } else {
        match item.scopes[0].scope {
            Scope::Project => "project".to_string(),
            Scope::User => "user".to_string(),
        }
    };
    let status = if item.scopes.len() == 1 {
        scope_option_status(&item.scopes[0])
    } else {
        // Multi-scope: report the most useful state across scopes.
        item.scopes
            .iter()
            .map(scope_option_status)
            .find(|s| *s != "not installed")
            .unwrap_or("not installed")
    };
    format!(
        "{name:<16} {hint:<28}  {status}",
        name = client_display_name(item.kind),
        hint = scope_hint,
        status = status,
    )
}

/// Status string for a single scope option.
fn scope_option_status(opt: &ScopeOption) -> &'static str {
    match (opt.installed, opt.file_present) {
        (false, _) => "not installed",
        (true, true) => "present, will merge",
        (true, false) => "will create",
    }
}

/// Render `~` for paths under `$HOME`. Cosmetic only.
fn home_relative(path: &Path) -> String {
    if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// Hybrid picker: one MultiSelect of clients, then a per-client scope prompt
/// only for clients that support more than one scope, then a final Confirm.
fn interactive_picker(items: &[PickerItem]) -> Result<PickerOutcome> {
    eprintln!("{CARTOG_LOGO}");

    if items.is_empty() {
        eprintln!("No MCP-aware clients are known to cartog. Nothing to configure.");
        return Ok(PickerOutcome::Cancelled);
    }

    let theme = ColorfulTheme::default();

    // Step 1 — which clients?
    let labels: Vec<String> = items.iter().map(format_picker_label).collect();
    let defaults: Vec<bool> = items.iter().map(PickerItem::any_installed).collect();

    let selection = MultiSelect::with_theme(&theme)
        .with_prompt("Step 1 — Which clients to configure? (space to toggle, enter to confirm)")
        .items(&labels)
        .defaults(&defaults)
        .interact_opt()
        .context("interactive picker failed")?;

    let Some(indices) = selection else {
        return Ok(PickerOutcome::Cancelled);
    };
    if indices.is_empty() {
        eprintln!("Nothing selected.");
        return Ok(PickerOutcome::Cancelled);
    }

    // Step 2 — resolve scope per chosen client. Only prompts when there's an
    // actual choice; single-scope clients are kept as-is.
    let mut chosen: Vec<(ClientKind, Scope)> = Vec::with_capacity(indices.len());
    for &i in &indices {
        let item = &items[i];
        let scope = if item.scopes.len() == 1 {
            item.scopes[0].scope
        } else {
            prompt_scope(&theme, item)?
        };
        chosen.push((item.kind, scope));
    }

    // Step 3 — final confirmation showing the resolved plan.
    eprintln!(
        "\nStep 3 — Confirm. Will configure {} client(s):",
        chosen.len()
    );
    for (kind, scope) in &chosen {
        let item = items.iter().find(|i| i.kind == *kind).unwrap();
        let opt = item.scopes.iter().find(|s| s.scope == *scope).unwrap();
        eprintln!(
            "  · {name:<16} {scope:<8} {path}",
            name = client_display_name(*kind),
            scope = scope_label(*scope),
            path = home_relative(&opt.path),
        );
    }

    let confirmed = Confirm::with_theme(&theme)
        .with_prompt("Apply?")
        .default(true)
        .interact_opt()
        .context("confirmation failed")?
        .unwrap_or(false);
    if !confirmed {
        eprintln!("Aborted, no files were modified.");
        return Ok(PickerOutcome::Cancelled);
    }

    Ok(PickerOutcome::Selected(chosen))
}

/// Run a `Select` prompt asking which scope to use for a multi-scope client.
/// Only invoked when `item.scopes.len() > 1`.
fn prompt_scope(theme: &ColorfulTheme, item: &PickerItem) -> Result<Scope> {
    use dialoguer::Select;

    let options: Vec<String> = item
        .scopes
        .iter()
        .map(|opt| {
            format!(
                "{scope:<8} {path}  ({status})",
                scope = scope_label(opt.scope),
                path = home_relative(&opt.path),
                status = scope_option_status(opt),
            )
        })
        .collect();

    let default_idx = item
        .scopes
        .iter()
        .position(|s| s.scope == item.default_scope())
        .unwrap_or(0);

    let idx = Select::with_theme(theme)
        .with_prompt(format!(
            "Step 2 — Where should {} write its MCP entry?",
            client_display_name(item.kind)
        ))
        .items(&options)
        .default(default_idx)
        .interact()
        .context("scope prompt failed")?;

    Ok(item.scopes[idx].scope)
}

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Project => "project",
        Scope::User => "user",
    }
}

/// Human-friendly client name for the picker. Mirrors `ClientKind`'s
/// `clap::ValueEnum` rendering but with capitalised display labels.
fn client_display_name(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::ClaudeCode => "Claude Code",
        ClientKind::ClaudeDesktop => "Claude Desktop",
        ClientKind::Cursor => "Cursor",
        ClientKind::Vscode => "VS Code",
        ClientKind::Windsurf => "Windsurf",
        ClientKind::Opencode => "OpenCode",
        ClientKind::Zed => "Zed",
        ClientKind::Codex => "Codex CLI",
        ClientKind::Gemini => "Gemini CLI",
    }
}

/// Run `run_ide` for an explicit set of (kind, scope) pairs chosen by the
/// picker. Bypasses the catalog filter pipeline because the picker has
/// already nailed the exact rows to apply.
fn run_ide_for_clients(
    chosen: &[(ClientKind, Scope)],
    dry_run: bool,
    no_watch: bool,
    cwd: &Path,
    homes: &HomeDirs,
) -> Result<IdeReport> {
    let specs: Vec<ClientSpec> = chosen
        .iter()
        .filter_map(|&(kind, scope)| spec_for(kind, scope, no_watch, cwd, homes))
        .collect();

    let mut steps = Vec::with_capacity(specs.len());
    let mut summary = IdeSummary::default();
    for spec in specs {
        // `interactive = false` here: the picker already prompted, so per-spec
        // confirmation prompts would be redundant.
        let step = process_spec(&spec, cwd, false, dry_run);
        summary.total += 1;
        match step.status {
            IdeStatus::Created => summary.created += 1,
            IdeStatus::Updated => summary.updated += 1,
            IdeStatus::Unchanged => summary.unchanged += 1,
            IdeStatus::Skipped => summary.skipped += 1,
            IdeStatus::Error => summary.error += 1,
        }
        steps.push(step);
    }

    Ok(IdeReport { steps, summary })
}

/// Build a single `ClientSpec` for an explicit kind/scope pair (picker path).
fn spec_for(
    kind: ClientKind,
    scope: Scope,
    no_watch: bool,
    cwd: &Path,
    homes: &HomeDirs,
) -> Option<ClientSpec> {
    let entry = CLIENT_CATALOGUE
        .iter()
        .find(|e| e.kind == kind && e.scope == scope)?;
    let path = match entry.scope {
        Scope::Project => project_path(entry.kind, cwd)?,
        Scope::User => user_path(entry.kind, homes)?,
    };
    Some(ClientSpec {
        kind: entry.kind,
        scope: entry.scope,
        path,
        strategy: entry.strategy,
        args: entry.args_kind.resolve(no_watch),
    })
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

    // ── cartog install positional dispatch ─────────────────────────────

    #[test]
    fn install_filter_single_client_returns_only_that_kind() {
        let chosen = filter_catalogue_by_clients(&[ClientKind::Cursor], IdeScope::All);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].0, ClientKind::Cursor);
    }

    #[test]
    fn install_filter_multiple_clients_returns_each_in_catalogue_order() {
        let chosen = filter_catalogue_by_clients(
            &[ClientKind::Cursor, ClientKind::Vscode, ClientKind::Codex],
            IdeScope::All,
        );
        // All three requested clients are present.
        let kinds: Vec<_> = chosen.iter().map(|(k, _)| *k).collect();
        assert!(kinds.contains(&ClientKind::Cursor));
        assert!(kinds.contains(&ClientKind::Vscode));
        assert!(kinds.contains(&ClientKind::Codex));
        assert_eq!(chosen.len(), 3);
    }

    #[test]
    fn install_filter_claude_code_returns_both_project_and_user_scopes() {
        let chosen = filter_catalogue_by_clients(&[ClientKind::ClaudeCode], IdeScope::All);
        // Claude Code is the only client with both Project and User entries
        // in CLIENT_CATALOGUE — `cartog install claude-code` must wire both.
        assert_eq!(chosen.len(), 2);
        let scopes: Vec<_> = chosen.iter().map(|(_, s)| *s).collect();
        assert!(scopes.contains(&Scope::Project));
        assert!(scopes.contains(&Scope::User));
    }

    #[test]
    fn install_filter_respects_project_scope() {
        // Cursor exists only at project scope; codex only at user scope.
        // --scope project must drop the user-only entry.
        let chosen = filter_catalogue_by_clients(
            &[ClientKind::Cursor, ClientKind::Codex],
            IdeScope::Project,
        );
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].0, ClientKind::Cursor);
        assert_eq!(chosen[0].1, Scope::Project);
    }

    #[test]
    fn install_filter_empty_clients_returns_empty() {
        // Empty positional list is handled by the caller (falls back to
        // `run_ide(None, ...)`); the filter helper itself returns nothing.
        let chosen = filter_catalogue_by_clients(&[], IdeScope::All);
        assert!(chosen.is_empty());
    }

    #[test]
    fn dedupe_drops_repeats_and_reports_them() {
        let (unique, dropped) = dedupe_preserving_order(vec![
            ClientKind::Cursor,
            ClientKind::Vscode,
            ClientKind::Cursor,
            ClientKind::Cursor,
            ClientKind::Codex,
        ]);
        assert_eq!(
            unique,
            vec![ClientKind::Cursor, ClientKind::Vscode, ClientKind::Codex]
        );
        assert_eq!(dropped, vec![ClientKind::Cursor, ClientKind::Cursor]);
    }

    #[test]
    fn dedupe_preserves_first_occurrence_order() {
        let (unique, dropped) = dedupe_preserving_order(vec![
            ClientKind::Vscode,
            ClientKind::Cursor,
            ClientKind::Vscode,
        ]);
        assert_eq!(unique, vec![ClientKind::Vscode, ClientKind::Cursor]);
        assert_eq!(dropped, vec![ClientKind::Vscode]);
    }

    #[test]
    fn dedupe_empty_input_returns_two_empty_vecs() {
        let (unique, dropped) = dedupe_preserving_order(Vec::new());
        assert!(unique.is_empty());
        assert!(dropped.is_empty());
    }

    #[test]
    fn install_filter_user_only_client_at_project_scope_yields_empty() {
        // Reproduces the F2 review finding: `cartog install --scope project codex`
        // would silently succeed with "0 clients" before the bail was added.
        // Codex is user-only, so the filter yields an empty vec — cmd_install
        // bails with an error message instead of running.
        let chosen = filter_catalogue_by_clients(&[ClientKind::Codex], IdeScope::Project);
        assert!(
            chosen.is_empty(),
            "codex has no Project entry in the catalogue"
        );
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

    // ── Picker helpers ────────────────────────────────────────────────────

    fn opt(scope: Scope, installed: bool, file_present: bool) -> ScopeOption {
        ScopeOption {
            scope,
            path: PathBuf::from("/tmp/foo"),
            installed,
            file_present,
        }
    }

    #[test]
    fn scope_option_status_reports_not_installed_when_parent_missing() {
        assert_eq!(
            scope_option_status(&opt(Scope::User, false, false)),
            "not installed"
        );
    }

    #[test]
    fn scope_option_status_reports_will_create_when_parent_exists_but_file_does_not() {
        assert_eq!(
            scope_option_status(&opt(Scope::Project, true, false)),
            "will create"
        );
    }

    #[test]
    fn scope_option_status_reports_will_merge_when_file_exists() {
        assert_eq!(
            scope_option_status(&opt(Scope::User, true, true)),
            "present, will merge"
        );
    }

    #[test]
    fn picker_items_groups_claude_code_into_two_scopes() {
        // Claude Code is the only client with both Project and User catalogue
        // entries; the hybrid picker collapses them into one PickerItem with
        // two ScopeOptions.
        let tmp = tempfile::tempdir().unwrap();
        let homes = HomeDirs::default();
        let items = picker_items(tmp.path(), &homes);
        let cc = items
            .iter()
            .find(|i| i.kind == ClientKind::ClaudeCode)
            .unwrap();
        assert_eq!(cc.scopes.len(), 2, "Claude Code should have 2 scopes");
        let scopes: Vec<Scope> = cc.scopes.iter().map(|s| s.scope).collect();
        assert!(scopes.contains(&Scope::Project));
        assert!(scopes.contains(&Scope::User));
    }

    #[test]
    fn picker_items_other_clients_have_a_single_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let homes = HomeDirs::default();
        let items = picker_items(tmp.path(), &homes);
        for item in &items {
            if item.kind != ClientKind::ClaudeCode {
                assert_eq!(
                    item.scopes.len(),
                    1,
                    "{:?} should have exactly one scope, got {}",
                    item.kind,
                    item.scopes.len(),
                );
            }
        }
    }

    #[test]
    fn picker_items_marks_project_clients_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let homes = HomeDirs::default();
        let items = picker_items(tmp.path(), &homes);
        let cursor = items.iter().find(|i| i.kind == ClientKind::Cursor).unwrap();
        // Project-scoped rows always read as "installed" — the repo IS the parent.
        assert!(cursor.scopes[0].installed);
        assert!(!cursor.scopes[0].file_present);
    }

    #[test]
    fn picker_items_marks_user_clients_not_installed_when_parent_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let homes = HomeDirs {
            claude_code: tmp.path().join("does/not/exist/claude.json"),
            claude_desktop: tmp.path().join("does/not/exist/desktop.json"),
            codex: tmp.path().join("does/not/exist/codex.toml"),
            gemini: tmp.path().join("does/not/exist/gemini.json"),
            windsurf: tmp.path().join("does/not/exist/windsurf.json"),
            opencode: tmp.path().join("does/not/exist/opencode.json"),
            zed: tmp.path().join("does/not/exist/zed.json"),
        };
        let items = picker_items(tmp.path(), &homes);
        for item in &items {
            for opt in &item.scopes {
                if opt.scope == Scope::User {
                    assert!(
                        !opt.installed,
                        "{:?} user-scope should be flagged not-installed",
                        item.kind
                    );
                }
            }
        }
    }

    #[test]
    fn picker_items_marks_user_client_installed_when_parent_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_dir = tmp.path().join("Library/Application Support/Claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let homes = HomeDirs {
            claude_desktop: claude_dir.join("claude_desktop_config.json"),
            ..HomeDirs::default()
        };
        let items = picker_items(tmp.path(), &homes);
        let claude = items
            .iter()
            .find(|i| i.kind == ClientKind::ClaudeDesktop)
            .unwrap();
        assert!(claude.scopes[0].installed);
        assert!(!claude.scopes[0].file_present);
    }

    #[test]
    fn format_picker_label_single_scope_includes_name_and_status() {
        let item = PickerItem {
            kind: ClientKind::Cursor,
            scopes: vec![opt(Scope::Project, true, false)],
        };
        let label = format_picker_label(&item);
        assert!(label.contains("Cursor"), "label missing name: {label}");
        assert!(
            label.contains("project"),
            "label missing scope hint: {label}"
        );
        assert!(
            label.contains("will create"),
            "label missing status: {label}"
        );
    }

    #[test]
    fn format_picker_label_multi_scope_hints_at_choice() {
        let item = PickerItem {
            kind: ClientKind::ClaudeCode,
            scopes: vec![
                opt(Scope::Project, true, false),
                opt(Scope::User, true, true),
            ],
        };
        let label = format_picker_label(&item);
        assert!(label.contains("Claude Code"));
        assert!(
            label.contains("project + user available"),
            "multi-scope hint missing: {label}",
        );
    }

    #[test]
    fn any_installed_true_when_at_least_one_scope_installed() {
        let item = PickerItem {
            kind: ClientKind::ClaudeCode,
            scopes: vec![
                opt(Scope::Project, true, false),
                opt(Scope::User, false, false),
            ],
        };
        assert!(item.any_installed());
    }

    #[test]
    fn any_installed_false_when_no_scope_installed() {
        let item = PickerItem {
            kind: ClientKind::Zed,
            scopes: vec![opt(Scope::User, false, false)],
        };
        assert!(!item.any_installed());
    }

    #[test]
    fn spec_for_returns_none_for_unknown_combination() {
        // Cursor only exists at project scope; asking for user scope should
        // return None so the picker can't construct an impossible spec.
        let tmp = tempfile::tempdir().unwrap();
        let homes = HomeDirs::default();
        assert!(spec_for(ClientKind::Cursor, Scope::User, false, tmp.path(), &homes).is_none());
    }

    #[test]
    fn spec_for_builds_claude_code_with_or_without_watch() {
        let tmp = tempfile::tempdir().unwrap();
        let homes = HomeDirs::default();
        let with = spec_for(
            ClientKind::ClaudeCode,
            Scope::Project,
            false,
            tmp.path(),
            &homes,
        )
        .unwrap();
        let without = spec_for(
            ClientKind::ClaudeCode,
            Scope::Project,
            true,
            tmp.path(),
            &homes,
        )
        .unwrap();
        assert_eq!(with.args, vec!["serve", "--watch"]);
        assert_eq!(without.args, vec!["serve"]);
    }
}
