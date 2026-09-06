//! Resolution of the registry file's path, and the kill switch that disables it.

use std::path::PathBuf;

use crate::state_dir::default_state_dir;

/// Environment variable overriding the registry's location.
///
/// Set to an **absolute** path to relocate the registry. Set to an **empty**
/// value to disable it entirely — both reads and writes become no-ops, which is
/// the user's opt-out from a machine-global file.
pub const REGISTRY_ENV: &str = "CARTOG_REGISTRY";

const REGISTRY_FILE_NAME: &str = "projects.sqlite";

/// Resolve the registry path, or `None` for "there is no registry".
///
/// `None` has three causes that every caller must treat identically — as a
/// working but empty registry, never as an error:
///
/// - [`REGISTRY_ENV`] is set to an empty value (explicit opt-out).
/// - [`REGISTRY_ENV`] is set to a *relative* path (rejected, see below).
/// - No state directory could be resolved (a sandbox with no `$HOME`).
///
/// Collapsing them into one `None` is deliberate: no hook needs a separate "is
/// the registry disabled" branch, so a disabled registry cannot take a code
/// path an enabled one doesn't.
///
/// A relative override is refused rather than resolved against the current
/// directory. `CARTOG_REGISTRY=projects.sqlite` exported in a shell profile
/// would otherwise give every directory its own registry, each seeing only
/// itself — silently inverting the one property the feature exists for.
pub fn registry_path() -> Option<PathBuf> {
    match std::env::var_os(REGISTRY_ENV) {
        // Set-but-empty is the kill switch, not a relative path.
        Some(v) if v.is_empty() => None,
        Some(v) => {
            let path = PathBuf::from(v);
            if path.is_absolute() {
                return Some(path);
            }
            warn_relative_override_once(&path);
            None
        }
        None => Some(default_state_dir()?.join(REGISTRY_FILE_NAME)),
    }
}

/// Warn once that a relative `CARTOG_REGISTRY` disabled the registry.
///
/// Once per process: the value is read on every registry access, and a
/// misconfigured profile would otherwise log on each one.
fn warn_relative_override_once(path: &std::path::Path) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::warn!(
        value = %path.display(),
        "{REGISTRY_ENV} must be an absolute path; a relative one would give every \
         directory its own registry. The project registry is disabled."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Guard that restores `CARTOG_REGISTRY` on drop, so a panicking test
    /// cannot leak the override into the rest of the binary.
    struct EnvGuard(Option<std::ffi::OsString>);

    impl EnvGuard {
        fn set(value: Option<&str>) -> Self {
            let prev = std::env::var_os(REGISTRY_ENV);
            match value {
                Some(v) => std::env::set_var(REGISTRY_ENV, v),
                None => std::env::remove_var(REGISTRY_ENV),
            }
            Self(prev)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var(REGISTRY_ENV, v),
                None => std::env::remove_var(REGISTRY_ENV),
            }
        }
    }

    #[test]
    #[serial]
    fn an_empty_env_value_disables_the_registry() {
        let _g = EnvGuard::set(Some(""));
        assert_eq!(registry_path(), None);
    }

    #[test]
    #[serial]
    fn an_env_value_overrides_the_path_verbatim() {
        let _g = EnvGuard::set(Some("/custom/reg.sqlite"));
        assert_eq!(registry_path(), Some(PathBuf::from("/custom/reg.sqlite")));
    }

    #[test]
    #[serial]
    fn a_relative_env_value_disables_the_registry_rather_than_going_per_cwd() {
        // A relative override resolved against the cwd would give every
        // directory its own registry, each seeing only itself — inverting the
        // one property a machine-global registry exists to provide.
        let _g = EnvGuard::set(Some("projects.sqlite"));
        assert_eq!(registry_path(), None);
    }

    #[test]
    #[serial]
    fn a_relative_env_value_with_a_separator_is_also_refused() {
        let _g = EnvGuard::set(Some("./sub/projects.sqlite"));
        assert_eq!(registry_path(), None);
    }

    #[test]
    #[serial]
    fn the_default_path_sits_in_the_state_dir() {
        let _g = EnvGuard::set(None);
        // Only assert the relationship: the state dir itself is
        // platform-dependent and may be absent in a sandbox.
        match (default_state_dir(), registry_path()) {
            (Some(dir), Some(reg)) => {
                assert_eq!(reg, dir.join(REGISTRY_FILE_NAME));
            }
            (None, reg) => assert_eq!(reg, None, "no state dir must mean no registry"),
            (Some(_), None) => panic!("a resolvable state dir must yield a registry path"),
        }
    }
}
