use std::time::Instant;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::pubsub::HasTopic;

/// An event paired with the logical time it occurred.
///
/// For timers, `at` should be the scheduled fire time, not the wall-clock time
/// when an async runtime woke up. Machines can then compare it against a fresh
/// `Instant::now()` to reason about scheduler slack and event-queue lag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timed<E> {
    pub at: Instant,
    pub inner: E,
}

impl<E> Timed<E> {
    /// Pairs `inner` with an explicit logical time.
    #[must_use]
    pub const fn new(at: Instant, inner: E) -> Self {
        Self { at, inner }
    }

    /// Pairs `inner` with the current wall-clock instant.
    ///
    /// Use this for I/O completion events, where "when it happened" is the
    /// moment the external result was observed. Timers should prefer
    /// [`Timed::new`] with the scheduled fire time.
    #[must_use]
    pub fn now(inner: E) -> Self {
        Self {
            at: Instant::now(),
            inner,
        }
    }
}

/// The side-effect requests and publish messages produced by one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome<A, P> {
    pub actions: Vec<A>,
    pub publish: Vec<P>,
}

impl<A, P> Outcome<A, P> {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            actions: Vec::new(),
            publish: Vec::new(),
        }
    }
}

/// The result of one external command, including the synchronous response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome<A, P, R> {
    pub actions: Vec<A>,
    pub publish: Vec<P>,
    pub response: R,
}

/// A pure, side-effect-free state machine.
///
/// The machine owns state and decisions. Shells execute returned actions and
/// report I/O results back as events.
pub trait Machine: Sized {
    type Config;
    type Event: Send + 'static;
    type Action: Send + 'static;
    type Publish: HasTopic<Self::Topic> + Serialize + Clone + Send + 'static;
    type Topic: DeserializeOwned + PartialEq + Clone + Send + 'static;
    type Command: DeserializeOwned + Send + 'static;
    type Response: Serialize + Send + 'static;

    fn new(config: Self::Config, now: Instant) -> Self;

    /// Handles one observed event.
    ///
    /// `now` is a fresh `Instant::now()` captured when the runtime dequeued the
    /// event; `event.at` is the logical time the event happened. The difference
    /// lets a machine reason about queue lag and timer slack without doing I/O.
    fn handle(
        &mut self,
        now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish>;

    fn handle_command(
        &mut self,
        now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response>;

    fn outcome(
        actions: Vec<Self::Action>,
        response: Self::Response,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        CommandOutcome {
            actions,
            publish: Vec::new(),
            response,
        }
    }

    fn outcome_with_publish(
        actions: Vec<Self::Action>,
        publish: Vec<Self::Publish>,
        response: Self::Response,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        CommandOutcome {
            actions,
            publish,
            response,
        }
    }
}
