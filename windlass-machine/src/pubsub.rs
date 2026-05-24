use tokio::sync::mpsc;
use tracing::warn;

/// Maps a publish message to its topic discriminant.
pub trait HasTopic<T: Clone> {
    fn topic(&self) -> T;
}

/// Sender used by transport connections to register topic subscriptions.
pub type SubscriberReg<T, P> = mpsc::UnboundedSender<(Vec<T>, mpsc::Sender<P>)>;

/// Single-owner fanout for typed pub/sub messages.
pub struct TopicFanout<T, P> {
    subscribers: Vec<(Vec<T>, mpsc::Sender<P>)>,
    reg_rx: mpsc::UnboundedReceiver<(Vec<T>, mpsc::Sender<P>)>,
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

    /// Push `msg` to every subscriber whose topic list contains `msg.topic()`.
    pub fn send(&mut self, msg: &P) {
        while let Ok(entry) = self.reg_rx.try_recv() {
            self.subscribers.push(entry);
        }

        let topic = msg.topic();
        self.subscribers.retain_mut(|(topics, tx)| {
            if !topics.contains(&topic) {
                return true;
            }

            match tx.try_send(msg.clone()) {
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
    use super::{HasTopic, TopicFanout};

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

        fanout.send(&Publish::State);

        assert_eq!(state_rx.recv().await, Some(Publish::State));
        assert!(activity_rx.try_recv().is_err());

        fanout.send(&Publish::Activity);

        assert_eq!(activity_rx.recv().await, Some(Publish::Activity));
    }

    #[tokio::test]
    async fn send_prunes_closed_subscribers() {
        let (mut fanout, reg_tx) = TopicFanout::new();
        let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);

        reg_tx
            .send((vec![Topic::State], closed_tx))
            .expect("subscriber registration should succeed");
        drop(closed_rx);

        fanout.send(&Publish::State);
        fanout.send(&Publish::State);
    }
}
