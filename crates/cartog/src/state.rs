//! Persistent CLI state — last update check, last known latest version, etc.
//!
//! State lives in an XDG-compliant per-platform directory resolved via the
//! `directories` crate:
//!
//! - Linux:   `$XDG_STATE_HOME/cartog/state.toml` (typically `~/.local/state/cartog/`)
//! - macOS:   `~/Library/Application Support/cartog/state.toml`
//! - Windows: `%LOCALAPPDATA%\cartog\state.toml`
//!
//! The schema is intentionally tiny and forward-compatible: unknown TOML keys
//! are silently ignored, and a missing file deserialises to `State::default()`.
//! Writes are atomic (write-temp + rename) so concurrent invocations cannot
//! observe a torn file.

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "state.toml";

/// Persisted CLI state. All fields are optional — an empty file is valid and
/// deserialises to `State::default()`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// RFC3339 timestamp of the last successful update check. `None` if no
    /// check has ever run on this machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update_check: Option<String>,

    /// Latest stable version observed by the most recent check (e.g. `"0.14.0"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_known_latest: Option<String>,

    /// Whether the current binary was outdated at the last check.
    #[serde(default, skip_serializing_if = "is_false")]
    pub last_known_outdated: bool,

    /// Mirror of `CARTOG_NO_UPDATE_CHECK` at the moment of the last write.
    /// Lets the next invocation honor a kill-switch without re-reading env on
    /// the hot path.
    #[serde(default, skip_serializing_if = "is_false")]
    pub update_check_disabled: bool,

    /// A deferred binary upgrade armed in-session, applied at the next safe
    /// boundary. `None` means nothing is armed.
    ///
    /// Written by `cartog self update --defer` (which skips the running-peer
    /// check so it can arm while a `cartog serve`/`watch` holds the lock),
    /// consumed and cleared by `--apply-pending` once no peer is live.
    ///
    /// Concurrency: the background update probe (`auto_check::run_check_once`)
    /// does load → mutate three fields → save, so it preserves any
    /// `pending_update` it read. The arm path runs synchronously in the
    /// foreground while the probe is a 24h-gated background thread, so the
    /// interleave window is the same negligible one the other fields already
    /// accept; `save_to`'s per-PID temp file prevents torn writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_update: Option<PendingUpdate>,
}

/// Intent to swap the binary to `target_version` once no peer lock is held.
/// Written by the arm path, consumed and cleared by the boundary-apply path.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpdate {
    /// Bare `MAJOR.MINOR.PATCH` the boundary swap should install.
    pub target_version: String,
    /// Version of the binary that armed this, for stale detection and logging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_from: Option<String>,
    /// RFC3339 timestamp the intent was armed (debugging / staleness logging).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_at: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Resolve the platform-specific state directory. Hosts both
/// `state.toml` and the PID lock files written by long-lived commands.
///
/// Returns `None` if no home/state directory could be resolved (e.g. a
/// sandboxed environment with neither `$HOME` nor `%USERPROFILE%`).
pub fn default_state_dir() -> Option<PathBuf> {
    let proj = ProjectDirs::from("io", "cartog", "cartog")?;
    // state_dir is Linux-only; macOS/Windows fall back to data_local_dir.
    Some(
        proj.state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| proj.data_local_dir().to_path_buf()),
    )
}

/// Resolve the platform-specific state file path (`state.toml` inside
/// [`default_state_dir`]).
pub fn default_state_file() -> Option<PathBuf> {
    Some(default_state_dir()?.join(FILE_NAME))
}

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

impl State {
    /// Load state from `path`. A missing file or malformed TOML yields
    /// `State::default()` — this is a best-effort cache, not an authoritative
    /// store.
    pub fn load_from(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                // tracing, not eprintln: avoid every-command stderr noise.
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read cartog state file; using defaults"
                );
                return Self::default();
            }
        };
        match toml::from_str::<State>(&text) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "cartog state file is malformed; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Atomically persist state to `path`. The parent directory is created if
    /// missing. The write goes to a sibling temp file first, then `rename`s
    /// onto the target — readers never observe a partial write.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string(self).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to serialise state: {e}"),
            )
        })?;
        // Sibling tmp keeps the rename within one filesystem (no EXDEV).
        // Per-PID disambiguation: two cartog processes saving concurrently
        // (e.g. an auto-check thread and a `self update`) must not race on
        // the same tmp filename — the loser's rename would clobber the
        // winner's data.
        let tmp = match path.file_name() {
            Some(name) => path.with_file_name(format!(
                ".{}.{}.tmp",
                name.to_string_lossy(),
                std::process::id(),
            )),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "state path has no file name",
                ));
            }
        };
        // fsync before rename: under power loss, the rename can land but
        // the file's data block may not have been flushed.
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(serialized.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn load_missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        let state = State::load_from(&path);
        assert_eq!(state, State::default());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        let original = State {
            last_update_check: Some("2026-05-06T14:32:00Z".to_string()),
            last_known_latest: Some("0.14.0".to_string()),
            last_known_outdated: true,
            update_check_disabled: false,
            pending_update: None,
        };
        original.save_to(&path).expect("save");
        let loaded = State::load_from(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn pending_update_roundtrips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        let original = State {
            pending_update: Some(PendingUpdate {
                target_version: "0.20.0".to_string(),
                armed_from: Some("0.19.0".to_string()),
                armed_at: Some("2026-05-29T10:00:00Z".to_string()),
            }),
            ..Default::default()
        };
        original.save_to(&path).expect("save");
        let loaded = State::load_from(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn default_state_omits_pending_update() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        State::default().save_to(&path).expect("save");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("pending_update"),
            "default state should not write pending_update, got: {text:?}"
        );
    }

    #[test]
    fn update_check_mutation_preserves_pending_update() {
        // Models the auto_check::run_check_once load→mutate-3-fields→save path:
        // a deferred update armed by `--defer` must survive a concurrent
        // background update-check that only touches last_*.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        State {
            pending_update: Some(PendingUpdate {
                target_version: "0.20.0".to_string(),
                armed_from: Some("0.19.0".to_string()),
                armed_at: None,
            }),
            ..Default::default()
        }
        .save_to(&path)
        .expect("seed armed state");

        // The probe's exact sequence.
        let mut state = State::load_from(&path);
        state.last_update_check = Some("2026-05-29T12:00:00Z".to_string());
        state.last_known_latest = Some("0.20.0".to_string());
        state.last_known_outdated = true;
        state.save_to(&path).expect("probe save");

        let loaded = State::load_from(&path);
        assert_eq!(
            loaded
                .pending_update
                .as_ref()
                .map(|p| p.target_version.as_str()),
            Some("0.20.0"),
            "the armed pending_update must survive the update-check save"
        );
        assert_eq!(loaded.last_known_latest.as_deref(), Some("0.20.0"));
    }

    #[test]
    fn pending_update_inner_optionals_skip_when_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        State {
            pending_update: Some(PendingUpdate {
                target_version: "0.20.0".to_string(),
                armed_from: None,
                armed_at: None,
            }),
            ..Default::default()
        }
        .save_to(&path)
        .expect("save");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("target_version"), "got: {text:?}");
        assert!(
            !text.contains("armed_from") && !text.contains("armed_at"),
            "None inner fields must be omitted, got: {text:?}"
        );
    }

    #[test]
    fn unknown_fields_alongside_pending_update_still_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        std::fs::write(
            &path,
            "future_field = \"hello\"\n\n[pending_update]\ntarget_version = \"0.21.0\"\n",
        )
        .unwrap();
        let state = State::load_from(&path);
        assert_eq!(
            state
                .pending_update
                .as_ref()
                .map(|p| p.target_version.as_str()),
            Some("0.21.0")
        );
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("subdir").join("state.toml");
        State::default().save_to(&path).expect("save");
        assert!(path.exists());
    }

    #[test]
    fn malformed_toml_returns_default_without_panicking() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        std::fs::write(&path, "{{ not toml at all").unwrap();
        let state = State::load_from(&path);
        assert_eq!(state, State::default());
    }

    #[test]
    fn binary_state_file_returns_default_without_panicking() {
        // Disk corruption or a wrong-format file (e.g. SQLite snapshot
        // mistakenly written here) must not crash cartog. read_to_string
        // returns Err on non-UTF8 bytes; we should fall back to default.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x80, 0xc3, 0x28]).unwrap();
        let state = State::load_from(&path);
        assert_eq!(state, State::default());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        // A future schema version may add fields; old binaries must keep
        // working — forward-compatibility.
        std::fs::write(
            &path,
            "last_known_latest = \"0.15.0\"\nfuture_field = \"hello\"\n",
        )
        .unwrap();
        let state = State::load_from(&path);
        assert_eq!(state.last_known_latest.as_deref(), Some("0.15.0"));
    }

    #[test]
    fn empty_file_loads_as_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        std::fs::write(&path, "").unwrap();
        let state = State::load_from(&path);
        assert_eq!(state, State::default());
    }

    #[test]
    fn save_omits_default_fields_for_compactness() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        State::default().save_to(&path).expect("save");
        let text = std::fs::read_to_string(&path).unwrap();
        // Default state should serialise to an empty document (no keys).
        // Skip-if-default keeps the file readable for humans.
        assert!(
            !text.contains("last_update_check"),
            "default state should not write last_update_check, got: {text:?}"
        );
        assert!(
            !text.contains("last_known_outdated"),
            "default state should not write last_known_outdated, got: {text:?}"
        );
    }

    #[test]
    fn save_overwrites_existing_atomically() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.toml");
        State {
            last_known_latest: Some("0.13.0".to_string()),
            ..Default::default()
        }
        .save_to(&path)
        .expect("first save");
        State {
            last_known_latest: Some("0.14.0".to_string()),
            ..Default::default()
        }
        .save_to(&path)
        .expect("second save");
        let loaded = State::load_from(&path);
        assert_eq!(loaded.last_known_latest.as_deref(), Some("0.14.0"));
    }

    #[test]
    fn default_path_resolves_or_returns_none_gracefully() {
        // `default_path` should never panic. On a normal dev workstation it
        // returns Some; in a sandbox without a home directory it returns None.
        // Either is acceptable — the test just asserts no panic.
        let _ = default_state_file();
        let _ = default_state_dir();
    }

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
        // `set_current_dir`. Other tests in this binary (config.rs,
        // commands/mod.rs doctor tests) also use #[serial] for cwd
        // mutation — they must all share the same serial slot.
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
