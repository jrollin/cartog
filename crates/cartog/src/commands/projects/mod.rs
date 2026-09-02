//! Implementations for the `cartog projects` subcommand group.
//!
//! `cartog projects` is a **read** command over the machine-local registry, not
//! over any project's index: it never opens a project database, so it works
//! from any directory — including one that was never indexed, which is the
//! point of a machine-global listing.

mod list;
mod maintain;

pub use list::cmd_projects_list;
pub use maintain::{cmd_projects_forget, cmd_projects_prune};

#[cfg(test)]
mod tests;
