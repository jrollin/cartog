//! Running `cartog serve`/`watch` processes, described for the user.
//!
//! Peers are found through their PID lock files, so a read-only `serve`
//! secondary (which holds no lock) is never listed.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Serialize;

/// How many processes a message names before collapsing the rest into a count.
pub(crate) const PEER_LIST_CAP: usize = 5;

/// One running cartog process holding at least one serve/watch lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RunningPeer {
    pub pid: u32,
    /// The project root from the registry, when the lock maps to a known project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Lock slots this process holds (a `serve --watch` holds two), sorted.
    pub slots: Vec<String>,
}

impl RunningPeer {
    /// The project root, else the first lock slot: the only other name we have.
    #[must_use]
    pub(crate) fn label(&self) -> &str {
        self.project
            .as_deref()
            .or_else(|| self.slots.first().map(String::as_str))
            .unwrap_or("unknown")
    }
}

/// The DB hash a slot is keyed on: `serve-<hash>` and `watch-<hash>` share it.
fn slot_hash(slot: &str) -> Option<&str> {
    slot.split_once('-').map(|(_, hash)| hash)
}

/// Group a `(slot, pid)` lock snapshot by PID, label each process with the
/// project root `roots` maps its slot hash to, and sort by PID. Takes
/// [`super::peer_lock_snapshot`] output, which already drops the update lock.
#[must_use]
pub(crate) fn group_peers(
    locks: &[(String, u32)],
    roots: &HashMap<String, String>,
) -> Vec<RunningPeer> {
    let mut by_pid: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (slot, pid) in locks {
        by_pid.entry(*pid).or_default().push(slot.clone());
    }
    by_pid
        .into_iter()
        .map(|(pid, mut slots)| {
            slots.sort();
            slots.dedup();
            let project = slots
                .iter()
                .find_map(|slot| slot_hash(slot).and_then(|h| roots.get(h)))
                .cloned();
            RunningPeer {
                pid,
                project,
                slots,
            }
        })
        .collect()
}

/// Registry rows keyed by slot hash, so a lock can be named by its project.
/// Best-effort: an absent or unreadable registry yields an empty map.
fn registry_roots() -> HashMap<String, String> {
    cartog_registry::list_projects(cartog_db::CURRENT_SCHEMA_VERSION)
        .projects
        .into_iter()
        .filter_map(|row| {
            let hash = slot_hash(&row.id)?.to_string();
            Some((hash, row.root.display().to_string()))
        })
        .collect()
}

/// Every running serve/watch process found in `state_dir`, named by project.
#[must_use]
pub(crate) fn running_peers(state_dir: &Path) -> Vec<RunningPeer> {
    let locks = super::peer_lock_snapshot(state_dir);
    if locks.is_empty() {
        return Vec::new();
    }
    group_peers(&locks, &registry_roots())
}

/// `PID 91841 (/w/app), PID 10428 (watch-…)`, capped at [`PEER_LIST_CAP`].
#[must_use]
pub(crate) fn format_peer_list(peers: &[RunningPeer]) -> String {
    let mut parts: Vec<String> = peers
        .iter()
        .take(PEER_LIST_CAP)
        .map(|p| format!("PID {} ({})", p.pid, p.label()))
        .collect();
    if peers.len() > PEER_LIST_CAP {
        parts.push(format!("and {} more", peers.len() - PEER_LIST_CAP));
    }
    parts.join(", ")
}

fn processes(n: usize) -> &'static str {
    if n == 1 {
        "process is"
    } else {
        "processes are"
    }
}

/// The exit-6 explanation for a plain `cartog self update`.
///
/// `windows` drops the `--force` hint: a running `cartog.exe` cannot be
/// replaced, so closing the processes is the only way through.
#[must_use]
pub(crate) fn peer_refusal_message(peers: &[RunningPeer], windows: bool) -> String {
    let head = format!(
        "{} cartog {} running: {}. Read-only sessions on the same projects are not listed.",
        peers.len(),
        processes(peers.len()),
        format_peer_list(peers),
    );
    if windows {
        format!(
            "{head} Close them and retry; `--force` is unavailable on Windows because a \
             running cartog.exe cannot be replaced."
        )
    } else {
        format!(
            "{head} Stop them and retry, or run `cartog self update --force` to swap the \
             binary now (they keep the old version until restarted)."
        )
    }
}

/// What a forced swap tells the user about processes left on the old binary.
#[must_use]
pub(crate) fn stale_peers_message(peers: &[RunningPeer], target: &str) -> String {
    format!(
        "cartog: {} {} still running the previous version: {}. Read-only sessions on the same \
         projects are not listed and also keep it. Restart them to use {target}.",
        peers.len(),
        processes(peers.len()),
        format_peer_list(peers),
    )
}
