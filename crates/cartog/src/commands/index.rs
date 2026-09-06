//! `cartog index` — build or rebuild the code graph index, including the
//! serve-peer LSP-deferral gate ([`lsp_defer_peer`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use super::progress::{install_cancel_probe, spinner_callback, stop_spinner, Spinner};
use super::shared::open_db_create;
use cartog_indexer as indexer;

/// Live serve peer to defer the LSP pass to, or `None` to run LSP locally.
///
/// Defer requires: LSP requested, not `--force` (force always runs LSP
/// locally), the `lsp` feature compiled in, a non-empty index (a first full
/// index pays the cold start rather than sealing the whole repo), and a live
/// DB-scoped serve lock. No state dir (sandboxed env) never defers.
fn lsp_defer_peer(
    db: &cartog_db::Database,
    lsp: bool,
    force: bool,
    state_dir: Option<PathBuf>,
    db_path: &Path,
) -> Option<cartog_process_lock::ActiveLock> {
    if !lsp || force || !cfg!(feature = "lsp") || !matches!(db.is_empty(), Ok(false)) {
        return None;
    }
    crate::state::detect_live_serve_peer(&state_dir?, db_path)
}

/// Build or rebuild the code graph index.
#[allow(clippy::too_many_arguments)] // thin CLI adapter over index_directory
pub fn cmd_index(
    db_path: &Path,
    path: &str,
    force: bool,
    lsp: bool,
    json: bool,
    embedding_dim: usize,
    redact: indexer::RedactionConfig,
    lsp_overrides: &std::collections::HashMap<String, Vec<String>>,
    filter: &indexer::WalkFilter,
    project: super::shared::ProjectSource<'_>,
) -> Result<()> {
    let root = Path::new(path);
    let db = open_db_create(db_path, embedding_dim)?;

    let deferred_peer = lsp_defer_peer(&db, lsp, force, crate::state::default_state_dir(), db_path);
    if let Some(peer) = &deferred_peer {
        tracing::info!(
            slot = %peer.slot,
            pid = peer.pid,
            "live `cartog serve` holds this DB's lock; deferring LSP to its warm servers (--force runs it locally)"
        );
    }
    let lsp = lsp && deferred_peer.is_none();

    // Ctrl-C → cancel probe, so the indexer (esp. the slow LSP phase) stops
    // cooperatively instead of being hard-killed mid-write.
    let cancel = install_cancel_probe();

    // Stderr-only; `Spinner::start` self-gates (TTY or CARTOG_PROGRESS), so --json stdout stays clean.
    let spinner = Spinner::start("Indexing").map(Arc::new);
    let cb = spinner_callback(&spinner, indexer::ProgressUpdate::label);
    let cb_ref: Option<indexer::ProgressCallback<'_>> =
        cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let cancel_ref: indexer::CancelProbe<'_> = &cancel;
    let result = indexer::index_directory(
        &db,
        root,
        force,
        lsp,
        cb_ref,
        Some(cancel_ref),
        redact,
        lsp_overrides,
        filter,
    );
    drop(cb);
    stop_spinner(spinner);
    let mut result = match result {
        Ok(r) => r,
        // Ctrl-C: the pass rolled back (single tx), so the index is unchanged.
        Err(e) if indexer::is_cancelled(&e) => {
            if !json {
                eprintln!("Indexing cancelled; the index was left unchanged.");
            }
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    // On a no-op run LSP wouldn't have run anyway — only report a real deferral.
    result.lsp_deferred_to_peer = deferred_peer.is_some() && result.dirty_files > 0;

    if !json && result.redaction_backfilled {
        eprintln!(
            "note: secret redaction was newly enabled; re-indexed all files to scrub stored content"
        );
    }
    if !json && result.lsp_deferred_to_peer {
        if let Some(peer) = &deferred_peer {
            eprintln!(
                "note: LSP resolution deferred to the running `cartog serve` (PID {}); use --force to run it locally",
                peer.pid
            );
        }
    }

    // Record the project in the machine-local registry. Placed after the last
    // early return above (cancellation rolls the whole pass back, so a
    // cancelled run must register nothing) and after the indexer's transaction
    // committed, never inside it.
    //
    // Gated on the pass having changed something: `record_indexed` pays a
    // `db.stats()` — five scans — and a no-op pass has nothing new to record.
    // A changed pass just wrote those tables, so their pages are warm.
    //
    // Nothing indexable at all registers nothing, matching the rule a degraded
    // `serve` already follows: advertising an empty project to every other
    // session on the machine is worse than omitting it. A later index that does
    // find symbols registers it then.
    //
    // The declared name/description are refreshed on **both** branches, since
    // a `[project] description` or README edit changes no byte of the database
    // — a no-op pass is exactly how such an edit reaches the registry. The
    // no-op branch uses `record_declared` rather than the config-less
    // `record_opened`, which would `Keep` the stale values, and it still pays
    // no `stats()`.
    //
    // A *rejected* config resolves to `Keep`, not to an empty `Set`: it never
    // knew the declared values, so overwriting them would let one parse error
    // erase them.
    let declared = super::shared::declared_update_for(project, root);
    let changed = result.files_indexed > 0 || result.files_removed > 0;
    if changed {
        crate::registry_hook::record_indexed(&db, db_path, root, declared);
    } else if matches!(db.is_empty(), Ok(false)) {
        crate::registry_hook::record_declared(db_path, root, declared);
    }

    // No-op run: nothing was added or removed this pass. The delta counters
    // are all zero, so the standard "0 symbols, 0 edges" line reads like a
    // failure. Report DB state instead — "up to date" when the index has
    // content, or "no indexable files" for an empty/unsupported tree.
    if !json && result.files_indexed == 0 && result.files_removed == 0 {
        let s = db.stats()?;
        if s.num_symbols == 0 {
            println!("No indexable files found under '{path}'.");
        } else {
            println!(
                "Index up to date ({} files, {} symbols unchanged)",
                s.num_files, s.num_symbols
            );
        }
        return Ok(());
    }

    super::shared::output(&result, json, None, indexer::render_index_summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_db::Database;

    /// The two caps must be the same number.
    ///
    /// The config side rejects over-length input; the registry side truncates
    /// at write time. A drift either rejects text the registry would happily
    /// have stored, or stores text the config swore was short enough. Pinned
    /// here rather than in `registry_hook`: that module lives in the library
    /// crate, which cannot see `config`, and this is the primary
    /// config-to-registry call site.
    #[test]
    fn the_config_and_registry_description_caps_are_the_same_number() {
        assert_eq!(
            crate::config::PROJECT_DESCRIPTION_MAX_CHARS,
            cartog_registry::DESCRIPTION_MAX_CHARS
        );
    }

    fn non_empty_db() -> Database {
        let db = Database::open_memory().unwrap();
        let sym = cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "a.py",
            1,
            10,
            0,
            100,
            None,
        );
        db.insert_symbols(std::slice::from_ref(&sym)).unwrap();
        db
    }

    fn live_serve_lock(state_dir: &Path, db_path: &Path) -> cartog_process_lock::ProcessLock {
        cartog_process_lock::ProcessLock::acquire(
            state_dir,
            &crate::state::slot_for_db("serve", db_path),
        )
        .expect("acquire test serve lock")
    }

    #[test]
    fn lsp_defer_peer_defers_with_live_serve_peer() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let db_dir = tempfile::TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let db = non_empty_db();
        let _lock = live_serve_lock(state_dir.path(), &db_path);

        let deferred = lsp_defer_peer(
            &db,
            true,
            false,
            Some(state_dir.path().to_path_buf()),
            &db_path,
        );
        // Defers exactly when the lsp feature is compiled in.
        assert_eq!(deferred.is_some(), cfg!(feature = "lsp"));
    }

    #[test]
    fn lsp_defer_peer_force_runs_lsp_locally() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let db_dir = tempfile::TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let db = non_empty_db();
        let _lock = live_serve_lock(state_dir.path(), &db_path);

        let deferred = lsp_defer_peer(
            &db,
            true,
            true,
            Some(state_dir.path().to_path_buf()),
            &db_path,
        );
        assert!(deferred.is_none(), "--force must run LSP locally");
    }

    #[test]
    fn lsp_defer_peer_respects_no_lsp() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let db_dir = tempfile::TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let db = non_empty_db();
        let _lock = live_serve_lock(state_dir.path(), &db_path);

        let deferred = lsp_defer_peer(
            &db,
            false,
            false,
            Some(state_dir.path().to_path_buf()),
            &db_path,
        );
        assert!(deferred.is_none(), "--no-lsp leaves nothing to defer");
    }

    #[test]
    fn lsp_defer_peer_never_defers_on_empty_index() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let db_dir = tempfile::TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let db = Database::open_memory().unwrap();
        let _lock = live_serve_lock(state_dir.path(), &db_path);

        let deferred = lsp_defer_peer(
            &db,
            true,
            false,
            Some(state_dir.path().to_path_buf()),
            &db_path,
        );
        assert!(
            deferred.is_none(),
            "a first-ever index must pay the LSP cold start, not seal the whole repo"
        );
    }

    #[test]
    fn lsp_defer_peer_no_state_dir_never_defers() {
        let db_dir = tempfile::TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let db = non_empty_db();

        let deferred = lsp_defer_peer(&db, true, false, None, &db_path);
        assert!(deferred.is_none());
    }

    #[test]
    fn index_result_json_omits_lsp_deferred_when_false() {
        // Keeps --json byte-identical for runs that don't defer.
        let json = serde_json::to_string(&indexer::IndexResult::default()).unwrap();
        assert!(!json.contains("lsp_deferred_to_peer"), "got: {json}");

        let deferred = indexer::IndexResult {
            lsp_deferred_to_peer: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&deferred).unwrap();
        assert!(
            json.contains("\"lsp_deferred_to_peer\":true"),
            "got: {json}"
        );
    }
}
