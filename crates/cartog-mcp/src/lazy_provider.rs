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

use std::sync::{Mutex, MutexGuard, PoisonError};

use cartog_rag as rag;

/// A provider built on first access from the config captured at server start.
///
/// `T` is the built value; `C` the config needed to build it. The build runs at
/// most once per `Lazy` in the common case, and [`Lazy::prime`] runs it *outside*
/// the cell lock so a slow build never blocks an unrelated caller.
///
/// [`Lazy::get`] hands back the guard, which the caller then holds for as long as
/// it uses the value — so callers serialize on each other's whole use of it, not
/// merely on the build. That is inherent here: the reranker's `score_batch` takes
/// `&mut self`.
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

    /// Build the value if it isn't built yet, holding no lock while doing so.
    ///
    /// Call this *before* acquiring any other lock. Building the cross-encoder
    /// can download ~150 MB on a cold cache, and the build used to run inside
    /// `get()` with the caller's database and embedding-provider locks already
    /// held — stalling every other tool on the server behind a network fetch.
    ///
    /// A concurrent second caller may duplicate the build; the loser's value is
    /// dropped. That is the deliberate trade for never holding a lock across an
    /// unbounded operation, and it costs at most one extra build per process.
    pub(crate) fn prime(&self) -> Result<(), PoisonError<MutexGuard<'_, Option<T>>>> {
        if self.cell.lock()?.is_some() {
            return Ok(());
        }
        let built = (self.build)(&self.config);
        let mut guard = self.cell.lock()?;
        // Only the first writer wins: a racing caller may already have stored one.
        if guard.is_none() {
            *guard = Some(built);
        }
        Ok(())
    }

    /// Borrow the value, building it on first call.
    ///
    /// Prefer [`Lazy::prime`] before taking other locks; this still builds
    /// in-place as a fallback so the value is never observed missing.
    ///
    /// Returns the poison error unchanged so callers keep their existing
    /// "lock poisoned (server restart required)" message.
    pub(crate) fn get(
        &self,
    ) -> Result<MutexGuard<'_, Option<T>>, PoisonError<MutexGuard<'_, Option<T>>>> {
        let mut guard = self.cell.lock()?;
        if guard.is_none() {
            *guard = Some((self.build)(&self.config));
        }
        Ok(guard)
    }

    /// Whether the value has been built yet.
    ///
    /// Exists for the lazy-load assertions: the ~162 MB deferral is what the
    /// memory win rests on, and `make bench-memory` — the only other guard —
    /// runs on macOS alone and in no CI job.
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
    fn prime_builds_without_holding_the_cell_lock() {
        // The whole point of `prime`: a slow build must not block a concurrent
        // reader of the same cell. If the build ran under the lock, the probe
        // below would block until the build finished and observe `true`.
        use std::sync::atomic::{AtomicBool, Ordering};
        static IN_BUILD: AtomicBool = AtomicBool::new(false);
        static LOCKED_DURING_BUILD: AtomicBool = AtomicBool::new(false);

        let lazy: Lazy<u32, u32> = Lazy::new(1, |c| {
            IN_BUILD.store(true, Ordering::SeqCst);
            // Give the probe a window to try the lock mid-build.
            std::thread::sleep(std::time::Duration::from_millis(50));
            IN_BUILD.store(false, Ordering::SeqCst);
            *c
        });

        std::thread::scope(|s| {
            s.spawn(|| lazy.prime().unwrap());
            s.spawn(|| {
                // Spin until the build is running, then confirm the cell is free.
                while !IN_BUILD.load(Ordering::SeqCst) {
                    std::hint::spin_loop();
                }
                if lazy.cell.try_lock().is_err() {
                    LOCKED_DURING_BUILD.store(true, Ordering::SeqCst);
                }
            });
        });

        assert!(
            !LOCKED_DURING_BUILD.load(Ordering::SeqCst),
            "prime held the cell lock across the build — that is what stalls \
             every other tool behind a cold-cache model download"
        );
        assert!(lazy.is_loaded(), "prime must leave the value built");
    }

    #[test]
    fn prime_then_get_does_not_rebuild() {
        static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let lazy: Lazy<u32, u32> = Lazy::new(5, |c| {
            BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *c
        });
        lazy.prime().unwrap();
        lazy.prime().unwrap();
        assert_eq!(*lazy.get().unwrap(), Some(5));
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 1);
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
