//! `cartog self migrate-db`: move a legacy .cartog.db into the new layout.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::state;

// ── migrate-db ──────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PlannedMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Plan the moves needed to migrate `.cartog.db` (+ -wal, -shm, .pre-v*.bak)
/// at `root` into `.cartog/`. Empty vec when nothing to migrate. Errors when
/// any destination already exists: we never overwrite.
pub(crate) fn plan_migration(root: &Path) -> Result<Vec<PlannedMove>> {
    let legacy_db = root.join(cartog_db::LEGACY_DB_FILE);
    if !legacy_db.exists() {
        return Ok(Vec::new());
    }
    // fs::rename moves the link, not the target. Symlinks usually point to a
    // shared / network DB the user wants to keep where it is.
    let meta = std::fs::symlink_metadata(&legacy_db)
        .map_err(|e| anyhow::anyhow!("stat {}: {e}", legacy_db.display()))?;
    if meta.file_type().is_symlink() {
        anyhow::bail!(
            "refusing to migrate {}: it is a symlink. Resolve or update it manually.",
            legacy_db.display()
        );
    }
    let new_dir = root.join(cartog_db::DB_DIR);
    let new_db = new_dir.join(cartog_db::DB_FILENAME);

    let mut moves = Vec::new();
    let mut push_move = |from: PathBuf, to: PathBuf| -> Result<()> {
        if to.exists() {
            anyhow::bail!("refusing to migrate: {} already exists", to.display());
        }
        // Same symlink guard as legacy_db above: fs::rename moves the link, not the target.
        let meta = std::fs::symlink_metadata(&from)
            .map_err(|e| anyhow::anyhow!("stat {}: {e}", from.display()))?;
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "refusing to migrate {}: it is a symlink. Resolve or update it manually.",
                from.display()
            );
        }
        moves.push(PlannedMove { from, to });
        Ok(())
    };

    push_move(legacy_db.clone(), new_db.clone())?;

    for suffix in ["-wal", "-shm"] {
        let from = root.join(format!("{}{suffix}", cartog_db::LEGACY_DB_FILE));
        if from.exists() {
            let to = new_dir.join(format!("{}{suffix}", cartog_db::DB_FILENAME));
            push_move(from, to)?;
        }
    }

    let entries = std::fs::read_dir(root)
        .map_err(|e| anyhow::anyhow!("read_dir({}): {e}", root.display()))?;
    let prefix = format!("{}.pre-v", cartog_db::LEGACY_DB_FILE);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(suffix) = name_str.strip_prefix(&prefix) else {
            continue;
        };
        let from = entry.path();
        let to = new_dir.join(format!("{}.pre-v{suffix}", cartog_db::DB_FILENAME));
        push_move(from, to)?;
    }

    Ok(moves)
}

/// Test seam: when set, skips the peer-lock check. Only honored in `cfg(test)` builds.
#[cfg(test)]
pub(crate) const TEST_SKIP_PEER_LOCK_ENV: &str = "CARTOG_TEST_SKIP_PEER_LOCK";

#[cfg(test)]
fn peer_lock_check_skipped() -> bool {
    std::env::var_os(TEST_SKIP_PEER_LOCK_ENV).is_some()
}

#[cfg(not(test))]
fn peer_lock_check_skipped() -> bool {
    false
}

/// Serve/watch lock slots a peer would hold if it were operating on this
/// project's database — both the legacy `.cartog.db` and the new
/// `.cartog/db.sqlite` under `root`, since a serve started before migration is
/// holding the legacy path's slot. Used to scope the migrate-db peer check to
/// this DB instead of every cartog process on the machine.
pub(crate) fn target_db_slots(root: &Path) -> Vec<String> {
    let paths = [
        root.join(cartog_db::LEGACY_DB_FILE),
        root.join(cartog_db::DB_DIR).join(cartog_db::DB_FILENAME),
    ];
    let mut slots = Vec::with_capacity(paths.len() * 2);
    for p in &paths {
        slots.push(state::slot_for_db("serve", p));
        slots.push(state::slot_for_db("watch", p));
    }
    slots
}

/// Peer-lock guard decision for `migrate-db`, factored out so it can be unit
/// tested without touching the filesystem or process state. A dry-run mutates
/// nothing, so it never blocks on a live peer; a real migration bails only if a
/// peer holds a lock for *this* project's DB (`relevant_slots`) — a serve in an
/// unrelated project must not block the migration.
pub(crate) fn migrate_peer_guard(
    dry_run: bool,
    active: &[cartog_process_lock::ActiveLock],
    relevant_slots: &[String],
) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    if let Some(peer) = active
        .iter()
        .find(|p| relevant_slots.iter().any(|s| s == &p.slot))
    {
        anyhow::bail!(
            "another cartog process is running on this database ({slot}, PID {pid}); \
             stop it before migrating",
            slot = peer.slot,
            pid = peer.pid,
        );
    }
    Ok(())
}

/// `cartog self migrate-db [--dry-run]`. Moves legacy DB files into `.cartog/`.
/// Refuses to run while another cartog peer holds the lock.
pub fn cmd_self_migrate_db(root: &Path, dry_run: bool, json: bool) -> Result<()> {
    if !peer_lock_check_skipped() {
        let active = state::default_state_dir()
            .map(|dir| cartog_process_lock::find_active_locks(&dir))
            .unwrap_or_default();
        migrate_peer_guard(dry_run, &active, &target_db_slots(root))?;
    }

    let preview = plan_migration(root)?;
    if preview.is_empty() {
        emit_migrate_result(root, &preview, false, json, "nothing-to-do");
        return Ok(());
    }

    if dry_run {
        emit_migrate_result(root, &preview, false, json, "dry-run");
        return Ok(());
    }

    // Checkpointing closes the WAL/SHM siblings, so re-plan afterwards.
    // Non-fatal: the post-rename sweep picks up any siblings left behind.
    let legacy_db = root.join(cartog_db::LEGACY_DB_FILE);
    if let Err(e) = cartog_db::checkpoint_wal(&legacy_db) {
        tracing::warn!(
            path = %legacy_db.display(),
            error = %e,
            "WAL checkpoint failed before migrate-db; proceeding anyway",
        );
    }
    let moves = plan_migration(root)?;

    let new_dir = root.join(cartog_db::DB_DIR);
    std::fs::create_dir_all(&new_dir)
        .map_err(|e| anyhow::anyhow!("create_dir_all({}): {e}", new_dir.display()))?;

    for mv in &moves {
        std::fs::rename(&mv.from, &mv.to).map_err(|e| {
            anyhow::anyhow!(
                "failed to move {} → {}: {e}",
                mv.from.display(),
                mv.to.display(),
            )
        })?;
    }

    // Sweep again: another SQLite reader may have re-created -wal/-shm
    // between the checkpoint and the renames. Move them too so the new
    // layout has the full set and the legacy path is empty.
    let mut extra_moves = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let from = root.join(format!("{}{suffix}", cartog_db::LEGACY_DB_FILE));
        if from.exists() {
            let meta = std::fs::symlink_metadata(&from)
                .map_err(|e| anyhow::anyhow!("stat {}: {e}", from.display()))?;
            if meta.file_type().is_symlink() {
                anyhow::bail!(
                    "refusing to migrate {}: it is a symlink. Resolve or update it manually.",
                    from.display()
                );
            }
            let to = new_dir.join(format!("{}{suffix}", cartog_db::DB_FILENAME));
            if !to.exists() {
                std::fs::rename(&from, &to).map_err(|e| {
                    anyhow::anyhow!(
                        "post-move sweep failed to move {} → {}: {e}",
                        from.display(),
                        to.display(),
                    )
                })?;
                extra_moves.push(PlannedMove { from, to });
            }
        }
    }
    let mut all_moves = moves;
    all_moves.extend(extra_moves);

    emit_migrate_result(root, &all_moves, true, json, "migrated");
    Ok(())
}

#[derive(Debug, Serialize)]
struct MigrateOutcome<'a> {
    status: &'a str,
    root: String,
    performed: bool,
    moves: Vec<MigrateMove>,
}

#[derive(Debug, Serialize)]
struct MigrateMove {
    from: String,
    to: String,
}

fn emit_migrate_result(
    root: &Path,
    moves: &[PlannedMove],
    performed: bool,
    json: bool,
    status: &str,
) {
    if json {
        let outcome = MigrateOutcome {
            status,
            root: root.display().to_string(),
            performed,
            moves: moves
                .iter()
                .map(|m| MigrateMove {
                    from: m.from.display().to_string(),
                    to: m.to.display().to_string(),
                })
                .collect(),
        };
        println!(
            "{}",
            serde_json::to_string(&outcome).expect("MigrateOutcome serialises")
        );
        return;
    }
    if moves.is_empty() {
        println!(
            "cartog: no legacy database found at {} — nothing to migrate.",
            root.display()
        );
        return;
    }
    let verb = if performed { "Moved" } else { "Would move" };
    for m in moves {
        println!("{verb}: {} → {}", m.from.display(), m.to.display());
    }
    if !performed {
        println!("(dry run — pass without --dry-run to apply)");
    }
}
