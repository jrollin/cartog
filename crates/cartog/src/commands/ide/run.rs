//! Command entry points (`cmd_ide`, `cmd_install`), orchestration, and atomic write.

use std::fs;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};

use super::catalogue::{
    build_specs, client_installed, client_name, project_path, serve_args, user_path, HomeDirs,
    CLIENT_CATALOGUE,
};
use super::merge::{codex_section_name, merge_codex_toml, merge_entry};
use super::picker::{confirm, interactive_picker, picker_items, PickerOutcome};
use super::{
    Action, ClientSpec, DiffPair, IdeReport, IdeStatus, IdeStep, IdeSummary, MergeStrategy, Scope,
};
use crate::cli::{ClientKind, IdeScope};

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
pub(super) fn dedupe_preserving_order(
    clients: Vec<ClientKind>,
) -> (Vec<ClientKind>, Vec<ClientKind>) {
    // ClientKind isn't Hash and there are only a dozen variants, so a linear
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
pub(super) fn filter_catalogue_by_clients(
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
        // confirmation prompts would be redundant. `auto_only = false`: the user
        // explicitly picked these rows, so wire them even if detection is unsure.
        let step = process_spec(&spec, cwd, false, dry_run, false);
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
pub(super) fn spec_for(
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
        args: serve_args(no_watch),
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

    // No explicit client → only wire user clients we detect as installed.
    // An explicit `--client X` always proceeds, even if X isn't detected.
    let auto_only = client.is_none();
    for spec in specs {
        let step = process_spec(&spec, cwd, interactive, dry_run, auto_only);
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

pub(super) fn process_spec(
    spec: &ClientSpec,
    cwd: &Path,
    interactive: bool,
    dry_run: bool,
    auto_only: bool,
) -> IdeStep {
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

    // In the auto path (no explicit client), skip user clients we can't detect
    // as installed. An explicitly-requested client is always wired.
    if auto_only
        && spec.scope == Scope::User
        && !client_installed(
            spec.kind,
            spec.scope,
            &spec.path,
            std::env::var_os("PATH").as_deref(),
        )
    {
        return make(
            IdeStatus::Skipped,
            "client not detected (no CLI on PATH, no config directory)".into(),
            None,
        );
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
