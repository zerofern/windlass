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

/// A command paired with its upstream cause and the channel its typed
/// response is returned on.
///
/// The cause is supplied by the sender: the domain shell passes
/// `EventCause::Action(id)` of the domain action that routed the
/// command (read from the task-local dispatch scope), web handlers
/// pass `External(ManualCommand)`.  The runtime records it on the
/// command step so the observability page can answer "which core —
/// and which step — sent this command".
pub type Command<M> = (
    <M as Machine>::Command,
    crate::machine::EventCause,
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
    /// Each `Shell::dispatch` call runs inside a
    /// [`crate::causal::CURRENT_ACTION_ID`] scope keyed to the action
    /// envelope's id.  Shells that spawn HTTP work must re-establish
    /// the scope inside their spawned future via
    /// [`crate::causal::scope`] so the eventual
    /// `HttpTap::observed_exchange` call inside the HTTP client can
    /// read the id and tag the captured exchange.
    fn apply(
        &mut self,
        actions: Vec<ActionEnvelope<M::Action>>,
        publishes: Vec<PublishEnvelope<M::Publish>>,
    ) {
        for env in actions {
            let id = env.id;
            crate::causal::CURRENT_ACTION_ID.sync_scope(Some(id), || {
                self.shell.dispatch(env.payload, &self.event_tx);
            });
        }
        for env in publishes {
            self.fanout.send(&env);
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
                    let event_json = serde_json::to_value(&timed.inner)
                        .unwrap_or(serde_json::Value::Null);
                    let event_variant = crate::machine::variant_name(&event_json).to_owned();

                    self.tap.gate_event(self.core_id, &EventGateView {
                        variant: &event_variant,
                        cause: &event_cause,
                        event: &event_json,
                    }).await;

                    let t0 = Instant::now();
                    let wall_now = Utc::now();
                    let outcome = self.machine.handle(t0, wall_now, timed);
                    let duration = t0.elapsed();

                    let step_id = Uuid::new_v4();
                    // Serialize each action/publish once into a Value so the
                    // variant name extraction and the StepRecord payload share
                    // the same JSON representation.
                    let action_jsons: Vec<serde_json::Value> = outcome
                        .actions
                        .iter()
                        .map(|a| serde_json::to_value(a).unwrap_or(serde_json::Value::Null))
                        .collect();
                    let publish_jsons: Vec<serde_json::Value> = outcome
                        .publishes
                        .iter()
                        .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null))
                        .collect();
                    let publish_topics_owned: Vec<String> = outcome
                        .publishes
                        .iter()
                        .map(|p| {
                            let topic = <M::Publish as crate::pubsub::HasTopic<M::Topic>>::topic(p);
                            let v = serde_json::to_value(&topic)
                                .unwrap_or(serde_json::Value::Null);
                            crate::machine::variant_name(&v).to_owned()
                        })
                        .collect();
                    let action_variants_owned: Vec<String> = action_jsons
                        .iter()
                        .map(|v| crate::machine::variant_name(v).to_owned())
                        .collect();
                    let publish_variants_owned: Vec<String> = publish_jsons
                        .iter()
                        .map(|v| crate::machine::variant_name(v).to_owned())
                        .collect();
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
                    let action_variants: Vec<&str> =
                        action_variants_owned.iter().map(String::as_str).collect();
                    let publish_variants: Vec<&str> =
                        publish_variants_owned.iter().map(String::as_str).collect();
                    let publish_topics: Vec<&str> =
                        publish_topics_owned.iter().map(String::as_str).collect();

                    self.tap.gate_outcome(self.core_id, &OutcomeGateView {
                        source_event_variant: &event_variant,
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
                        event_variant: &event_variant,
                        event: &event_json,
                        event_cause: &event_cause,
                        state_after: &state_json,
                        action_ids: &action_ids,
                        action_variants: &action_variants,
                        action_payloads: &action_jsons,
                        publish_ids: &publish_ids,
                        publish_variants: &publish_variants,
                        publish_payloads: &publish_jsons,
                        publish_topics: &publish_topics,
                    });
                }
                command = self.command_rx.recv() => {
                    let Some((cmd, cause, reply)) = command else { break };
                    // Serialize before `handle_command` consumes the value;
                    // the JSON doubles as the step's event payload and the
                    // source of the variant name.
                    let cmd_json = serde_json::to_value(&cmd)
                        .unwrap_or(serde_json::Value::Null);
                    let cmd_variant = crate::machine::variant_name(&cmd_json).to_owned();
                    let t0 = Instant::now();
                    let wall_now = Utc::now();
                    let outcome = self.machine.handle_command(t0, wall_now, cmd);
                    let duration = t0.elapsed();

                    let step_id = Uuid::new_v4();
                    let action_jsons: Vec<serde_json::Value> = outcome
                        .actions
                        .iter()
                        .map(|a| serde_json::to_value(a).unwrap_or(serde_json::Value::Null))
                        .collect();
                    let publish_jsons: Vec<serde_json::Value> = outcome
                        .publishes
                        .iter()
                        .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null))
                        .collect();
                    let publish_topics_owned: Vec<String> = outcome
                        .publishes
                        .iter()
                        .map(|p| {
                            let topic = <M::Publish as crate::pubsub::HasTopic<M::Topic>>::topic(p);
                            let v = serde_json::to_value(&topic)
                                .unwrap_or(serde_json::Value::Null);
                            crate::machine::variant_name(&v).to_owned()
                        })
                        .collect();
                    let action_variants_owned: Vec<String> = action_jsons
                        .iter()
                        .map(|v| crate::machine::variant_name(v).to_owned())
                        .collect();
                    let publish_variants_owned: Vec<String> = publish_jsons
                        .iter()
                        .map(|v| crate::machine::variant_name(v).to_owned())
                        .collect();
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
                    let action_variants: Vec<&str> =
                        action_variants_owned.iter().map(String::as_str).collect();
                    let publish_variants: Vec<&str> =
                        publish_variants_owned.iter().map(String::as_str).collect();
                    let publish_topics: Vec<&str> =
                        publish_topics_owned.iter().map(String::as_str).collect();

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
                    self.tap.observed_step(self.core_id, &StepRecordView {
                        step_id,
                        core: self.core_id,
                        recorded_at: Utc::now(),
                        duration,
                        kind: StepKind::Command { response: response_status },
                        event_variant: &cmd_variant,
                        event: &cmd_json,
                        event_cause: &cause,
                        state_after: &state_json,
                        action_ids: &action_ids,
                        action_variants: &action_variants,
                        action_payloads: &action_jsons,
                        publish_ids: &publish_ids,
                        publish_variants: &publish_variants,
                        publish_payloads: &publish_jsons,
                        publish_topics: &publish_topics,
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
    use crate::machine::{CommandOutcome, EventCause, ExternalCause, Machine, Outcome, Timed};
    use crate::pubsub::HasTopic;
    use crate::shell::Shell;
    use crate::tap::{
        CoreId, EventGateView, NullRuntimeTap, OutcomeGateView, RuntimeTap, StepKind,
        StepRecordView,
    };

    #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
    enum Event {
        Ping,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    enum Cmd {
        Ask { nonce: u32 },
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
        type Command = Cmd;
        type Response = u32;
        type StateSnapshot = Self;

        fn new(_config: Self::Config, _now: Instant) -> Self {
            Self { asks: 0 }
        }

        fn handle(
            &mut self,
            _now: Instant,
            _wall_now: chrono::DateTime<chrono::Utc>,
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
            _wall_now: chrono::DateTime<chrono::Utc>,
            _cmd: Self::Command,
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
        let envelope = beep_rx.recv().await.expect("beep envelope arrived");
        assert_eq!(envelope.payload, Publish::Beep);
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
            .send((
                Cmd::Ask { nonce: 7 },
                EventCause::External(ExternalCause::ManualCommand),
                reply_tx,
            ))
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
        let _ = machine.handle_command(Instant::now(), chrono::Utc::now(), Cmd::Ask { nonce: 0 });
        let value = serde_json::to_value(machine.state_snapshot())
            .expect("state snapshot should serialize");
        assert_eq!(value["asks"], 1);
    }

    /// Captures every `observed_step` so tests can assert what the
    /// runtime records for the observability layer.
    #[derive(Default)]
    struct RecordingTap {
        steps: std::sync::Mutex<
            Vec<(
                String,
                serde_json::Value,
                crate::machine::StoredEventCause,
                bool,
            )>,
        >,
    }

    #[async_trait::async_trait]
    impl RuntimeTap for RecordingTap {
        async fn gate_event(&self, _core: CoreId, _view: &EventGateView<'_>) {}
        async fn gate_outcome(&self, _core: CoreId, _view: &OutcomeGateView<'_>) {}
        fn reserve_step_ids(
            &self,
            _core: CoreId,
            _step_id: uuid::Uuid,
            _action_ids: &[uuid::Uuid],
            _publish_ids: &[uuid::Uuid],
        ) {
        }
        fn observed_step(&self, _core: CoreId, view: &StepRecordView<'_>) {
            self.steps.lock().unwrap().push((
                view.event_variant.to_owned(),
                view.event.clone(),
                view.event_cause.into(),
                matches!(view.kind, StepKind::Command { .. }),
            ));
        }
    }

    /// The command step must record the real command variant, its
    /// payload, and the sender-supplied cause — not the opaque
    /// `"Command"` / `null` / `manual_command` triple it recorded
    /// before, which made command rows unreadable on the
    /// observability page.
    #[tokio::test]
    async fn command_step_records_variant_payload_and_cause() {
        let tap = std::sync::Arc::new(RecordingTap::default());
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();
        let (handles, _join) =
            spawn::<FakeMachine, FakeShell>(CoreId::Mam, tap.clone(), (), action_tx).await;

        let origin_action = uuid::Uuid::new_v4();
        let (reply_tx, reply_rx) = oneshot::channel();
        handles
            .commands
            .send((
                Cmd::Ask { nonce: 42 },
                EventCause::Action(origin_action),
                reply_tx,
            ))
            .expect("command channel should be open");
        assert_eq!(reply_rx.await, Ok(1));
        // The dispatched action proves the step completed before we
        // inspect the tap.
        assert_eq!(action_rx.recv().await, Some(Action::Pong));

        let steps = tap.steps.lock().unwrap();
        let (variant, payload, cause, is_command) = steps.last().expect("command step recorded");
        assert_eq!(variant, "Ask");
        assert_eq!(payload["Ask"]["nonce"], 42);
        assert_eq!(
            *cause,
            crate::machine::StoredEventCause::Action { id: origin_action }
        );
        assert!(is_command);
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
