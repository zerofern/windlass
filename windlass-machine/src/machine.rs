use std::path::PathBuf;
use std::time::Instant;

use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::pubsub::HasTopic;

/// Why an event arrived — the upstream side effect that produced it.
///
/// The observability layer uses this to render the bidirectional causal
/// graph: an event's cause points back at the action or publish that
/// produced it; clicking that node jumps to the originating step in
/// whichever core emitted it. See `docs/observability-redesign.md`
/// "Architecture / `Timed<E>` causal extension".
///
/// This is the runtime-side enum. The wire-side counterpart
/// (`StoredEventCause`) lands in §37f with the rest of the SSE shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventCause {
    /// Event is the result of an action this or another core dispatched.
    Action(Uuid),
    /// Event is the result of a publish from another core.
    Publish(Uuid),
    /// Event originates outside the action/publish graph.
    External(ExternalCause),
}

/// What kind of external source produced an event.
///
/// Runtime-side only — uses zero-copy variants where it can
/// (`&'static str` for timer / Docker-event names). The wire-side
/// `StoredExternalCause` (everything as `String`) lands in §37f.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExternalCause {
    /// A timer fired. `name` identifies the timer (e.g. `"VpnHealthPoll"`).
    Timer { name: &'static str },
    /// A file-watcher event arrived.
    FileWatcher { path: PathBuf },
    /// A Docker engine event arrived. `kind` is the event class
    /// (e.g. `"container.start"`, `"container.health_status"`).
    DockerEvent { kind: &'static str },
    /// An operator-initiated command (web UI button, CLI invocation).
    ManualCommand,
    /// System boot — the first event each core emits during init.
    Init,
    /// The cause is not yet wired up. Used as a placeholder during the
    /// §37 migration; every `Unknown` site should be replaced before
    /// observability ships.
    Unknown,
}

/// An event paired with the logical time it occurred and the upstream
/// cause that produced it.
///
/// For timers, `at` should be the scheduled fire time, not the wall-clock
/// time when an async runtime woke up. Machines can then compare it
/// against a fresh `Instant::now()` to reason about scheduler slack and
/// event-queue lag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timed<E> {
    pub at: Instant,
    pub cause: EventCause,
    pub inner: E,
}

impl<E> Timed<E> {
    /// Event arrived as the result of action `action_id` completing.
    /// Used by shells at I/O completion sites.
    #[must_use]
    pub const fn from_action(at: Instant, action_id: Uuid, inner: E) -> Self {
        Self {
            at,
            cause: EventCause::Action(action_id),
            inner,
        }
    }

    /// Event arrived because a subscribed publish `publish_id` fired in
    /// another core. Used by cross-core subscriber bridges.
    #[must_use]
    pub const fn from_publish(at: Instant, publish_id: Uuid, inner: E) -> Self {
        Self {
            at,
            cause: EventCause::Publish(publish_id),
            inner,
        }
    }

    /// Event originates outside the action/publish graph — timer fire,
    /// file watcher, Docker watcher, manual command, init, etc.
    #[must_use]
    pub const fn external(at: Instant, cause: ExternalCause, inner: E) -> Self {
        Self {
            at,
            cause: EventCause::External(cause),
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

    /// An owned, serializable snapshot of the machine's internal state.
    ///
    /// `Send + 'static` is required because the observability controller
    /// hands the snapshot to a serialization worker on a separate task;
    /// see §37 / `docs/observability-redesign.md` "Architecture / `Machine`
    /// trait extension".
    type StateSnapshot: Serialize + Send + 'static;

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

    /// Returns an owned snapshot of the machine's current state.
    ///
    /// Called by the observability controller after every `handle` /
    /// `handle_command` invocation; the serialized snapshot is attached
    /// to the resulting `StepRecord`.
    fn state_snapshot(&self) -> Self::StateSnapshot;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_action_sets_action_cause() {
        let id = Uuid::new_v4();
        let t = Timed::from_action(Instant::now(), id, 42_u32);
        assert_eq!(t.cause, EventCause::Action(id));
        assert_eq!(t.inner, 42);
    }

    #[test]
    fn from_publish_sets_publish_cause() {
        let id = Uuid::new_v4();
        let t = Timed::from_publish(Instant::now(), id, "hello");
        assert_eq!(t.cause, EventCause::Publish(id));
        assert_eq!(t.inner, "hello");
    }

    #[test]
    fn external_wraps_inner_cause() {
        let t = Timed::external(
            Instant::now(),
            ExternalCause::Timer { name: "TestTimer" },
            (),
        );
        assert_eq!(
            t.cause,
            EventCause::External(ExternalCause::Timer { name: "TestTimer" })
        );
    }

    #[test]
    fn distinct_action_ids_compare_unequal() {
        let a = EventCause::Action(Uuid::new_v4());
        let b = EventCause::Action(Uuid::new_v4());
        assert_ne!(a, b);
    }

    #[test]
    fn external_unknown_round_trips() {
        let c = EventCause::External(ExternalCause::Unknown);
        assert_eq!(c, EventCause::External(ExternalCause::Unknown));
        assert_ne!(c, EventCause::External(ExternalCause::Init));
    }
}
