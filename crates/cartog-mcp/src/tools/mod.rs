//! MCP tool implementations, grouped by concern. Each submodule adds a
//! `#[tool_router(router = ...)]` impl block; lib.rs combines them.

mod graph;
mod index;
mod manage;
pub(crate) mod projects;
mod rag;
mod search;
