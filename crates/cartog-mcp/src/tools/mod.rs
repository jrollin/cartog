//! MCP tool implementations, grouped by concern. Each submodule adds a
//! `#[tool_router(router = ...)]` impl block; lib.rs combines them.

mod graph;
mod index;
mod manage;
mod rag;

// Private like its siblings in a normal build. Widened only under `cfg(test)`,
// where `tests/search_all.rs` drives the pure candidate-selection helper
// directly — that logic is duplicated from the binary crate (no crate can host
// a shared copy: `cartog-registry` has no `cartog-db` dependency by design),
// so it needs its own tests on this side to catch drift between the two.
#[cfg(not(test))]
mod search;
#[cfg(test)]
pub(crate) mod search;

// Private like its siblings in a normal build. Widened only under `cfg(test)`,
// where `tests/degraded.rs` drives this module's two pure helpers directly
// rather than through the process-global `CARTOG_REGISTRY` — this crate has two
// independent test-serialization mechanisms, so an env-mutating test cannot be
// reliably isolated from the other set.
#[cfg(not(test))]
mod projects;
#[cfg(test)]
pub(crate) mod projects;
