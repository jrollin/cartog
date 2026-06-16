//! Bridge between cartog's library-level progress callbacks and MCP
//! `notifications/progress`.
//!
//! Long-running tools (`cartog_index`, `cartog_rag_index`) accept a
//! synchronous callback and run inside `tokio::task::spawn_blocking`. The
//! bridge converts those synchronous events into asynchronous
//! `notifications/progress` calls on the MCP peer, gated on the request
//! supplying a `progressToken` in `_meta`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::model::{ProgressNotificationParam, ProgressToken};
use tokio::sync::mpsc;

/// Internal, transport-neutral phase event shipped from a blocking task to
/// the async forwarder, which renders it (via `Phase::render`) and accumulates
/// it into a `ProgressNotificationParam`.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Indexer(cartog_indexer::ProgressUpdate),
    Rag(cartog_rag::indexer::ProgressUpdate),
}

/// A phase rendered for the wire: human label plus its in-phase `(done, total)`
/// when the phase carries a counter. The forwarder turns these per-phase counts
/// into a globally-monotonic `progress` (the MCP spec requires `progress` to
/// strictly increase per token), so `done`/`total` here are phase-local, not
/// cumulative.
struct PhaseProgress {
    message: String,
    /// `Some((done, total))` for counting phases (parse/store/resolve/embed);
    /// `None` for marker phases (walking, preparing, storing-embeddings).
    counts: Option<(u32, u32)>,
    /// Stable discriminant of the phase so the forwarder can tell one counting
    /// phase from the next (two phases can both start at `done == 0`).
    kind: PhaseKind,
}

/// Identity of a phase, used by the forwarder to detect boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhaseKind {
    Walking,
    Parsing,
    Storing,
    ResolvingLsp,
    Preparing,
    Embedding,
    StoringEmbeddings,
}

impl Phase {
    /// Render the phase for the wire. Labels come from `ProgressUpdate::label()`
    /// (single source of truth in cartog-indexer / cartog-rag); the per-phase
    /// `(done, total)` is extracted here for the forwarder to accumulate.
    fn render(self) -> PhaseProgress {
        use cartog_indexer::ProgressUpdate as IxU;
        use cartog_rag::indexer::ProgressUpdate as RgU;
        match self {
            Phase::Indexer(u @ IxU::Parsing { done, total }) => PhaseProgress {
                message: u.label(),
                counts: Some((done, total)),
                kind: PhaseKind::Parsing,
            },
            Phase::Indexer(u @ IxU::Storing { done, total }) => PhaseProgress {
                message: u.label(),
                counts: Some((done, total)),
                kind: PhaseKind::Storing,
            },
            Phase::Indexer(u @ IxU::ResolvingLsp { done, total }) => PhaseProgress {
                message: u.label(),
                counts: Some((done, total)),
                kind: PhaseKind::ResolvingLsp,
            },
            Phase::Indexer(u @ IxU::Walking) => PhaseProgress {
                message: u.label(),
                counts: None,
                kind: PhaseKind::Walking,
            },
            Phase::Rag(u @ RgU::Embedding { processed, total }) => PhaseProgress {
                message: u.label(),
                counts: Some((processed, total)),
                kind: PhaseKind::Embedding,
            },
            Phase::Rag(u @ RgU::Preparing) => PhaseProgress {
                message: u.label(),
                counts: None,
                kind: PhaseKind::Preparing,
            },
            Phase::Rag(u @ RgU::Storing) => PhaseProgress {
                message: u.label(),
                counts: None,
                kind: PhaseKind::StoringEmbeddings,
            },
        }
    }
}

/// Async notification dispatcher. Real impl wraps an rmcp `Peer`; tests
/// substitute a closure that pushes into a `Vec`.
pub type Notifier = Arc<
    dyn Fn(ProgressNotificationParam) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

/// Build a [`Notifier`] backed by an rmcp server peer. Errors from
/// `notify_progress` are intentionally swallowed — progress is best-effort.
pub fn peer_notifier(peer: rmcp::Peer<rmcp::RoleServer>) -> Notifier {
    Arc::new(move |param| {
        let peer = peer.clone();
        Box::pin(async move {
            if let Err(e) = peer.notify_progress(param).await {
                tracing::debug!("notify_progress dropped: {e}");
            }
        })
    })
}

/// Handle returned by [`spawn_forwarder`]. Drop the `tx` to signal "no more
/// events"; the forwarder drains then exits. Then `await` the join handle
/// so every queued notification is written before the tool result returns.
pub struct Forwarder {
    pub tx: mpsc::Sender<Phase>,
    pub join: tokio::task::JoinHandle<()>,
}

/// Bounded channel capacity. Phase events from both tools are sparse (at
/// most ~`N/512 + 3` for very large RAG runs), so 64 is comfortable. We
/// use `try_send` on the producer side so a stalled client cannot block
/// the indexer thread; dropped events are logged at debug level.
const CHANNEL_CAPACITY: usize = 64;

/// Spawn an async forwarder that consumes [`Phase`] events from `rx` and
/// pushes them to `notifier` as `notifications/progress` with the given token.
///
/// The MCP spec requires `progress` to *increase* with each notification, but
/// each phase reports a `done` that resets to 0 at its start. The forwarder
/// bridges this by accumulating a `base` (the summed totals of all completed
/// phases): within a counting phase it reports `progress = base + done` against
/// `total = base + phase_total`, so the bar climbs coherently across phases
/// instead of resetting or overshooting. Marker phases with no counter (walking,
/// preparing, storing-embeddings) step `progress` by one with no `total`. A new
/// phase is recognized when the event's [`PhaseKind`] changes.
///
/// To satisfy the spec's "MUST increase" clause, an internal `last_progress`
/// clamp keeps the running value non-decreasing across phase boundaries and
/// out-of-order rayon emits (which can make `done` jitter), and a frame is only
/// emitted when its `progress` strictly exceeds the last *emitted* value.
/// Duplicate or straggler events that carry no forward progress are dropped
/// rather than sent equal, so every notification on the wire strictly increases
/// (this also tightens flood control). A phase boundary always advances `base`,
/// so the first frame of a new phase clears the previous one.
pub fn spawn_forwarder(token: ProgressToken, notifier: Notifier) -> Forwarder {
    let (tx, mut rx) = mpsc::channel::<Phase>(CHANNEL_CAPACITY);
    let join = tokio::spawn(async move {
        let mut base: f64 = 0.0;
        let mut cur: Option<(PhaseKind, u32)> = None; // (kind, total) of the phase in flight
        let mut last_progress: f64 = 0.0;
        let mut last_emitted: Option<f64> = None; // strictly-increasing wire guard
        while let Some(phase) = rx.recv().await {
            let PhaseProgress {
                message,
                counts,
                kind,
            } = phase.render();
            // Phase boundary: a different kind arrived. Fold the finished phase's
            // total (a counting phase) or its single marker unit into the base.
            if let Some((prev_kind, prev_total)) = cur {
                if prev_kind != kind {
                    base += if prev_total > 0 {
                        prev_total as f64
                    } else {
                        1.0
                    };
                }
            }
            let total = match counts {
                Some((done, total)) => {
                    cur = Some((kind, total));
                    last_progress = last_progress.max(base + done as f64);
                    Some(base + total as f64)
                }
                None => {
                    // Marker phase: occupies one unit, no determinate total.
                    cur = Some((kind, 0));
                    last_progress = last_progress.max(base);
                    None
                }
            };
            // Spec: progress MUST increase per notification. Drop frames that
            // would tie or dip the last emitted value (duplicates/stragglers).
            if last_emitted.is_some_and(|prev| last_progress <= prev) {
                continue;
            }
            last_emitted = Some(last_progress);
            (notifier)(ProgressNotificationParam {
                progress_token: token.clone(),
                progress: last_progress,
                total,
                message: Some(message),
            })
            .await;
        }
    });
    Forwarder { tx, join }
}

/// Build a library-side callback from an mpsc sender. The returned closure
/// is `'static`, `Send`, `Sync`, suitable for handing to library code that
/// will run inside `tokio::task::spawn_blocking`.
///
/// Events are sent best-effort via `try_send`. If the forwarder has been
/// dropped or the channel is full, the event is silently discarded.
pub fn indexer_callback(
    tx: mpsc::Sender<Phase>,
) -> impl Fn(cartog_indexer::ProgressUpdate) + Send + Sync + 'static {
    move |u| {
        if tx.try_send(Phase::Indexer(u)).is_err() {
            tracing::debug!("progress channel full or closed; dropping event");
        }
    }
}

/// Same as [`indexer_callback`] for the RAG indexer.
pub fn rag_callback(
    tx: mpsc::Sender<Phase>,
) -> impl Fn(cartog_rag::indexer::ProgressUpdate) + Send + Sync + 'static {
    move |u| {
        if tx.try_send(Phase::Rag(u)).is_err() {
            tracing::debug!("progress channel full or closed; dropping event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_indexer::ProgressUpdate as IxU;
    use cartog_rag::indexer::ProgressUpdate as RgU;
    use rmcp::model::NumberOrString;
    use std::sync::Mutex;

    fn token() -> ProgressToken {
        ProgressToken(NumberOrString::String("test-token".into()))
    }

    fn capturing_notifier() -> (Notifier, Arc<Mutex<Vec<ProgressNotificationParam>>>) {
        let events: Arc<Mutex<Vec<ProgressNotificationParam>>> = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let notifier: Notifier = Arc::new(move |param| {
            let captured = Arc::clone(&captured);
            Box::pin(async move {
                captured.lock().unwrap().push(param);
            })
        });
        (notifier, events)
    }

    #[tokio::test]
    async fn forwarder_emits_one_notification_per_event_in_order() {
        let (notifier, events) = capturing_notifier();
        let fwd = spawn_forwarder(token(), notifier);

        let cb = indexer_callback(fwd.tx.clone());
        cb(IxU::Walking);
        cb(IxU::Parsing { done: 0, total: 7 });
        cb(IxU::Storing { done: 0, total: 5 });

        drop(cb);
        drop(fwd.tx);
        fwd.join.await.unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 3);
        // progress is globally monotonic across phases; totals are cumulative
        // (base + phase total) so the bar climbs instead of resetting per phase.
        assert!(events[0].progress < events[1].progress);
        assert!(events[1].progress < events[2].progress);
        assert_eq!(events[0].message.as_deref(), Some("scanning files"));
        assert_eq!(events[1].message.as_deref(), Some("parsing 7 files"));
        assert_eq!(events[2].message.as_deref(), Some("storing 5 files"));
        assert_eq!(events[0].total, None); // Walking marker
        assert_eq!(events[1].total, Some(8.0)); // base 1 (walking) + 7
        assert_eq!(events[2].total, Some(13.0)); // base 8 (walking+parsing) + 5
    }

    #[test]
    fn resolving_lsp_phase_renders_message_and_counts() {
        let start = Phase::Indexer(IxU::ResolvingLsp { done: 0, total: 9 }).render();
        assert_eq!(start.message, "resolving 9 edges with LSP");
        assert_eq!(start.counts, Some((0, 9)));
        let mid = Phase::Indexer(IxU::ResolvingLsp { done: 3, total: 9 }).render();
        assert_eq!(mid.message, "resolving 3/9 edges with LSP");
        assert_eq!(mid.counts, Some((3, 9)));
    }

    #[tokio::test]
    async fn rag_callback_round_trips_through_forwarder() {
        let (notifier, events) = capturing_notifier();
        let fwd = spawn_forwarder(token(), notifier);

        let cb = rag_callback(fwd.tx.clone());
        cb(RgU::Preparing);
        cb(RgU::Embedding {
            processed: 512,
            total: 1024,
        });
        cb(RgU::Storing);

        drop(cb);
        drop(fwd.tx);
        fwd.join.await.unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].message.as_deref(), Some("preparing"));
        assert_eq!(events[1].message.as_deref(), Some("embedding 512/1024"));
        assert_eq!(events[1].total, Some(1025.0)); // base 1 (preparing) + 1024
        assert_eq!(events[2].message.as_deref(), Some("storing embeddings"));
        // Spec: progress strictly increases across the marker→counting→marker
        // boundaries (0 → 513 → 1025).
        assert!(events[0].progress < events[1].progress);
        assert!(events[1].progress < events[2].progress);
    }

    #[tokio::test]
    async fn progress_climbs_within_phase_and_stays_monotonic_across_phases() {
        let (notifier, events) = capturing_notifier();
        let fwd = spawn_forwarder(token(), notifier);
        let cb = indexer_callback(fwd.tx.clone());

        // A full parse phase that climbs, then a store phase that also climbs.
        cb(IxU::Parsing {
            done: 0,
            total: 100,
        });
        cb(IxU::Parsing {
            done: 64,
            total: 100,
        });
        cb(IxU::Parsing {
            done: 100,
            total: 100,
        });
        cb(IxU::Storing { done: 0, total: 40 });
        cb(IxU::Storing {
            done: 40,
            total: 40,
        });

        drop(cb);
        drop(fwd.tx);
        fwd.join.await.unwrap();

        let e = events.lock().unwrap();
        // The store phase's first event (done 0) ties the parse phase's final
        // progress (100), so it is dropped to keep the wire strictly increasing:
        // 0 → 64 → 100 → 140, four frames, not five.
        assert_eq!(e.len(), 4);
        // Within the parse phase: 0 → 64 → 100 against total 100.
        assert_eq!((e[0].progress, e[0].total), (0.0, Some(100.0)));
        assert_eq!((e[1].progress, e[1].total), (64.0, Some(100.0)));
        assert_eq!((e[2].progress, e[2].total), (100.0, Some(100.0)));
        // Store phase folds the finished parse total (100) into the base and
        // climbs to 140 against cumulative total 140 — never resets to 0.
        assert_eq!((e[3].progress, e[3].total), (140.0, Some(140.0)));
        // Spec: progress strictly increases with each notification.
        for w in e.windows(2) {
            assert!(w[0].progress < w[1].progress);
        }
    }

    #[tokio::test]
    async fn out_of_order_straggler_is_dropped_not_emitted_equal() {
        let (notifier, events) = capturing_notifier();
        let fwd = spawn_forwarder(token(), notifier);
        let cb = indexer_callback(fwd.tx.clone());

        // Rayon workers can emit out of order: a higher done arrives before a
        // lower one. The straggler clamps to the running max, which ties the
        // last emitted value — so it is dropped, not sent equal (spec: progress
        // MUST increase per notification).
        cb(IxU::Parsing {
            done: 0,
            total: 100,
        });
        cb(IxU::Parsing {
            done: 64,
            total: 100,
        });
        cb(IxU::Parsing {
            done: 32,
            total: 100,
        }); // late straggler, lower done

        drop(cb);
        drop(fwd.tx);
        fwd.join.await.unwrap();

        let e = events.lock().unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].progress, 0.0);
        assert_eq!(e[1].progress, 64.0); // straggler (clamped to 64) suppressed
    }

    #[tokio::test]
    async fn duplicate_events_never_emit_equal_progress() {
        let (notifier, events) = capturing_notifier();
        let fwd = spawn_forwarder(token(), notifier);
        let cb = indexer_callback(fwd.tx.clone());

        // Rayon can re-emit an identical (done, total). The spec requires every
        // notification to increase, so duplicates are dropped, not sent equal.
        cb(IxU::Parsing {
            done: 10,
            total: 50,
        });
        cb(IxU::Parsing {
            done: 10,
            total: 50,
        });
        cb(IxU::Parsing {
            done: 10,
            total: 50,
        });

        drop(cb);
        drop(fwd.tx);
        fwd.join.await.unwrap();

        let e = events.lock().unwrap();
        assert_eq!(e.len(), 1, "duplicate-progress frames must be suppressed");
        assert_eq!(e[0].progress, 10.0);
    }

    #[tokio::test]
    async fn dropped_sender_lets_forwarder_exit_cleanly() {
        let (notifier, events) = capturing_notifier();
        let fwd = spawn_forwarder(token(), notifier);
        drop(fwd.tx);
        fwd.join.await.unwrap();
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn full_channel_drops_event_silently_without_panic() {
        let (tx, _rx) = mpsc::channel::<Phase>(1);
        let cb = indexer_callback(tx);
        cb(IxU::Walking);
        cb(IxU::Parsing { done: 0, total: 1 });
    }
}
