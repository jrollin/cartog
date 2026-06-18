//! Interactive client/scope picker for `cartog ide` on a TTY.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect};

use super::catalogue::{
    client_display_name, client_installed, project_path, user_path, HomeDirs, CLIENT_CATALOGUE,
};
use super::{Scope, CARTOG_LOGO};
use crate::cli::ClientKind;

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

/// One picker row: a client and every scope it supports. The catalogue has
/// multiple entries per dual-scope client (Claude Code and Kiro each have
/// project + user rows) but the picker shows one row per *kind* and uses a
/// follow-up `Select` to resolve scope only when there's a real choice to make.
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
/// (kind, scope) entry in the catalogue. Claude Code and Kiro end up with two
/// scope options; every other client has one.
pub fn picker_items(cwd: &Path, homes: &HomeDirs) -> Vec<PickerItem> {
    let path_env = std::env::var_os("PATH");
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
        let installed = client_installed(entry.kind, entry.scope, &path, path_env.as_deref());
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
pub(super) fn scope_option_status(opt: &ScopeOption) -> &'static str {
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
pub(super) fn interactive_picker(items: &[PickerItem]) -> Result<PickerOutcome> {
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

/// Read one Y/n answer from stdin. Returns false on EOF — a closed stdin
/// must NOT be treated as consent (a script that pipes nothing would
/// otherwise silently apply every change). The caller is responsible for
/// gating interactive mode behind `stdin.is_terminal()` before invoking.
pub(super) fn confirm(label: &str) -> Result<bool> {
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
