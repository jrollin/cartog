//! User-global state for cartog: everything kept per *user* rather than per
//! project.
//!
//! The graph crates (`cartog-db`, `cartog-indexer`, …) are all per-project.
//! This crate is the one place that knows about the machine as a whole:
//!
//! - [`default_state_dir`] / [`default_state_file`] — the XDG-compliant
//!   per-user directory hosting `state.toml` and the PID lock files.
//! - [`slot_for_db`] — reduces a database path to the stable,
//!   filesystem-safe slot name `cartog-process-lock` writes as `<slot>.pid`.
//! - [`detect_live_serve_peer`] — finds a running `cartog serve` holding a
//!   given database's serve lock.
//!
//! These began in the `cartog` binary crate, where `cartog-mcp` and
//! `cartog-watch` — the crates that derive the slots they lock on — could not
//! reach them. Extracting them here makes the reference those crates already
//! documented an actual, callable one.
//!
//! A machine-local registry of indexed projects (`projects.sqlite`) is
//! proposed on top of this crate in `docs/explanation/project-registry.md`;
//! it is not implemented yet.
#![doc = ""]
#![doc = include_str!("../README.md")]

mod slot;
mod state_dir;

pub use slot::{detect_live_serve_peer, slot_for_db};
pub use state_dir::{default_state_dir, default_state_file};
