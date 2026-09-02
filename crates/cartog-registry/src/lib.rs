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
//! - [`record_project`] / [`list_projects`] / [`forget_project_at`] /
//!   [`prune_projects_at`] — the machine-local project registry
//!   (`projects.sqlite`): one row per indexed project, so a session in one
//!   repository can discover the *other* indexed projects on the machine —
//!   their database paths, languages and sizes — without merging their code
//!   graphs. It records where each index lives and a summary of what it
//!   holds, never any code. [`REGISTRY_ENV`] relocates it, or disables it
//!   entirely when set to an empty value.
//! - [`readme_description`] — the first prose paragraph of a project's
//!   `README.md`, the fallback source for a registry row's description when
//!   `.cartog.toml` declares no `[project] description`.
//!
//! The state-directory and slot helpers began in the `cartog` binary crate,
//! where `cartog-mcp` and `cartog-watch` — the crates that derive the slots
//! they lock on — could not reach them. Extracting them here makes the
//! reference those crates already documented an actual, callable one.
//!
//! The registry deliberately does **not** depend on `cartog-db`. The few
//! values it needs from the graph schema (its version, the embedding
//! fingerprint) are read by callers that already depend on that crate and
//! passed in as plain primitives, so a graph-schema bump never forces a
//! registry migration and a registry bump never forces a re-index.
#![doc = ""]
#![doc = include_str!("../README.md")]

mod corrupt;
mod describe;
mod fingerprint;
mod maintain;
mod model;
mod open;
mod path;
mod read;
mod schema;
mod slot;
mod state_dir;
mod write;

pub use describe::{readme_description, DESCRIPTION_MAX_CHARS};
pub use maintain::{forget_project_at, prune_projects_at, Removed};
pub use model::{
    format_timestamp, infer_root_from_db_path, Declared, DeclaredUpdate, Description,
    DescriptionSource, Listing, Markers, ProjectFacts, ProjectRow,
};
pub use path::{registry_path, REGISTRY_ENV};
pub use read::{list_projects, list_projects_at};
pub use slot::{detect_live_serve_peer, slot_for_db};
pub use state_dir::{default_state_dir, default_state_file};
pub use write::record_project;
