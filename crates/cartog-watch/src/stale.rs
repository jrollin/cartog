//! Lock-light staleness state the watcher publishes for the MCP server to read.
//!
//! The watcher writes timestamps + a pending-embedding count; the MCP response
//! layer reads a [`StaleSnapshot`] to decide whether to prepend a "results may
//! be stale" banner. All fields are atomics, so reads never block and never
//! hold a lock across `.await`.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Shared staleness state, written by the watch thread and read by the MCP
/// server over an `Arc<StaleState>`.
///
/// Structural staleness uses monotonic sequence counters, not wall-clock time:
/// `change_seq` bumps on every observed change, and a reindex records the
/// `change_seq` value it caught up to. This is exact regardless of how fast
/// changes and reindexes interleave (a wall-clock second can hold both).
#[derive(Debug, Default)]
pub struct StaleState {
    /// Symbols awaiting RAG embedding after the last reindex; 0 when current.
    rag_pending: AtomicU32,
    /// Count of relevant file changes observed (monotonically increasing).
    change_seq: AtomicU64,
    /// The `change_seq` value the most recent completed reindex caught up to.
    reindexed_seq: AtomicU64,
}

impl StaleState {
    /// Construct a fresh shared state.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record that a relevant file change was observed (debounce-window start).
    pub fn note_change(&self) {
        self.change_seq.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a completed reindex: it caught the structural index up to the
    /// change count observed when it *started* (`caught_up_to`), and left
    /// `rag_pending` symbols awaiting embedding. Pass the `change_seq` read
    /// before the reindex ran so a change arriving mid-reindex stays counted.
    pub fn note_reindex(&self, caught_up_to: u64, rag_pending: u32) {
        self.reindexed_seq.store(caught_up_to, Ordering::Relaxed);
        self.rag_pending.store(rag_pending, Ordering::Relaxed);
    }

    /// Current observed-change count, to capture before starting a reindex.
    #[must_use]
    pub fn change_seq(&self) -> u64 {
        self.change_seq.load(Ordering::Relaxed)
    }

    /// Clear the pending-embedding count after embeddings catch up.
    pub fn clear_rag_pending(&self) {
        self.rag_pending.store(0, Ordering::Relaxed);
    }

    /// Read a point-in-time snapshot for banner decisions.
    #[must_use]
    pub fn snapshot(&self) -> StaleSnapshot {
        StaleSnapshot {
            rag_pending: self.rag_pending.load(Ordering::Relaxed),
            change_seq: self.change_seq.load(Ordering::Relaxed),
            reindexed_seq: self.reindexed_seq.load(Ordering::Relaxed),
        }
    }
}

/// An immutable read of [`StaleState`].
#[derive(Debug, Clone, Copy)]
pub struct StaleSnapshot {
    pub rag_pending: u32,
    pub change_seq: u64,
    pub reindexed_seq: u64,
}

impl StaleSnapshot {
    /// True when embeddings are behind the structural index (RAG staleness).
    #[must_use]
    pub fn rag_stale(&self) -> bool {
        self.rag_pending > 0
    }

    /// True when changes have been observed that the last reindex didn't cover
    /// (debounce gap or a change that arrived mid-reindex).
    #[must_use]
    pub fn structural_stale(&self) -> bool {
        self.change_seq > self.reindexed_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_then_no_reindex_is_structurally_stale() {
        let s = StaleState::new();
        s.note_change();
        assert!(s.snapshot().structural_stale());
    }

    #[test]
    fn reindex_caught_up_to_change_clears_structural_staleness() {
        let s = StaleState::new();
        s.note_change();
        let seq = s.change_seq();
        s.note_reindex(seq, 0);
        assert!(!s.snapshot().structural_stale());
    }

    #[test]
    fn change_arriving_mid_reindex_stays_stale() {
        // Reindex captures change_seq=1 at start; a 2nd change bumps to 2 before
        // it records. The reindex caught up only to 1, so seq 2 is still stale —
        // no wall-clock granularity gap can hide it.
        let s = StaleState::new();
        s.note_change(); // seq = 1
        let caught = s.change_seq();
        s.note_change(); // seq = 2 (arrived during the reindex)
        s.note_reindex(caught, 0);
        assert!(
            s.snapshot().structural_stale(),
            "the mid-reindex change is not yet covered"
        );
    }

    #[test]
    fn pending_embeddings_are_rag_stale_until_cleared() {
        let s = StaleState::new();
        s.note_reindex(0, 5);
        assert!(s.snapshot().rag_stale());
        s.clear_rag_pending();
        assert!(!s.snapshot().rag_stale());
    }

    #[test]
    fn fresh_state_is_not_stale() {
        let snap = StaleState::new().snapshot();
        assert!(!snap.rag_stale());
        assert!(!snap.structural_stale());
    }
}
