//! CLI command implementations.
//!
//! Each command group lives in its own module; this file is the re-export hub
//! so `main.rs` keeps calling `commands::cmd_*` regardless of where a command
//! moved. Cross-cutting helpers live in [`shared`] (DB open, token-budget
//! output, "no result" diagnostics) and [`progress`] (spinner + Ctrl-C probe).
//!
//! - [`index`] — `index`
//! - [`search`] — `search`
//! - [`graph`] — `outline`, `callees`, `impact`, `trace`, `refs`, `hierarchy`, `deps`
//! - [`rag`] — `rag setup`, `rag index`, `rag search`, `context`
//! - [`manage`] — `stats`, `map`, `changes`, `push`, `pull`, `watch`
//! - [`ide`], [`init`], [`remote`], [`config_display`], [`doctor`], [`self_cmd`] — already standalone

mod progress;
mod shared;

#[cfg(test)]
mod test_support;

pub use progress::SpinnerSafeWriter;

mod graph;
mod index;
mod manage;
mod rag;
mod search;

pub use graph::{
    cmd_callees, cmd_deps, cmd_hierarchy, cmd_impact, cmd_outline, cmd_refs, cmd_trace,
};
pub use index::cmd_index;
pub use manage::{cmd_changes, cmd_map, cmd_pull, cmd_push, cmd_stats, cmd_watch};
pub use rag::{cmd_context, cmd_rag_index, cmd_rag_search, cmd_rag_setup};
pub use search::cmd_search;

pub mod ide;
pub mod init;
pub mod mermaid;
pub mod remote;

mod config_display;
pub use config_display::cmd_config;

mod doctor;
pub use doctor::cmd_doctor;

mod self_cmd;
pub use self_cmd::{
    cmd_self_migrate_db, cmd_self_rollback, cmd_self_update, cmd_self_version, UpdateMode,
};
