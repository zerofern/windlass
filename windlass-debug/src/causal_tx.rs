use std::future::Future;

use tokio::sync::mpsc;
use uuid::Uuid;
use windlass_core::events::Event;

tokio::task_local! {
    /// The ID of the action currently executing in this task.
    /// Set by `CausalTx::run`; read by `make_http_observer` to link HTTP
    /// exchanges to the action that triggered them.
    pub(crate) static CURRENT_ACTION_ID: Option<Uuid>;
}

/// A per-action handle used to send the result event produced by an async
/// action handler back into the event stream, carrying the action's ID so
/// the debug system can record the causation link.
///
/// Constructed by the shell dispatch callback before each `execute` call.
/// When debug mode is off the event is forwarded directly to the main channel
/// with zero overhead; when on it goes through the dedicated causation channel
/// so the main loop can record `caused_by_action` on the resulting event.
pub struct CausalTx {
    pub(crate) action_id: Uuid,
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
    #[must_use]
    pub const fn debug(action_id: Uuid, tx: mpsc::Sender<(Event, Uuid)>) -> Self {
        Self {
            action_id,
            inner: CausalTxInner::Debug(tx),
        }
    }

    #[must_use]
    pub const fn plain(action_id: Uuid, tx: mpsc::Sender<Event>) -> Self {
        Self {
            action_id,
            inner: CausalTxInner::Plain(tx),
        }
    }

    /// Wraps `f` in a `CURRENT_ACTION_ID` scope so that any HTTP calls made
    /// inside the spawned task can be attributed to this action.
    ///
    /// Usage:
    /// ```ignore
    /// tokio::spawn(causal_tx.run(|causal_tx| async move {
    ///     let event = client.do_thing().await;
    ///     causal_tx.send(event).await;
    /// }));
    /// ```
    pub fn run<F, Fut>(self, f: F) -> impl Future<Output = ()>
    where
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = ()>,
    {
        let id = self.action_id;
        CURRENT_ACTION_ID.scope(Some(id), f(self))
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn init_event() -> Event {
        Event::Init {
            at: Utc::now(),
            is_gluetun_healthy: true,
            port_files: Err("no".to_string()),
        }
    }

    #[tokio::test]
    async fn causal_tx_plain_sends_event() {
        let (tx, mut rx) = mpsc::channel(1);
        let id = Uuid::new_v4();
        let causal = CausalTx::plain(id, tx);
        causal.send(init_event()).await;
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn causal_tx_debug_sends_event_with_action_id() {
        let (tx, mut rx) = mpsc::channel(1);
        let id = Uuid::new_v4();
        let causal = CausalTx::debug(id, tx);
        causal.send(init_event()).await;
        let (_, action_id) = rx.recv().await.unwrap();
        assert_eq!(action_id, id);
    }

    #[tokio::test]
    async fn causal_tx_run_sets_task_local() {
        let (tx, _rx) = mpsc::channel(1);
        let id = Uuid::new_v4();
        let causal = CausalTx::plain(id, tx);
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(causal.run(|_ctx| async move {
            let captured = CURRENT_ACTION_ID.with(|v| *v);
            let _ = result_tx.send(captured);
        }))
        .await
        .unwrap();
        let captured = result_rx.await.unwrap();
        assert_eq!(captured, Some(id));
    }
}
