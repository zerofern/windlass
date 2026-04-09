use tokio::sync::mpsc;
use uuid::Uuid;
use windlass_core::events::Event;

/// A per-action handle used to send the result event produced by an async
/// action handler back into the event stream, carrying the action's ID so
/// the debug system can record the causation link.
///
/// Constructed by the shell dispatch callback before each `execute` call.
/// When debug mode is off the event is forwarded directly to the main channel
/// with zero overhead; when on it goes through the dedicated causation channel
/// so the main loop can record `caused_by_action` on the resulting event.
pub struct CausalTx {
    action_id: Uuid,
    inner: CausalTxInner,
}

enum CausalTxInner {
    /// Debug mode — sends `(Event, action_id)` to the causation channel so
    /// the shell loop can link the resulting event back to this action.
    Debug(mpsc::Sender<(Event, Uuid)>),
    /// Non-debug mode — sends the event directly to the main event channel.
    Plain(mpsc::Sender<Event>),
}

impl CausalTx {
    pub fn debug(action_id: Uuid, tx: mpsc::Sender<(Event, Uuid)>) -> Self {
        Self { action_id, inner: CausalTxInner::Debug(tx) }
    }

    pub fn plain(action_id: Uuid, tx: mpsc::Sender<Event>) -> Self {
        Self { action_id, inner: CausalTxInner::Plain(tx) }
    }

    /// Sends the result event produced by this action. Consumes `self` since
    /// each action produces exactly one result event.
    pub async fn send(self, event: Event) {
        match self.inner {
            CausalTxInner::Debug(tx) => {
                let _ = tx.send((event, self.action_id)).await;
            }
            CausalTxInner::Plain(tx) => {
                let _ = tx.send(event).await;
            }
        }
    }
}
