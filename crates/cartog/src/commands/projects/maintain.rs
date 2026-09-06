//! `cartog projects forget` / `prune` — removing registry rows.
//!
//! Neither command touches a project's index. Forgetting a project means
//! forgetting where it is; the index stays exactly where it was, and a later
//! `cartog index` in that directory re-registers it.

use anyhow::{bail, Result};
use serde::Serialize;

use cartog_registry::Removed;

#[derive(Debug, Serialize)]
struct RemovedJson {
    registry_available: bool,
    dropped: Vec<String>,
    /// Populated only when a `forget` target matched several projects; the
    /// command then drops nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ambiguous: Vec<String>,
    dry_run: bool,
}

/// Drop one project's registry row.
pub fn cmd_projects_forget(target: &str, json: bool) -> Result<()> {
    let Some(registry) = cartog_registry::registry_path() else {
        bail!(
            "no project registry on this machine (CARTOG_REGISTRY is disabled, \
             or no state directory could be resolved)"
        );
    };
    let removed = cartog_registry::forget_project_at(&registry, target);

    if !removed.ambiguous.is_empty() {
        // Not an error the user can act on without the candidate list, so
        // print it rather than failing with a bare message.
        let payload = to_json(&removed, false);
        return super::super::shared::output(&payload, json, None, |_| {
            format!(
                "'{target}' matches {} projects; nothing was dropped. \
                 Re-run with one of these ids:\n{}\n",
                removed.ambiguous.len(),
                removed
                    .ambiguous
                    .iter()
                    .map(|id| format!("  {id}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        });
    }

    let payload = to_json(&removed, false);
    super::super::shared::output(&payload, json, None, |_| {
        if removed.unavailable {
            // `unavailable` here almost always means "there is no registry"
            // (nothing indexed on this machine yet, or it is disabled), not a
            // fault — say so, since "could not be opened" reads like an error
            // the user should chase.
            return "No project registry on this machine — nothing has been indexed yet, \
                    or CARTOG_REGISTRY is disabled. Nothing was dropped.\n"
                .to_string();
        }
        match removed.dropped.len() {
            0 => format!("No registered project matches '{target}'.\n"),
            _ => format!(
                "Forgot '{target}'. Its index was not touched — re-run `cartog index` \
                 there to register it again.\n"
            ),
        }
    })
}

/// Drop registry rows whose database file is gone.
pub fn cmd_projects_prune(dry_run: bool, json: bool) -> Result<()> {
    let Some(registry) = cartog_registry::registry_path() else {
        bail!(
            "no project registry on this machine (CARTOG_REGISTRY is disabled, \
             or no state directory could be resolved)"
        );
    };
    let removed = cartog_registry::prune_projects_at(&registry, dry_run);
    let payload = to_json(&removed, dry_run);

    super::super::shared::output(&payload, json, None, |_| {
        if removed.unavailable {
            return "No project registry found; nothing to prune.\n".to_string();
        }
        if removed.dropped.is_empty() {
            return "Every registered project's database still exists; nothing to prune.\n"
                .to_string();
        }
        let verb = if dry_run { "Would drop" } else { "Dropped" };
        format!(
            "{verb} {} row{} whose database no longer exists:\n{}\n",
            removed.dropped.len(),
            if removed.dropped.len() == 1 { "" } else { "s" },
            removed
                .dropped
                .iter()
                .map(|id| format!("  {id}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    })
}

fn to_json(removed: &Removed, dry_run: bool) -> RemovedJson {
    RemovedJson {
        registry_available: !removed.unavailable,
        dropped: removed.dropped.clone(),
        ambiguous: removed.ambiguous.clone(),
        dry_run,
    }
}
