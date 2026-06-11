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
/// the async forwarder. Each variant maps to a `ProgressNotificationParam`
/// via [`Phase::into_message_and_total`].
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Indexer(cartog_indexer::ProgressUpdate),
    Rag(cartog_rag::indexer::ProgressUpdate),
}

impl Phase {
    /// Render `(message, total)` for [`ProgressNotificationParam`].
    ///
    /// The `progress` counter itself is owned by the forwarder so it can
    /// stay monotonic across phases (the MCP spec requires this).
    pub fn into_message_and_total(self) -> (String, Option<f64>) {
        use cartog_indexer::ProgressUpdate as IxU;
        use cartog_rag::indexer::ProgressUpdate as RgU;
        // Labels come from `ProgressUpdate::label()` (single source of truth in
        // cartog-indexer / cartog-rag); only the `total` for the MCP progress
        // bar is computed here.
        match self {
            Phase::Indexer(u @ IxU::Parsing { total, .. }) => (u.label(), Some(total as f64)),
            Phase::Indexer(u @ IxU::Storing { total, .. }) => (u.label(), Some(total as f64)),
            Phase::Indexer(u @ IxU::ResolvingLsp { total, .. }) => (u.label(), Some(total as f64)),
            Phase::Indexer(u) => (u.label(), None),
            Phase::Rag(u @ RgU::Embedding { total, .. }) => (u.label(), Some(total as f64)),
            Phase::Rag(u) => (u.label(), None),
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
/// pushes them to `notifier` as `notifications/progress` with the given
/// token.
///
/// `progress` is a monotonically-increasing counter incremented once per
/// emitted event, per the MCP spec.
pub fn spawn_forwarder(token: ProgressToken, notifier: Notifier) -> Forwarder {
    let (tx, mut rx) = mpsc::channel::<Phase>(CHANNEL_CAPACITY);
    let join = tokio::spawn(async move {
        let mut counter: f64 = 0.0;
        while let Some(phase) = rx.recv().await {
            counter += 1.0;
            let (message, total) = phase.into_message_and_total();
            (notifier)(ProgressNotificationParam {
                progress_token: token.clone(),
                progress: counter,
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
        cb(IxU::Parsing {
            processed: 3,
            total: 7,
        });
        cb(IxU::Storing {
            processed: 2,
            total: 5,
        });

        drop(cb);
        drop(fwd.tx);
        fwd.join.await.unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert!(events[0].progress < events[1].progress);
        assert!(events[1].progress < events[2].progress);
        assert_eq!(events[0].message.as_deref(), Some("scanning files"));
        assert_eq!(events[1].message.as_deref(), Some("parsing 3/7 files"));
        assert_eq!(events[2].message.as_deref(), Some("storing 2/5 files"));
        assert_eq!(events[0].total, None);
        assert_eq!(events[1].total, Some(7.0));
        assert_eq!(events[2].total, Some(5.0));
    }

    #[test]
    fn resolving_lsp_phase_maps_to_message() {
        let (msg, total) = Phase::Indexer(IxU::ResolvingLsp {
            processed: 4,
            total: 9,
        })
        .into_message_and_total();
        assert_eq!(msg, "resolving edges 4/9 files");
        assert_eq!(total, Some(9.0));
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
        assert_eq!(events[1].total, Some(1024.0));
        assert_eq!(events[2].message.as_deref(), Some("storing embeddings"));
    }

    #[tokio::test]
    async fn lsp_phase_forwards_per_file_counter() {
        let (notifier, events) = capturing_notifier();
        let fwd = spawn_forwarder(token(), notifier);

        fwd.tx
            .send(Phase::Indexer(IxU::ResolvingLsp {
                processed: 3,
                total: 7,
            }))
            .await
            .unwrap();
        drop(fwd.tx);
        fwd.join.await.unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].message.as_deref(),
            Some("resolving edges 3/7 files")
        );
        assert_eq!(events[0].total, Some(7.0));
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
        cb(IxU::Parsing {
            processed: 0,
            total: 1,
        });
    }
}
