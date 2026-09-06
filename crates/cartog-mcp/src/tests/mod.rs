//! Unit tests for the cartog-mcp server, split by concern.

use crate::*;

mod degraded;
mod federated;
mod handlers;
mod lsp_gate;
mod read_tools;
mod schema;
mod search_all;
mod single_writer;
mod update;
mod validation;

/// RAII override of `CARTOG_REGISTRY`, restoring the previous value on drop.
///
/// Mandatory for **any** test in this crate that can reach a registry read or
/// write, even one expected not to: the registry is **user-global**, so a test
/// that sets the var without restoring it — or that runs in parallel with one
/// that does — reads and writes the developer's own registry. `#[serial]` on
/// each such test closes the parallel half; this guard closes the leak half,
/// including on panic. Pair them: this crate has two independent serialization
/// mechanisms, so the guard alone does not stop a concurrent test from
/// observing the override.
pub(super) struct RegistryEnv(Option<std::ffi::OsString>);

impl RegistryEnv {
    pub(super) fn set(value: &std::ffi::OsStr) -> Self {
        let prev = std::env::var_os(cartog_registry::REGISTRY_ENV);
        std::env::set_var(cartog_registry::REGISTRY_ENV, value);
        Self(prev)
    }

    /// Point at a fresh registry inside `dir`.
    pub(super) fn isolated(dir: &std::path::Path) -> Self {
        Self::set(dir.join("projects.sqlite").as_os_str())
    }
}

impl Drop for RegistryEnv {
    fn drop(&mut self) {
        match self.0.take() {
            Some(v) => std::env::set_var(cartog_registry::REGISTRY_ENV, v),
            None => std::env::remove_var(cartog_registry::REGISTRY_ENV),
        }
    }
}

/// Build a `StaleSnapshot` for banner tests. Shared by `schema` (budget +
/// banner interaction) and `handlers` (banner gating).
pub(super) fn snap(rag_pending: u32, change_seq: u64, reindexed_seq: u64) -> StaleSnapshot {
    StaleSnapshot {
        rag_pending,
        change_seq,
        reindexed_seq,
    }
}

/// Mock embedding provider used wherever a `CartogServer` must be constructed
/// without loading a real model (single_writer, lsp_gate, read_tools).
pub(super) fn test_provider() -> Box<dyn rag::provider::EmbeddingProvider> {
    Box::new(rag::provider::test_utils::MockEmbeddingProvider::new(
        rag::EMBEDDING_DIM,
    ))
}
