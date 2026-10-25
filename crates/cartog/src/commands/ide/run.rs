//! Command entry points (`cmd_ide`, `cmd_install`), orchestration, and atomic write.

use std::fs;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};

use super::catalogue::{
    build_specs, client_installed, client_name, detection, project_path, required_dir, serve_args,
    user_path, HomeDirs, CLIENT_CATALOGUE,
};
use super::merge::{codex_section_name, entry_disabled, merge_codex_toml, merge_entry};
use super::picker::{confirm, interactive_picker, picker_items, PickerOutcome};
use super::remove::cmd_ide_remove;
use super::rules::{apply_rule_plan, plan_rules, rule_selection, RuleSelection};
use super::{
    Action, ClientSpec, DiffPair, FileKind, IdeReport, IdeStatus, IdeStep, IdeSummary,
    MergeStrategy, Operation, Scope,
};
use crate::cli::{ClientKind, IdeScope};

/// Public CLI entry point.
#[allow(clippy::too_many_arguments)] // thin CLI adapter; each arg is a distinct flag
pub fn cmd_ide(
    client: Option<ClientKind>,
    scope: IdeScope,
    yes: bool,
    dry_run: bool,
    no_watch: bool,
    remove: bool,
    rules: bool,
    json: bool,
) -> Result<()> {
    if remove {
        return cmd_ide_remove(client, scope, yes, dry_run, json);
    }
    let cwd = std::env::current_dir()?;
    check_rules_scope(rules, scope, &cwd, home_dir().as_deref())?;
    let interactive =
        !yes && !dry_run && !json && client.is_none() && std::io::stdin().is_terminal();
    let homes = HomeDirs::detect();
    let path_env = std::env::var_os("PATH");

    // Interactive picker: shown when the user invokes `cartog ide` with no
    // explicit filter and a real TTY. Skipped under --yes/--dry-run/--json,
    // when --client is set, or when stdin is piped (CI/scripts).
    let report = if interactive {
        let items = picker_items(&cwd, &homes, path_env.as_deref());
        match interactive_picker(&items)? {
            PickerOutcome::Cancelled => return Ok(()),
            PickerOutcome::Selected(chosen) => {
                let picked: Vec<ClientKind> = chosen.iter().map(|&(kind, _)| kind).collect();
                let selection = rules.then_some(RuleSelection::Named(&picked));
                let plan = plan_rules(selection, &cwd, &homes, path_env.as_deref());
                let mut report = run_ide_for_clients(&chosen, dry_run, no_watch, &cwd, &homes)?;
                apply_rule_plan(&mut report, plan, &cwd, dry_run);
                report
            }
        }
    } else {
        let named: Vec<ClientKind> = client.into_iter().collect();
        let selection = rules.then_some(rule_selection(&named));
        // Evidence is read before the MCP writes, so this run's own folders never count.
        let plan = plan_rules(selection, &cwd, &homes, path_env.as_deref());
        let mut report = run_ide(client, scope, interactive, dry_run, no_watch, &cwd, &homes)?;
        ensure_ide_targets(client, scope, report.steps.len(), plan.targets.len())?;
        apply_rule_plan(&mut report, plan, &cwd, dry_run);
        report
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
    rules: bool,
    json: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    check_rules_scope(rules, scope, &cwd, home_dir().as_deref())?;
    let homes = HomeDirs::detect();
    let path_env = std::env::var_os("PATH");

    let report = if clients.is_empty() {
        // No positional clients → install into every detected editor in scope,
        // exactly like `cartog ide --yes`. The picker is skipped (yes implied).
        let selection = rules.then_some(RuleSelection::Detected);
        let plan = plan_rules(selection, &cwd, &homes, path_env.as_deref());
        let mut report = run_ide(None, scope, false, dry_run, no_watch, &cwd, &homes)?;
        ensure_ide_targets(None, scope, report.steps.len(), plan.targets.len())?;
        apply_rule_plan(&mut report, plan, &cwd, dry_run);
        report
    } else {
        // Dedupe so `cartog install cursor cursor` doesn't waste cycles; warn
        // so a script bug that produces dupes is visible.
        let (clients, duplicates) = dedupe_preserving_order(clients);
        if !duplicates.is_empty() {
            eprintln!(
                "note: duplicate clients ignored: {}",
                duplicates
                    .iter()
                    .map(|c| client_name(*c))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // Build the (kind, scope) pairs from the catalogue, filtered by the
        // requested clients + scope. Reuses the picker's aggregation path so
        // multiple positional clients produce a single combined report.
        let chosen = filter_catalogue_by_clients(&clients, scope);
        let selection = rules.then_some(RuleSelection::Named(&clients));
        let plan = plan_rules(selection, &cwd, &homes, path_env.as_deref());
        if chosen.is_empty() && plan.targets.is_empty() {
            // User asked for clients that don't exist at the requested scope
            // (e.g. `--scope project codex` — codex is user-only). Bail with
            // an actionable error rather than silently printing "0 clients".
            let requested: Vec<&str> = clients.iter().map(|c| client_name(*c)).collect();
            anyhow::bail!(
                "no catalogue entries match clients=[{}] at scope={}. \
                 Try --scope all, or pick clients available at this scope.",
                requested.join(", "),
                scope_slug(scope),
            );
        }
        let mut report = run_ide_for_clients(&chosen, dry_run, no_watch, &cwd, &homes)?;
        apply_rule_plan(&mut report, plan, &cwd, dry_run);
        report
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

/// `--rules` writes project files, which a user-scope run never touches. In the home
/// directory the same paths are user scope: `~/.claude/rules/` and `~/.kiro/steering/`
/// apply to every project.
pub(super) fn check_rules_scope(
    rules: bool,
    scope: IdeScope,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<()> {
    if !rules {
        return Ok(());
    }
    if scope == IdeScope::User {
        anyhow::bail!("rule files are project files: --rules needs --scope project or --scope all");
    }
    if home.is_some_and(|home| same_dir(cwd, home)) {
        anyhow::bail!(
            "rule files are project files: run --rules from a project directory, not your \
             home directory ({}), where agent rule dirs apply to every project",
            cwd.display()
        );
    }
    Ok(())
}

fn home_dir() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// A run with neither an MCP config row nor a rule file to touch is a filter mistake.
pub(super) fn ensure_ide_targets(
    client: Option<ClientKind>,
    scope: IdeScope,
    mcp_rows: usize,
    rule_targets: usize,
) -> Result<()> {
    if mcp_rows + rule_targets > 0 {
        return Ok(());
    }
    anyhow::bail!(
        "no clients match the requested filter (client={} + scope={}). \
         Check `--client` and `--scope` are compatible \
         (e.g. codex and copilot-cli are user-scoped only); `--scope all` covers both.",
        client.map_or("all", client_name),
        scope_slug(scope),
    );
}

/// Run `run_ide` for an explicit set of (kind, scope) pairs chosen by the
/// picker. Bypasses the catalog filter pipeline because the picker has
/// already nailed the exact rows to apply.
pub(super) fn run_ide_for_clients(
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
        summary.record(step.status);
        steps.push(step);
    }

    Ok(IdeReport {
        operation: Operation::Install,
        dry_run,
        steps,
        summary,
    })
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
        detection: detection(entry.kind, entry.scope, cwd, homes),
        requires: required_dir(entry.kind, entry.scope, homes),
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

/// Wire every catalogue row matching `client` + `scope`. No matching row yields an
/// empty report: the caller decides whether that is an error, since rule files may
/// still be due (`ensure_ide_targets`).
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
    let mut steps = Vec::with_capacity(specs.len());
    let mut summary = IdeSummary::default();

    // No explicit client → only wire clients we detect as in use.
    // An explicit `--client X` always proceeds, even if X isn't detected.
    let auto_only = client.is_none();
    for spec in specs {
        let step = process_spec(&spec, cwd, interactive, dry_run, auto_only);
        summary.record(step.status);
        steps.push(step);
    }

    Ok(IdeReport {
        operation: Operation::Install,
        dry_run,
        steps,
        summary,
    })
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
        file_kind: FileKind::McpConfig,
        status,
        message,
        diff,
    };

    if let Some(dir) = spec.requires.as_deref().filter(|d| !d.is_dir()) {
        return make(
            IdeStatus::Skipped,
            format!(
                "{} not found; install VS Code and the extension first",
                dir.display()
            ),
            None,
        );
    }

    // In the auto path (no explicit client), skip clients we can't detect as in
    // use. An explicitly-requested client is always wired.
    if auto_only
        && !client_installed(
            spec.kind,
            &spec.path,
            &spec.detection,
            std::env::var_os("PATH").as_deref(),
        )
    {
        return make(
            IdeStatus::Skipped,
            "client not detected (no CLI on PATH, no config or project folder)".into(),
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
            // Only a parse failure is about JSONC syntax; a wrong shape needs the generic fix.
            let jsonc_syntax = spec.path.extension().is_some_and(|ext| ext == "jsonc")
                && e.downcast_ref::<serde_json::Error>().is_some();
            let next = if jsonc_syntax {
                "JSONC comments or trailing commas are not rewritten; add the cartog entry by hand"
            } else {
                "fix the file or remove it and re-run"
            };
            return make(
                IdeStatus::Skipped,
                format!("not modified ({e}); {next}"),
                None,
            );
        }
    };

    // Split `Action::Unchanged` off into its terminal branch so the rest of the
    // function only deals with the two write-causing variants. The bound
    // `write_action` lets us produce planned/done strings without an
    // `unreachable!()` arm later.
    let note = if entry_disabled(spec.strategy, &outcome.new_json) {
        " (cartog is disabled in this client; turn it on there)"
    } else {
        ""
    };
    let write_action = match outcome.action {
        Action::Unchanged => {
            return make(
                IdeStatus::Unchanged,
                format!("already up to date{note}"),
                None,
            );
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
            format!("{}{note}", write_action.planned()),
            Some(DiffPair {
                before: existing,
                after: outcome.new_json,
            }),
        );
    }

    if let Err(e) = atomic_write(&spec.path, &outcome.new_json) {
        return make(IdeStatus::Error, format!("write failed: {e}"), None);
    }

    make(status, format!("{}{note}", write_action.done()), None)
}

pub(super) fn scope_slug(scope: IdeScope) -> &'static str {
    match scope {
        IdeScope::Project => "project",
        IdeScope::User => "user",
        IdeScope::All => "all",
    }
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

pub(super) fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    // Renaming over a symlink would replace it with a regular file and leave the
    // real config (often in a dotfiles repo) stale: write at the link's target.
    let resolved = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)
            .with_context(|| format!("could not resolve symlink {}", path.display()))?,
        _ => path.to_path_buf(),
    };
    let path = resolved.as_path();
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
    // Keep the original mode: a config can hold API keys in `env`.
    let perms = fs::metadata(path).ok().map(|meta| meta.permissions());
    let result = write_and_swap(&tmp, path, contents, perms.as_ref());
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_and_swap(
    tmp: &Path,
    path: &Path,
    contents: &str,
    perms: Option<&fs::Permissions>,
) -> Result<()> {
    use std::io::Write;

    let mut file =
        create_tmp(tmp, perms).with_context(|| format!("could not create {}", tmp.display()))?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("could not write {}", tmp.display()))?;
    // `create_tmp`'s mode is masked by the umask: set the exact one.
    if let Some(perms) = perms {
        fs::set_permissions(tmp, perms.clone())
            .with_context(|| format!("could not copy permissions onto {}", tmp.display()))?;
    }
    fs::rename(tmp, path)
        .with_context(|| format!("could not move tmp file into {}", path.display()))
}

/// Create the temp file fresh (a stale or planted one is replaced, never written
/// through) and, on Unix, with the final mode before any byte of the config lands.
pub(super) fn create_tmp(tmp: &Path, perms: Option<&fs::Permissions>) -> std::io::Result<fs::File> {
    let _ = fs::remove_file(tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(perms) = perms {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(perms.mode());
    }
    #[cfg(not(unix))]
    let _ = perms;
    options.open(tmp)
}
