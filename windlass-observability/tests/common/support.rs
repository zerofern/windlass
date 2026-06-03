//! Shared fixtures for the §37pre acceptance tests.
//!
//! `TinyMachine` is the smallest Machine that exercises every part of
//! the trait: one event variant, one action variant, one publish
//! variant, one command variant.  Every acceptance test uses it so
//! the tests stay focused on the controller behavior rather than
//! production-machine semantics.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use windlass_machine::{
    ActionEnvelope, CommandOutcome, HasTopic, Machine, Outcome, PublishEnvelope, Shell, Timed,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum TinyEvent {
    Ping,
    /// Used by acceptance test #1 (fanout-bridge harness) — the
    /// subscriber bridge turns a `Beep` publish into a `BeepHeard`
    /// event on the consuming core, preserving `publish_id`.
    BeepHeard,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum TinyAction {
    Pong,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TinyTopic {
    Beeps,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TinyPublish {
    Beep,
}

impl HasTopic<TinyTopic> for TinyPublish {
    fn topic(&self) -> TinyTopic {
        TinyTopic::Beeps
    }
}

#[derive(Clone, Serialize)]
pub struct TinyMachine;

impl Machine for TinyMachine {
    type Config = ();
    type Event = TinyEvent;
    type Action = TinyAction;
    type Publish = TinyPublish;
    type Topic = TinyTopic;
    type Command = ();
    type Response = ();
    type StateSnapshot = ();

    fn new((): (), _now: Instant) -> Self {
        Self
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            TinyEvent::Ping => Outcome {
                actions: vec![TinyAction::Pong],
                publishes: vec![TinyPublish::Beep],
            },
            // BeepHeard is purely an observed event — the subscriber
            // bridge in test #1 needs *some* event to convert to.
            TinyEvent::BeepHeard => Outcome {
                actions: Vec::new(),
                publishes: Vec::new(),
            },
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        (): Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        Self::outcome(Vec::new(), ())
    }

    fn state_snapshot(&self) {}
}

pub struct TinyShell {
    sink: mpsc::UnboundedSender<TinyAction>,
}

impl Shell for TinyShell {
    type Config = mpsc::UnboundedSender<TinyAction>;
    type Event = TinyEvent;
    type Action = TinyAction;

    async fn new(
        config: Self::Config,
        _event_tx: tokio::sync::mpsc::UnboundedSender<Timed<Self::Event>>,
    ) -> Self {
        Self { sink: config }
    }

    fn dispatch(
        &mut self,
        action: Self::Action,
        _event_tx: &tokio::sync::mpsc::UnboundedSender<Timed<Self::Event>>,
    ) {
        let _ = self.sink.send(action);
    }
}

// Make sure the envelope re-exports stay live; the bridge harness
// consumes them via the public surface only.
#[allow(dead_code)]
pub fn _envelopes_in_scope(_: ActionEnvelope<TinyAction>, _: PublishEnvelope<TinyPublish>) {}
