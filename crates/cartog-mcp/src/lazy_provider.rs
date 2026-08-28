//! Deferred construction for the reranker provider.
//!
//! Loading the cross-encoder commits ~162 MB of heap that a server which never
//! serves a semantic query has no use for. Measured with `footprint` on an idle
//! `cartog serve`: 246 MB with the reranker eagerly loaded vs 91 MB without, and
//! a config-less degraded peer (no DB, no embeddings) paid the same 167 MB of
//! `MALLOC_LARGE` as a real one. With several projects open at once that
//! dominated the machine's committed memory, so the cross-encoder is built on
//! first use instead of at server start.
//!
//! The embedding provider stays eager: `reconcile_embedding_fingerprint` needs
//! its name/model/dimension at start, and that reconcile is what keeps a
//! provider swap from silently serving vectors from the previous model.

use std::sync::{Mutex, MutexGuard};

use cartog_rag as rag;

/// A provider built on first access from the config captured at server start.
///
/// `T` is the built value; `C` the config needed to build it. The closure runs
/// at most once per `Lazy`, under the same lock callers already take, so a
/// concurrent second caller waits for the first build rather than duplicating
/// it.
pub(crate) struct Lazy<T, C> {
    /// `None` until first access. The build result is cached even when it is a
    /// provider-absent `None` (see [`LazyReranker`]), so a missing model isn't
    /// re-probed on every query.
    cell: Mutex<Option<T>>,
    config: C,
    build: fn(&C) -> T,
}

impl<T, C> Lazy<T, C> {
    pub(crate) fn new(config: C, build: fn(&C) -> T) -> Self {
        Self {
            cell: Mutex::new(None),
            config,
            build,
        }
    }

    /// Borrow the value, building it on first call.
    ///
    /// Returns the poison error unchanged so callers keep their existing
    /// "lock poisoned (server restart required)" message.
    pub(crate) fn get(
        &self,
    ) -> Result<MutexGuard<'_, Option<T>>, std::sync::PoisonError<MutexGuard<'_, Option<T>>>> {
        let mut guard = self.cell.lock()?;
        if guard.is_none() {
            *guard = Some((self.build)(&self.config));
        }
        Ok(guard)
    }

    /// Whether the value has been built yet. Test-only: the point of this type
    /// is that nothing else needs to care.
    #[cfg(test)]
    pub(crate) fn is_loaded(&self) -> bool {
        self.cell.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

/// Reranker provider, built on first use. `None` means "not configured or
/// unavailable" — `create_reranker_provider` already logs and degrades rather
/// than failing, so there is no `Result` here.
pub(crate) type LazyReranker =
    Lazy<Option<Box<dyn rag::provider::RerankerProvider>>, rag::EmbeddingProviderConfig>;

pub(crate) fn lazy_reranker(config: rag::EmbeddingProviderConfig) -> LazyReranker {
    Lazy::new(config, |c| {
        rag::create_reranker_provider(
            &c.reranker_provider,
            c.reranker_model.as_deref(),
            c.intra_threads,
        )
    })
}

/// A `LazyReranker` that yields no provider without loading anything. Used by
/// the in-crate test harnesses that construct a server directly.
#[cfg(test)]
pub(crate) fn no_reranker() -> LazyReranker {
    Lazy::new(rag::EmbeddingProviderConfig::default(), |_| None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_on_first_access_not_construction() {
        let lazy: Lazy<u32, u32> = Lazy::new(7, |c| *c + 1);
        assert!(!lazy.is_loaded(), "must not build at construction");
        assert_eq!(*lazy.get().unwrap(), Some(8));
        assert!(lazy.is_loaded(), "must be built after first access");
    }

    #[test]
    fn builds_only_once() {
        static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let lazy: Lazy<u32, u32> = Lazy::new(1, |c| {
            BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *c
        });
        for _ in 0..3 {
            // Bind then drop: `let _ =` on a guard would release it immediately
            // and trips `let_underscore_lock`.
            let guard = lazy.get().unwrap();
            drop(guard);
        }
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
