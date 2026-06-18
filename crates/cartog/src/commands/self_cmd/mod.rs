//! Implementations for the `cartog self` subcommand group.
//!
//! Pure logic is factored into helpers (`resolve_install_source`,
//! `VersionInfo`) that take their inputs as arguments so integration tests
//! can drive them without touching the real environment, filesystem, or
//! network. The thin `cmd_self_*` wrappers gather the real-world inputs and
//! delegate to the pure helpers.

mod download;
mod exit;
mod migrate;
mod update;
mod version;

// Crate-internal globs so each submodule's `use super::*` sees its siblings'
// items (cross-concern calls: run_check -> fetch_latest_version, run_upgrade ->
// perform_upgrade, etc.).
pub(crate) use download::*;
pub(crate) use update::*;
pub(crate) use version::*;

// The public `cartog self` surface, re-exported for commands/mod.rs.
pub use migrate::cmd_self_migrate_db;
pub use update::{cmd_self_rollback, cmd_self_update, UpdateMode};
pub use version::cmd_self_version;

// migrate's planning + peer-guard internals are referenced only by its tests.
#[cfg(test)]
pub(crate) use migrate::{
    migrate_peer_guard, plan_migration, target_db_slots, TEST_SKIP_PEER_LOCK_ENV,
};

#[cfg(test)]
mod tests;
