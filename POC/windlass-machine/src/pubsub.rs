use tokio::sync::mpsc;
use tracing::warn;

use crate::machine::PublishEnvelope;

/// Maps a publish message to its topic discriminant.
pub trait HasTopic<T: Clone> {
    fn topic(&self) -> T;
}

/// Sender used by transport connections to register topic subscriptions.
///
/// Subscribers receive [`PublishEnvelope<P>`] (not bare `P`) so cross-
/// core bridges can preserve the `publish_id` when constructing
/// resulting events via [`crate::Timed::from_publish`].  This is the
/// §37pre C2 lock that makes the cross-core causal graph navigable.
pub type SubscriberReg<T, P> = mpsc::UnboundedSender<(Vec<T>, mpsc::Sender<PublishEnvelope<P>>)>;

/// Single-owner fanout for typed pub/sub messages.
pub struct TopicFanout<T, P> {
    subscribers: Vec<(Vec<T>, mpsc::Sender<PublishEnvelope<P>>)>,
    reg_rx: mpsc::UnboundedReceiver<(Vec<T>, mpsc::Sender<PublishEnvelope<P>>)>,
}

impl<T, P> TopicFanout<T, P>
where
    T: PartialEq + Clone + Send + 'static,
    P: HasTopic<T> + Clone + Send + 'static,
{
    #[must_use]
    pub fn new() -> (Self, SubscriberReg<T, P>) {
        let (reg_tx, reg_rx) = mpsc::unbounded_channel();
        (
            Self {
                subscribers: Vec::new(),
                reg_rx,
            },
            reg_tx,
        )
    }

    /// Push `envelope` to every subscriber whose topic list contains
    /// `envelope.payload.topic()`.  Subscribers receive the envelope
    /// verbatim, preserving the runtime-minted `publish_id` so bridges
    /// can construct downstream events with
    /// `Timed::from_publish(now, envelope.id, derived_event)`.
    pub fn send(&mut self, envelope: &PublishEnvelope<P>) {
        while let Ok(entry) = self.reg_rx.try_recv() {
            self.subscribers.push(entry);
        }

        let topic = envelope.payload.topic();
        self.subscribers.retain_mut(|(topics, tx)| {
            if !topics.contains(&topic) {
                return true;
            }

            match tx.try_send(envelope.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("topic subscriber lagging, message dropped");
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{HasTopic, TopicFanout};
    use crate::machine::PublishEnvelope;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Topic {
        State,
        Activity,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Publish {
        State,
        Activity,
    }

    impl HasTopic<Topic> for Publish {
        fn topic(&self) -> Topic {
            match self {
                Self::State => Topic::State,
                Self::Activity => Topic::Activity,
            }
        }
    }

    fn env(payload: Publish) -> PublishEnvelope<Publish> {
        PublishEnvelope {
            id: Uuid::new_v4(),
            payload,
        }
    }

    #[tokio::test]
    async fn send_delivers_to_matching_subscribers() {
        let (mut fanout, reg_tx) = TopicFanout::new();
        let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(1);
        let (activity_tx, mut activity_rx) = tokio::sync::mpsc::channel(1);

        reg_tx
            .send((vec![Topic::State], state_tx))
            .expect("subscriber registration should succeed");
        reg_tx
            .send((vec![Topic::Activity], activity_tx))
            .expect("subscriber registration should succeed");

        fanout.send(&env(Publish::State));

        assert_eq!(
            state_rx.recv().await.map(|e| e.payload),
            Some(Publish::State)
        );
        assert!(activity_rx.try_recv().is_err());

        fanout.send(&env(Publish::Activity));

        assert_eq!(
            activity_rx.recv().await.map(|e| e.payload),
            Some(Publish::Activity)
        );
    }

    #[tokio::test]
    async fn send_preserves_publish_id() {
        let (mut fanout, reg_tx) = TopicFanout::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        reg_tx.send((vec![Topic::State], tx)).unwrap();

        let envelope = env(Publish::State);
        let expected_id = envelope.id;
        fanout.send(&envelope);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, expected_id, "publish_id must round-trip");
    }

    #[tokio::test]
    async fn send_prunes_closed_subscribers() {
        let (mut fanout, reg_tx) = TopicFanout::new();
        let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);

        reg_tx
            .send((vec![Topic::State], closed_tx))
            .expect("subscriber registration should succeed");
        drop(closed_rx);

        fanout.send(&env(Publish::State));
        fanout.send(&env(Publish::State));
    }
}
