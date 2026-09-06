//! Persistent CLI state — last update check, last known latest version, etc.
//!
//! State lives in `state.toml` inside the user-global state directory resolved
//! by [`cartog_registry::default_state_dir`]:
//!
//! - Linux:   `$XDG_STATE_HOME/cartog/state.toml` (typically `~/.local/state/cartog/`)
//! - macOS:   `~/Library/Application Support/cartog/state.toml`
//! - Windows: `%LOCALAPPDATA%\cartog\state.toml`
//!
//! The schema is intentionally tiny and forward-compatible: unknown TOML keys
//! are silently ignored, and a missing file deserialises to `State::default()`.
//! Writes are atomic (write-temp + rename) so concurrent invocations cannot
//! observe a torn file.
//!
//! The state *directory* and the PID-lock slot helpers now live in
//! `cartog-registry`, which owns everything user-global; they are re-exported
//! here so existing `crate::state::*` call sites keep resolving.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub use cartog_registry::{
    default_state_dir, default_state_file, detect_live_serve_peer, slot_for_db,
};

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
}
