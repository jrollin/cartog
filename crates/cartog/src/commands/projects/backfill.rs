//! `cartog projects add` / `scan` — registering an index that already exists.
//!
//! Registration is normally a side effect of a write (`index`, `rag index`,
//! `pull`, `serve` startup), which leaves a real gap: a project indexed last
//! month and untouched since never appears in the listing. These two commands
//! close it without indexing anything.
//!
//! Neither command ever *creates* an index. Registering a project cartog has
//! never indexed would put a row in the registry describing nothing, so a root
//! with no database is refused (`add`) or skipped (`scan`).
//!
//! Neither stamps `last_indexed` either: nothing here indexed anything, so the
//! row renders as `never` — see [`crate::registry_hook::record_backfilled`].

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use cartog_db::Database;
use cartog_registry::DeclaredUpdate;

use crate::config::{resolve_project_at, DeclaredAtRoot};
use crate::registry_hook::{record_backfilled, record_declared, resolve_declared};

#[derive(Debug, Serialize)]
struct AddedJson {
    registered: Vec<RegisteredJson>,
    /// How many visited directories held no index.
    ///
    /// A count, not a list: a `scan` walks every directory under the named
    /// root, so `--depth 2` over a work tree of 50 repos visits several
    /// hundred — and each entry would carry the same single reason
    /// ("no index here"), making the payload grow without conveying anything.
    /// The registered set is the answer to the user's question.
    #[serde(skip_serializing_if = "super::super::shared::is_zero")]
    skipped_no_index: usize,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct RegisteredJson {
    root: String,
    db_path: String,
    /// `None` when the database could not be measured (most often an older
    /// graph schema). The row is still written; the listing marks it
    /// `stale-schema` and shows `?` for the counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_count: Option<u32>,
}

/// Register one already-indexed project, without indexing it.
pub fn cmd_projects_add(path: &str, json: bool) -> Result<()> {
    require_registry()?;

    let root = absolutize(Path::new(path))
        .with_context(|| format!("could not resolve the project root '{path}'"))?;
    if !root.is_dir() {
        bail!("'{}' is not a directory", root.display());
    }

    let outcome = register_root(&root, false);
    let payload = match outcome {
        Outcome::Registered(row) => AddedJson {
            registered: vec![row],
            skipped_no_index: 0,
            dry_run: false,
        },
        // `add` names one project explicitly, so "there is no index here" is an
        // error the user can act on, not a line in a skip list.
        Outcome::NoIndex => bail!(
            "no cartog index at '{}' — run `cartog index` there first.\n\
             `projects add` registers an index that already exists; it never creates one.",
            root.display()
        ),
    };

    super::super::shared::output(&payload, json, None, |p| {
        let row = &p.registered[0];
        format!(
            "Registered '{}'.\n  database: {}\n\
             Its graph was not re-indexed, so `projects list` shows it as last indexed \
             'never' until the next `cartog index` there.\n",
            row.root, row.db_path,
        )
    })
}

/// Register every already-indexed project under a directory the user named.
pub fn cmd_projects_scan(dir: &str, depth: u32, dry_run: bool, json: bool) -> Result<()> {
    require_registry()?;

    let base = absolutize(Path::new(dir))
        .with_context(|| format!("could not resolve the scan directory '{dir}'"))?;
    if !base.is_dir() {
        bail!("'{}' is not a directory", base.display());
    }

    let mut registered = Vec::new();
    let mut skipped_no_index = 0usize;
    for root in candidate_roots(&base, depth) {
        match register_root(&root, dry_run) {
            Outcome::Registered(row) => registered.push(row),
            Outcome::NoIndex => skipped_no_index += 1,
        }
    }

    let payload = AddedJson {
        registered,
        skipped_no_index,
        dry_run,
    };

    super::super::shared::output(&payload, json, None, |p| {
        if p.registered.is_empty() {
            return format!(
                "No cartog index found under '{}' ({} director{} searched, {} level{} deep).\n",
                base.display(),
                p.skipped_no_index,
                if p.skipped_no_index == 1 { "y" } else { "ies" },
                depth,
                if depth == 1 { "" } else { "s" },
            );
        }
        let verb = if p.dry_run {
            "Would register"
        } else {
            "Registered"
        };
        let mut out = format!(
            "{verb} {} project{}:\n",
            p.registered.len(),
            if p.registered.len() == 1 { "" } else { "s" },
        );
        for row in &p.registered {
            let counts = row
                .symbol_count
                .map_or_else(|| "not measurable".to_string(), |n| format!("{n} symbols"));
            out.push_str(&format!("  {} ({counts})\n", row.root));
        }
        if p.dry_run {
            out.push_str("\nNothing was written. Re-run without --dry-run to register these.\n");
        }
        out
    })
}

enum Outcome {
    Registered(RegisteredJson),
    /// No database at the resolved path — cartog has never indexed this root.
    NoIndex,
}

/// Register `root` if it holds a database, else report why not.
///
/// The database is opened **read-only** so a scan never migrates a schema or
/// takes a write lock on a project the user is working in elsewhere. A DB on an
/// older schema fails that open; the row is still written, from the
/// migration-free metadata probe alone, because a stale-schema project is
/// precisely the one worth being able to find — the listing flags it and shows
/// `?` for the counts it could not read.
fn register_root(root: &Path, dry_run: bool) -> Outcome {
    let resolved = resolve_project_at(root);
    if !resolved.db_path.is_file() {
        return Outcome::NoIndex;
    }

    // A rejected config declares *nothing*; it does not declare "no name". So
    // it must not overwrite a name/description an earlier working config
    // stored — `cartog index` draws the same line via `ProjectSource::Rejected`.
    let declared = match &resolved.declared {
        DeclaredAtRoot::Known { name, description } => DeclaredUpdate::Set(resolve_declared(
            name.as_deref(),
            description.as_deref(),
            root,
        )),
        DeclaredAtRoot::Unreadable => DeclaredUpdate::Keep,
    };

    let measured = Database::open_readonly(&resolved.db_path).ok();
    let symbol_count = measured
        .as_ref()
        .and_then(|db| db.stats().ok())
        .map(|s| s.num_symbols);

    if !dry_run {
        match measured {
            Some(db) => record_backfilled(&db, &resolved.db_path, root, declared),
            // Unmeasurable: still record identity + the metadata probe, which
            // is what surfaces the `stale-schema` marker.
            None => record_declared(&resolved.db_path, root, declared),
        }
    }

    Outcome::Registered(RegisteredJson {
        root: root.display().to_string(),
        db_path: resolved.db_path.display().to_string(),
        symbol_count,
    })
}

/// Every directory to consider under `base`, `base` itself included.
///
/// Walks at most `depth` levels below `base` and never follows symlinks: a
/// scan the user pointed at one directory must not escape it through a link.
/// Descends into a registered project too — a monorepo can hold nested indexes.
fn candidate_roots(base: &Path, depth: u32) -> Vec<PathBuf> {
    let mut out = vec![base.to_path_buf()];
    let mut frontier = vec![base.to_path_buf()];

    for _ in 0..depth {
        let mut next = Vec::new();
        for dir in &frontier {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                // `file_type` on the entry does not follow the link, so a
                // symlinked directory is skipped rather than descended.
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                if is_uninteresting_dirname(&path) {
                    continue;
                }
                out.push(path.clone());
                next.push(path);
            }
        }
        frontier = next;
    }
    out
}

/// Directories a scan must not descend into.
///
/// Dotdirs (`.git`, `.cartog`) and the usual dependency/build sinks: a
/// `node_modules` tree can hold thousands of directories and never a project
/// root the user meant.
fn is_uninteresting_dirname(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "target" | "vendor" | "dist" | "build" | "venv" | "__pycache__"
        )
}

/// Absolutize without requiring the path to exist yet, so the error message
/// can name the resolved path.
fn absolutize(path: &Path) -> Result<PathBuf> {
    let expanded = crate::config::expand_tilde(path.to_path_buf());
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    Ok(std::env::current_dir()
        .context("could not read the current directory")?
        .join(expanded))
}

/// Both commands write, so a disabled or unresolvable registry is a hard
/// failure here rather than the soft "nothing to show" a read command reports.
fn require_registry() -> Result<()> {
    if cartog_registry::registry_path().is_none() {
        bail!(
            "no project registry on this machine (CARTOG_REGISTRY is disabled, \
             or no state directory could be resolved)"
        );
    }
    Ok(())
}
