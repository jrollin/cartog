//! Unit tests for the cartog-mcp server, split by concern.

use crate::*;

mod degraded;
mod handlers;
mod lsp_gate;
mod read_tools;
mod schema;
mod search_all;
mod single_writer;
mod update;
mod validation;

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
