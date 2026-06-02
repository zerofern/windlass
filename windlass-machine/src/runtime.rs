use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::machine::{Machine, Timed};
use crate::pubsub::{SubscriberReg, TopicFanout};
use crate::shell::Shell;

/// A command paired with the channel its typed response is returned on.
pub type Command<M> = (
    <M as Machine>::Command,
    oneshot::Sender<<M as Machine>::Response>,
);

/// Channels for talking to a running [`ServiceRuntime`].
pub struct ServiceHandles<M: Machine> {
    /// Inject timed events (I/O results, timer fires, forwarded publishes).
    pub events: mpsc::UnboundedSender<Timed<M::Event>>,
    /// Issue commands and receive a typed response per command.
    pub commands: mpsc::UnboundedSender<Command<M>>,
    /// Register topic subscriptions on the runtime's publish fanout.
    pub subscribe: SubscriberReg<M::Topic, M::Publish>,
}

/// Generic event loop that drives one sans-I/O [`Machine`] paired with its
/// imperative [`Shell`].
///
/// The runtime owns the machine, shell, event and command channels, and the
/// publish fanout. It calls [`Machine::handle`] for timed events and
/// [`Machine::handle_command`] for commands, dispatches the returned actions
/// through the shell, and routes published facts to subscribers.
pub struct ServiceRuntime<M: Machine, S> {
    machine: M,
    shell: S,
    event_tx: mpsc::UnboundedSender<Timed<M::Event>>,
    event_rx: mpsc::UnboundedReceiver<Timed<M::Event>>,
    command_rx: mpsc::UnboundedReceiver<Command<M>>,
    fanout: TopicFanout<M::Topic, M::Publish>,
}

impl<M, S> ServiceRuntime<M, S>
where
    M: Machine,
    S: Shell<Event = M::Event, Action = M::Action>,
{
    fn apply(&mut self, actions: Vec<M::Action>, publish: Vec<M::Publish>) {
        for action in actions {
            self.shell.dispatch(action, &self.event_tx);
        }
        for msg in publish {
            self.fanout.send(&msg);
        }
    }

    /// Runs until both the event and command channels are closed.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                event = self.event_rx.recv() => {
                    let Some(event) = event else { break };
                    let outcome = self.machine.handle(Instant::now(), event);
                    self.apply(outcome.actions, outcome.publish);
                }
                command = self.command_rx.recv() => {
                    let Some((cmd, reply)) = command else { break };
                    let outcome = self.machine.handle_command(Instant::now(), cmd);
                    self.apply(outcome.actions, outcome.publish);
                    let _ = reply.send(outcome.response);
                }
            }
        }
    }
}

/// Builds a machine and shell, spawns the runtime loop, and returns the handles
/// used to drive it plus the task's `JoinHandle`.
pub async fn spawn<M, S>(
    machine_config: M::Config,
    shell_config: S::Config,
) -> (ServiceHandles<M>, JoinHandle<()>)
where
    M: Machine + Send + 'static,
    M::Config: Send,
    S: Shell<Event = M::Event, Action = M::Action> + Send + 'static,
    S::Config: Send,
{
    let now = Instant::now();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (fanout, subscribe) = TopicFanout::new();

    let machine = M::new(machine_config, now);
    let shell = S::new(shell_config, event_tx.clone()).await;

    let runtime = ServiceRuntime {
        machine,
        shell,
        event_tx: event_tx.clone(),
        event_rx,
        command_rx,
        fanout,
    };
    let join = tokio::spawn(runtime.run());

    (
        ServiceHandles {
            events: event_tx,
            commands: command_tx,
            subscribe,
        },
        join,
    )
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde::{Deserialize, Serialize};
    use tokio::sync::{mpsc, oneshot};

    use super::spawn;
    use crate::machine::{CommandOutcome, ExternalCause, Machine, Outcome, Timed};
    use crate::pubsub::HasTopic;
    use crate::shell::Shell;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Ping,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Action {
        Pong,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    enum Topic {
        Beeps,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    enum Publish {
        Beep,
    }

    impl HasTopic<Topic> for Publish {
        fn topic(&self) -> Topic {
            Topic::Beeps
        }
    }

    // A machine that turns every Ping event into a Pong action plus a Beep
    // publish, and answers Ask commands with a counter that increments per call.
    #[derive(Clone, Serialize)]
    struct FakeMachine {
        asks: u32,
    }

    impl Machine for FakeMachine {
        type Config = ();
        type Event = Event;
        type Action = Action;
        type Publish = Publish;
        type Topic = Topic;
        type Command = (); // "Ask"
        type Response = u32;
        type StateSnapshot = Self;

        fn new(_config: Self::Config, _now: Instant) -> Self {
            Self { asks: 0 }
        }

        fn handle(
            &mut self,
            _now: Instant,
            event: Timed<Self::Event>,
        ) -> Outcome<Self::Action, Self::Publish> {
            match event.inner {
                Event::Ping => Outcome {
                    actions: vec![Action::Pong],
                    publish: vec![Publish::Beep],
                },
            }
        }

        fn handle_command(
            &mut self,
            _now: Instant,
            (): Self::Command,
        ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
            self.asks += 1;
            Self::outcome(vec![Action::Pong], self.asks)
        }

        fn state_snapshot(&self) -> Self::StateSnapshot {
            self.clone()
        }
    }

    // A shell that forwards every dispatched action to a test sink so the test
    // can observe side effects deterministically.
    struct FakeShell {
        sink: mpsc::UnboundedSender<Action>,
    }

    impl Shell for FakeShell {
        type Config = mpsc::UnboundedSender<Action>;
        type Event = Event;
        type Action = Action;

        async fn new(
            config: Self::Config,
            _event_tx: mpsc::UnboundedSender<Timed<Self::Event>>,
        ) -> Self {
            Self { sink: config }
        }

        fn dispatch(
            &mut self,
            action: Self::Action,
            _event_tx: &mpsc::UnboundedSender<Timed<Self::Event>>,
        ) {
            let _ = self.sink.send(action);
        }
    }

    #[tokio::test]
    async fn event_produces_action_and_publish() {
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();
        let (handles, _join) = spawn::<FakeMachine, FakeShell>((), action_tx).await;

        let (beep_tx, mut beep_rx) = mpsc::channel(1);
        handles
            .subscribe
            .send((vec![Topic::Beeps], beep_tx))
            .expect("subscriber registration should succeed");

        handles
            .events
            .send(Timed::external(
                Instant::now(),
                ExternalCause::Unknown,
                Event::Ping,
            ))
            .expect("event channel should be open");

        assert_eq!(action_rx.recv().await, Some(Action::Pong));
        assert_eq!(beep_rx.recv().await, Some(Publish::Beep));
    }

    #[tokio::test]
    async fn command_returns_typed_response_and_dispatches_actions() {
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();
        let (handles, _join) = spawn::<FakeMachine, FakeShell>((), action_tx).await;

        let (reply_tx, reply_rx) = oneshot::channel();
        handles
            .commands
            .send(((), reply_tx))
            .expect("command channel should be open");

        assert_eq!(reply_rx.await, Ok(1));
        assert_eq!(action_rx.recv().await, Some(Action::Pong));
    }

    #[test]
    fn fake_machine_state_snapshot_serializes() {
        // §37b: every Machine impl exposes an owned, serializable state
        // snapshot. The FakeMachine's snapshot is itself; after one Ask
        // command the asks counter is visible in the JSON.
        let mut machine = FakeMachine::new((), Instant::now());
        let _ = machine.handle_command(Instant::now(), ());
        let value = serde_json::to_value(machine.state_snapshot())
            .expect("state snapshot should serialize");
        assert_eq!(value["asks"], 1);
    }

    #[tokio::test]
    async fn loop_exits_when_channels_close() {
        let (action_tx, _action_rx) = mpsc::unbounded_channel();
        let (handles, join) = spawn::<FakeMachine, FakeShell>((), action_tx).await;

        drop(handles);

        join.await.expect("runtime task should join cleanly");
    }
}
