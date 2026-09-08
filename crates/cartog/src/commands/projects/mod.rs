//! Implementations for the `cartog projects` subcommand group.
//!
//! `cartog projects list`/`forget`/`prune` operate on the machine-local
//! registry, not on any project's index: they never open a project database,
//! so they work from any directory — including one that was never indexed,
//! which is the point of a machine-global listing.
//!
//! `add`/`scan` are the exception, and only just: they open the database of a
//! project the user named **read-only**, to record what it holds. They never
//! create or migrate one.

mod backfill;
mod list;
mod maintain;

pub use backfill::{cmd_projects_add, cmd_projects_scan};
pub use list::cmd_projects_list;
pub use maintain::{cmd_projects_forget, cmd_projects_prune};

#[cfg(test)]
mod tests;
