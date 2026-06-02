use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::machine::{ActionEnvelope, Machine, PublishEnvelope, Timed};
use crate::pubsub::{SubscriberReg, TopicFanout};
use crate::shell::Shell;
use crate::tap::{
    CommandResponseStatus, CoreId, EventGateView, OutcomeGateView, RuntimeTap, StepKind,
    StepRecordView,
};

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
/// The runtime owns the machine, shell, event and command channels, the
/// publish fanout, and an `Arc<dyn RuntimeTap>` for observability gating
/// and recording. See `docs/observability-redesign.md` for the loop
/// shape and engineering contracts.
pub struct ServiceRuntime<M: Machine, S> {
    core_id: CoreId,
    machine: M,
    shell: S,
    event_tx: mpsc::UnboundedSender<Timed<M::Event>>,
    event_rx: mpsc::UnboundedReceiver<Timed<M::Event>>,
    command_rx: mpsc::UnboundedReceiver<Command<M>>,
    fanout: TopicFanout<M::Topic, M::Publish>,
    tap: Arc<dyn RuntimeTap>,
}

impl<M, S> ServiceRuntime<M, S>
where
    M: Machine,
    S: Shell<Event = M::Event, Action = M::Action>,
{
    /// Apply one outcome: dispatch each action envelope through the
    /// shell, fan each publish envelope out through `TopicFanout`.
    ///
    /// Action IDs ride along through `CausalTx` in §37e (HTTP tap).
    /// Publish IDs ride along through `TopicFanout` once it switches
    /// to envelope-aware sends (planned follow-up to §37d under C2).
    fn apply(
        &mut self,
        actions: Vec<ActionEnvelope<M::Action>>,
        publishes: Vec<PublishEnvelope<M::Publish>>,
    ) {
        for env in actions {
            self.shell.dispatch(env.payload, &self.event_tx);
        }
        for env in publishes {
            self.fanout.send(&env.payload);
        }
    }

    /// Runs until both the event and command channels are closed.
    #[allow(clippy::too_many_lines)]
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                event = self.event_rx.recv() => {
                    let Some(timed) = event else { break };
                    let event_cause = timed.cause.clone();
                    // §37f will introduce a Serialize bound on M::Event
                    // and replace this placeholder with the real
                    // serialized payload + extracted variant name.
                    let event_json = serde_json::Value::Null;

                    self.tap.gate_event(self.core_id, &EventGateView {
                        variant: "?",
                        cause: &event_cause,
                        event: &event_json,
                    }).await;

                    let t0 = Instant::now();
                    let outcome = self.machine.handle(t0, timed);
                    let duration = t0.elapsed();

                    let step_id = Uuid::new_v4();
                    let actions: Vec<ActionEnvelope<M::Action>> = outcome
                        .actions
                        .into_iter()
                        .map(|payload| ActionEnvelope { id: Uuid::new_v4(), payload })
                        .collect();
                    let publishes: Vec<PublishEnvelope<M::Publish>> = outcome
                        .publishes
                        .into_iter()
                        .map(|payload| PublishEnvelope { id: Uuid::new_v4(), payload })
                        .collect();
                    let action_ids: Vec<Uuid> = actions.iter().map(|e| e.id).collect();
                    let publish_ids: Vec<Uuid> = publishes.iter().map(|e| e.id).collect();
                    // §37f populates the variant-name slices once
                    // M::Action / M::Publish gain a name accessor.
                    let action_variants: Vec<&str> = action_ids.iter().map(|_| "?").collect();
                    let publish_variants: Vec<&str> = publish_ids.iter().map(|_| "?").collect();

                    self.tap.gate_outcome(self.core_id, &OutcomeGateView {
                        source_event_variant: "?",
                        action_variants: &action_variants,
                        action_ids: &action_ids,
                        publish_variants: &publish_variants,
                        publish_ids: &publish_ids,
                    }).await;

                    self.tap.reserve_step_ids(
                        self.core_id,
                        step_id,
                        &action_ids,
                        &publish_ids,
                    );

                    self.apply(actions, publishes);

                    let snapshot = self.machine.state_snapshot();
                    let state_json = serde_json::to_value(snapshot)
                        .unwrap_or(serde_json::Value::Null);
                    self.tap.observed_step(self.core_id, &StepRecordView {
                        step_id,
                        core: self.core_id,
                        recorded_at: Utc::now(),
                        duration,
                        kind: StepKind::Event,
                        event_variant: "?",
                        event: &event_json,
                        event_cause: &event_cause,
                        state_after: &state_json,
                        action_ids: &action_ids,
                        action_variants: &action_variants,
                        publish_ids: &publish_ids,
                        publish_variants: &publish_variants,
                    });
                }
                command = self.command_rx.recv() => {
                    let Some((cmd, reply)) = command else { break };
                    // Command bodies are opaque to the gate views in
                    // §37d; the kind tag is enough to distinguish them
                    // in the step record stream.
                    let t0 = Instant::now();
                    let outcome = self.machine.handle_command(t0, cmd);
                    let duration = t0.elapsed();

                    let step_id = Uuid::new_v4();
                    let actions: Vec<ActionEnvelope<M::Action>> = outcome
                        .actions
                        .into_iter()
                        .map(|payload| ActionEnvelope { id: Uuid::new_v4(), payload })
                        .collect();
                    let publishes: Vec<PublishEnvelope<M::Publish>> = outcome
                        .publishes
                        .into_iter()
                        .map(|payload| PublishEnvelope { id: Uuid::new_v4(), payload })
                        .collect();
                    let action_ids: Vec<Uuid> = actions.iter().map(|e| e.id).collect();
                    let publish_ids: Vec<Uuid> = publishes.iter().map(|e| e.id).collect();
                    let action_variants: Vec<&str> = action_ids.iter().map(|_| "?").collect();
                    let publish_variants: Vec<&str> = publish_ids.iter().map(|_| "?").collect();

                    self.tap.reserve_step_ids(
                        self.core_id,
                        step_id,
                        &action_ids,
                        &publish_ids,
                    );

                    self.apply(actions, publishes);

                    let response_status = if reply.send(outcome.response).is_ok() {
                        CommandResponseStatus::Sent
                    } else {
                        CommandResponseStatus::ReceiverDropped
                    };

                    let snapshot = self.machine.state_snapshot();
                    let state_json = serde_json::to_value(snapshot)
                        .unwrap_or(serde_json::Value::Null);
                    // Commands don't carry an upstream Timed::cause; use
                    // External(ManualCommand) as a stand-in until §37f
                    // distinguishes "domain command" causes from
                    // "operator command" causes.
                    let cause = crate::machine::EventCause::External(
                        crate::machine::ExternalCause::ManualCommand,
                    );
                    self.tap.observed_step(self.core_id, &StepRecordView {
                        step_id,
                        core: self.core_id,
                        recorded_at: Utc::now(),
                        duration,
                        kind: StepKind::Command { response: response_status },
                        event_variant: "?",
                        event: &serde_json::Value::Null,
                        event_cause: &cause,
                        state_after: &state_json,
                        action_ids: &action_ids,
                        action_variants: &action_variants,
                        publish_ids: &publish_ids,
                        publish_variants: &publish_variants,
                    });
                }
            }
        }
    }
}

/// Builds a machine and shell, spawns the runtime loop, and returns the handles
/// used to drive it plus the task's `JoinHandle`.
///
/// `tap` is the observability hook; pass [`crate::tap::NullRuntimeTap`]
/// when observability is not needed, or an
/// `Arc<windlass_observability::ObservabilityController>` to attach
/// the live backend.
pub async fn spawn<M, S>(
    core_id: CoreId,
    tap: Arc<dyn RuntimeTap>,
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
        core_id,
        machine,
        shell,
        event_tx: event_tx.clone(),
        event_rx,
        command_rx,
        fanout,
        tap,
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
    use crate::tap::{CoreId, NullRuntimeTap};

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
                    publishes: vec![Publish::Beep],
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
        let (handles, _join) =
            spawn::<FakeMachine, FakeShell>(CoreId::Vpn, NullRuntimeTap::arc(), (), action_tx)
                .await;

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
        let (handles, _join) =
            spawn::<FakeMachine, FakeShell>(CoreId::Vpn, NullRuntimeTap::arc(), (), action_tx)
                .await;

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
        let (handles, join) =
            spawn::<FakeMachine, FakeShell>(CoreId::Vpn, NullRuntimeTap::arc(), (), action_tx)
                .await;

        drop(handles);

        join.await.expect("runtime task should join cleanly");
    }
}
