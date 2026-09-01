//! Project-scoped PID-lock slot derivation and live-peer detection.
//!
//! A *slot* is the stable, filesystem-safe name a long-lived command
//! (`cartog serve`, `cartog watch`) uses for its PID file inside the state
//! directory, and — since the registry keys rows on the serve slot — the
//! identity of a project in `projects.sqlite`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Derive a stable, project-scoped slot name for a long-lived command.
///
/// `prefix` is the command family (`"serve"`, `"watch"`); the returned
/// slot looks like `serve-<16 hex chars>`. The hash covers the most
/// fully-resolved form of `db_path` we can obtain at call time:
///
/// 1. `db_path.canonicalize()` if the file already exists — resolves
///    every symlink including the leaf, so two peers reaching the same
///    physical DB via different symlink paths agree on the slot.
/// 2. Otherwise, walk up ancestors and canonicalize the closest one
///    that exists, then re-append the missing suffix lexically. The DB
///    parent directory is created by the indexer well before any
///    long-lived command runs against it, so step 2 normally
///    canonicalizes the parent and re-appends just the filename.
/// 3. Only when no ancestor exists at all (the entire path is missing)
///    do we hash the raw `db_path` verbatim. That branch is best-effort
///    and may produce different slots for logically-equivalent paths,
///    but it requires an unusual setup (running cartog against a path
///    whose project root does not exist).
///
/// Two peers on logically-equivalent paths (relative vs absolute,
/// symlinked components, symlinked leaf, macOS `/tmp` → `/private/tmp`,
/// Windows verbatim `\\?\` prefix) collide on the same slot under steps
/// 1 and 2, which is the correct outcome.
pub fn slot_for_db(prefix: &str, db_path: &Path) -> String {
    use sha2::{Digest, Sha256};

    let normalized = resolve_db_path_for_slot(db_path);

    let mut hasher = Sha256::new();
    // Hash the OS-native bytes so two paths that differ only in non-UTF-8
    // byte sequences don't collide via lossy U+FFFD replacement.
    hasher.update(normalized.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    // 8 bytes = 16 hex chars: enough collision resistance for a per-user
    // state dir (~2^64 entropy) while keeping filenames short.
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{hex}")
}

/// Pick the most fully-resolved form of `db_path` for hashing. See
/// [`slot_for_db`] for the three-step strategy.
fn resolve_db_path_for_slot(db_path: &Path) -> PathBuf {
    // Step 1: full canonicalize succeeds when the DB file (including any
    // symlinks along the path) exists. Resolves a symlinked leaf too.
    if let Ok(canon) = db_path.canonicalize() {
        return canon;
    }
    // Step 2: walk up ancestors until one canonicalizes, then re-append
    // the missing suffix. ancestors() yields self first, then each parent,
    // ending with "/" (or "" for a relative path with no separators).
    let mut suffix: Vec<&OsStr> = Vec::new();
    for ancestor in db_path.ancestors() {
        // Empty ("") or current-dir (".") ancestor: a bare relative path
        // (e.g. `db.sqlite`, `subdir/db.sqlite`) exhausts its ancestors
        // before finding one that canonicalizes. Anchor on the canonical
        // CWD so two equivalent forms (`db.sqlite` and `./db.sqlite` from
        // the same cwd) produce the same slot. Note: this couples the
        // slot to the cwd, which is the correct behaviour — the same
        // bare relative path from a different cwd refers to a different
        // physical DB.
        let is_implicit_cwd =
            ancestor.as_os_str().is_empty() || ancestor.as_os_str() == std::ffi::OsStr::new(".");
        if is_implicit_cwd {
            if let Ok(cwd) = std::env::current_dir() {
                // Best-effort canonicalize so symlinked cwds normalize.
                let base = cwd.canonicalize().unwrap_or(cwd);
                let mut result = base;
                for component in suffix.iter().rev() {
                    result.push(component);
                }
                return result;
            }
            // No cwd available (extremely unusual): fall through to step 3.
            break;
        }
        if let Ok(canon_ancestor) = ancestor.canonicalize() {
            let mut result = canon_ancestor;
            // suffix was pushed parent-first as we walked up, so reverse
            // to get child-first ordering.
            for component in suffix.iter().rev() {
                result.push(component);
            }
            return result;
        }
        // This ancestor didn't exist. Remember its file_name so we can
        // re-attach it to the eventual canonical root.
        if let Some(name) = ancestor.file_name() {
            suffix.push(name);
        } else {
            // Reached "/" without an existing ancestor: bail.
            break;
        }
    }
    // Step 3: no ancestor canonicalized (path entirely missing). Walk
    // components() to drop CurDir (`.`) and collapse redundant separators
    // before hashing, so logically-equivalent inputs like `/x/y/db` and
    // `/x/./y/db` still produce the same slot. The branch is still
    // best-effort — components() does NOT resolve `..` or symlinks — but
    // the most common drift (a stray `./`) is eliminated.
    let mut normalized = PathBuf::new();
    for component in db_path.components() {
        match component {
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Find a live `cartog serve` peer holding this DB's serve lock.
///
/// Matches ONLY the DB-scoped serve slot, never the watch slot: a standalone
/// `cartog watch` runs `lsp=false` and can never resolve LSP edges, so
/// deferring to it would defer to nobody (`serve --watch` holds the serve
/// slot anyway). `find_active_locks` already verifies liveness and PID reuse.
pub fn detect_live_serve_peer(
    state_dir: &Path,
    db_path: &Path,
) -> Option<cartog_process_lock::ActiveLock> {
    let serve_slot = slot_for_db("serve", db_path);
    cartog_process_lock::find_active_locks(state_dir)
        .into_iter()
        .find(|lock| lock.slot == serve_slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn slot_for_db_is_deterministic() {
        let p = Path::new("/tmp/some-cartog.db");
        let a = slot_for_db("serve", p);
        let b = slot_for_db("serve", p);
        assert_eq!(a, b);
    }

    #[test]
    fn slot_for_db_differs_per_prefix() {
        let p = Path::new("/tmp/cartog.db");
        assert_ne!(slot_for_db("serve", p), slot_for_db("watch", p));
    }

    #[test]
    fn slot_for_db_differs_per_path() {
        assert_ne!(
            slot_for_db("serve", Path::new("/a/cartog.db")),
            slot_for_db("serve", Path::new("/b/cartog.db"))
        );
    }

    #[test]
    fn slot_for_db_is_filesystem_safe() {
        let slot = slot_for_db("serve", Path::new("/path/with spaces & specials/cartog.db"));
        assert!(slot.starts_with("serve-"));
        // 16 hex chars after the prefix-dash.
        let hex = &slot["serve-".len()..];
        assert_eq!(hex.len(), 16);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn slot_for_db_stable_across_db_creation() {
        // Regression: canonicalize() on the WHOLE db_path used to flip
        // between Err (file missing) and Ok (file present), changing the
        // hash input. We now canonicalize only the parent, which exists
        // both before and after the DB file is created. Two peers
        // spanning the DB-creation moment must compute the same slot.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("cartog.db");
        let before = slot_for_db("serve", &db_path);
        std::fs::write(&db_path, b"").unwrap();
        let after = slot_for_db("serve", &db_path);
        assert_eq!(
            before, after,
            "slot must be stable across DB creation (parent canonicalize path)"
        );
    }

    #[test]
    fn slot_for_db_normalizes_symlinked_parents() {
        // Two equivalent paths (one going through a symlinked parent,
        // one through the canonical parent) must hash to the same slot.
        // This is the macOS /tmp → /private/tmp scenario in microcosm.
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("data");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        {
            // Skip on non-Unix: symlink creation needs admin on Windows.
            let _ = link;
            return;
        }

        let via_real = slot_for_db("serve", &real.join("cartog.db"));
        let via_link = slot_for_db("serve", &link.join("cartog.db"));
        assert_eq!(
            via_real, via_link,
            "logically-equivalent paths must share the slot"
        );
    }

    #[test]
    #[serial]
    fn slot_for_db_normalizes_relative_when_parent_exists() {
        // A relative path whose parent canonicalizes from cwd should
        // hash the same as the equivalent absolute path. Verifies the
        // canonicalize-parent strategy normalizes "." and trailing
        // separators.
        //
        // `#[serial]`: this test mutates the process-global cwd via
        // `set_current_dir`. Every cwd-mutating test in this crate must carry
        // #[serial] so they share one slot — serial_test's lock is
        // process-local, so it serializes only within this test binary.
        let dir = tempfile::TempDir::new().unwrap();
        let abs = dir.path().join("cartog.db");
        let prev = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).unwrap();
        let via_rel = slot_for_db("serve", Path::new("./cartog.db"));
        let via_abs = slot_for_db("serve", &abs);
        if let Some(p) = prev {
            let _ = std::env::set_current_dir(p);
        }
        assert_eq!(
            via_rel, via_abs,
            "relative and absolute paths to the same DB must share the slot"
        );
    }

    #[test]
    fn slot_for_db_resolves_symlinked_db_leaf() {
        // Regression: pre-fix slot_for_db only canonicalized the parent
        // directory and rejoined the raw leaf name. A DB file that's a
        // symlink to a different physical path produced a different slot
        // than the target, so two cartog peers reaching the same DB via
        // the symlink vs the real path both became primary on the same
        // file. The full-path canonicalize-when-leaf-exists strategy
        // resolves this.
        let dir = tempfile::TempDir::new().unwrap();
        let real_dir = dir.path().join("storage");
        std::fs::create_dir(&real_dir).unwrap();
        let real_db = real_dir.join("real.db");
        std::fs::write(&real_db, b"").unwrap();
        let link_dir = dir.path().join("proj");
        std::fs::create_dir(&link_dir).unwrap();
        let link_db = link_dir.join("db.sqlite");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_db, &link_db).unwrap();
        #[cfg(not(unix))]
        {
            let _ = link_db;
            return;
        }

        let via_real = slot_for_db("serve", &real_db);
        let via_link = slot_for_db("serve", &link_db);
        assert_eq!(
            via_real, via_link,
            "symlinked DB leaf must produce the same slot as the real target"
        );
    }

    #[test]
    #[serial]
    fn slot_for_db_bare_relative_anchors_on_cwd() {
        // Regression: a bare relative path like `db.sqlite` exhausted
        // its ancestors (`db.sqlite`, ``) before finding one that
        // canonicalized, falling through to the raw-path branch and
        // hashing just "db.sqlite". A peer running from a DIFFERENT cwd
        // with the same `--db db.sqlite` arg would compute the SAME
        // hash for a DIFFERENT physical file. Anchoring on the
        // canonical cwd fixes this — and also makes `db.sqlite` and
        // `./db.sqlite` from the same cwd produce the same slot.
        let dir = tempfile::TempDir::new().unwrap();
        let prev = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).unwrap();
        let bare = slot_for_db("serve", Path::new("db.sqlite"));
        let dotted = slot_for_db("serve", Path::new("./db.sqlite"));
        let absolute = slot_for_db("serve", &dir.path().join("db.sqlite"));
        if let Some(p) = prev {
            let _ = std::env::set_current_dir(p);
        }
        assert_eq!(
            bare, dotted,
            "bare relative and './' relative must produce the same slot"
        );
        assert_eq!(
            bare, absolute,
            "bare relative must anchor on cwd and match the absolute equivalent"
        );
    }

    #[test]
    fn slot_for_db_walks_up_when_parent_missing() {
        // Regression: when neither the DB file nor its immediate parent
        // exists yet, the old code hashed the raw path verbatim — so
        // equivalent forms (with/without trailing slash, relative vs
        // absolute) produced different slots, breaking single-writer.
        // The new code walks up ancestors until one canonicalizes and
        // appends the missing suffix lexically.
        let dir = tempfile::TempDir::new().unwrap();
        // dir.path() exists; .cartog/db.sqlite does not (no parent created).
        let a = dir.path().join(".cartog").join("db.sqlite");
        // Equivalent form via a redundant "./" (parent walked the same way).
        let b = dir.path().join(".").join(".cartog").join("db.sqlite");

        let slot_a = slot_for_db("serve", &a);
        let slot_b = slot_for_db("serve", &b);
        assert_eq!(
            slot_a, slot_b,
            "equivalent paths with missing parent must produce the same slot"
        );
    }

    #[test]
    fn slot_for_db_step3_normalizes_curdir_when_no_ancestor_exists() {
        // When NO ancestor exists at all (every step-2 canonicalize fails),
        // step 3 used to hash the raw path verbatim — so `/missing/x/db`
        // and `/missing/./x/db` produced different slots and two peers
        // racing to create the same logical DB could each win their own
        // O_EXCL election. Components-level normalization fixes the common
        // `./` drift without resolving symlinks or `..`.
        let bare = slot_for_db(
            "serve",
            Path::new("/nonexistent-cartog-root/proj/db.sqlite"),
        );
        let dotted = slot_for_db(
            "serve",
            Path::new("/nonexistent-cartog-root/./proj/db.sqlite"),
        );
        assert_eq!(
            bare, dotted,
            "step-3 fallback must normalize CurDir to keep equivalent missing paths consistent"
        );
    }

    #[test]
    fn slot_for_db_stable_across_parent_creation() {
        // Stronger form of slot_for_db_stable_across_db_creation: the
        // PARENT directory is also missing initially. Walking ancestors
        // should anchor on dir.path() both before and after the parent
        // is created, yielding the same slot.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(".cartog").join("db.sqlite");
        let before = slot_for_db("serve", &db_path);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, b"").unwrap();
        let after = slot_for_db("serve", &db_path);
        assert_eq!(
            before, after,
            "slot must be stable across parent-dir + DB-file creation"
        );
    }

    // ── detect_live_serve_peer ──

    /// Hold a live lock for `slot` (this process's own PID) in `dir`.
    fn hold_lock(dir: &Path, slot: &str) -> cartog_process_lock::ProcessLock {
        cartog_process_lock::ProcessLock::acquire(dir, slot).expect("acquire test lock")
    }

    #[test]
    fn detect_live_serve_peer_finds_live_serve_lock() {
        let state_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let _lock = hold_lock(state_dir.path(), &slot_for_db("serve", &db_path));

        let peer = detect_live_serve_peer(state_dir.path(), &db_path)
            .expect("live serve lock must be detected");
        assert_eq!(peer.pid, std::process::id());
        assert_eq!(peer.slot, slot_for_db("serve", &db_path));
    }

    #[test]
    fn detect_live_serve_peer_ignores_dead_pid() {
        let state_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let slot = slot_for_db("serve", &db_path);
        // 4_194_304 is Linux's pid_max — guaranteed dead on every platform.
        std::fs::write(state_dir.path().join(format!("{slot}.pid")), "4194304\n").unwrap();

        assert!(detect_live_serve_peer(state_dir.path(), &db_path).is_none());
    }

    #[test]
    fn detect_live_serve_peer_ignores_other_db_slot() {
        let state_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let ours = db_dir.path().join("cartog.db");
        let theirs = db_dir.path().join("other.db");
        let _lock = hold_lock(state_dir.path(), &slot_for_db("serve", &theirs));

        assert!(detect_live_serve_peer(state_dir.path(), &ours).is_none());
    }

    #[test]
    fn detect_live_serve_peer_ignores_watch_slot() {
        // Serve-only by design: a standalone watcher never resolves LSP edges.
        let state_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let _lock = hold_lock(state_dir.path(), &slot_for_db("watch", &db_path));

        assert!(detect_live_serve_peer(state_dir.path(), &db_path).is_none());
    }

    #[test]
    fn detect_live_serve_peer_missing_state_dir_returns_none() {
        let db_dir = TempDir::new().unwrap();
        let db_path = db_dir.path().join("cartog.db");
        let missing = db_dir.path().join("no-such-state-dir");

        assert!(detect_live_serve_peer(&missing, &db_path).is_none());
    }
}
