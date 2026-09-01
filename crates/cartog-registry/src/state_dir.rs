//! Resolution of the user-global cartog state directory and the files inside it.

use directories::ProjectDirs;
use std::path::{Path, PathBuf};

const STATE_FILE_NAME: &str = "state.toml";

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
    Some(default_state_dir()?.join(STATE_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_resolves_or_returns_none_gracefully() {
        // Must not panic in any environment; either resolves or is None.
        let _ = default_state_file();
        let _ = default_state_dir();
    }

    #[test]
    fn default_state_file_lives_inside_default_state_dir() {
        if let (Some(dir), Some(file)) = (default_state_dir(), default_state_file()) {
            assert_eq!(file.parent(), Some(dir.as_path()));
            assert_eq!(file.file_name().unwrap(), STATE_FILE_NAME);
        }
    }
}
