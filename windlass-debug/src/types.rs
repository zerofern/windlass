use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;
use uuid::Uuid;
use windlass_core::events::Event;
use windlass_core::types::SystemState;
use windlass_types::HttpExchange;

// ── Event lifecycle ───────────────────────────────────────────────────────────

/// An event that has arrived in the queue and is waiting to be processed.
#[derive(Serialize, Clone, Debug)]
pub struct StoredEvent {
    pub id: Uuid,
    /// The event's own timestamp (`event.at()`).
    pub at: DateTime<Utc>,
    /// When the event entered the intake queue.
    pub arrived_at: DateTime<Utc>,
    pub variant: &'static str,
    /// Full serialised form of the event (sent to frontend; editable via REST).
    pub payload: Value,
    /// Set when the event was produced by an action (Phase 4+).
    pub caused_by_action: Option<Uuid>,
    /// The original event kept for dispatch. Not sent to the frontend.
    #[serde(skip)]
    pub(crate) event: Event,
}

impl StoredEvent {
    /// Returns a reference to the original event (used for dispatch).
    #[must_use]
    pub const fn event(&self) -> &Event {
        &self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn stored_event_event_returns_inner_event() {
        let at = Utc::now();
        let event = Event::Init {
            at,
            is_gluetun_healthy: true,
            port_files: Err("nope".to_string()),
        };
        let stored = StoredEvent {
            id: Uuid::new_v4(),
            at,
            arrived_at: Utc::now(),
            variant: "Init",
            payload: serde_json::Value::Null,
            caused_by_action: None,
            event,
        };
        assert!(matches!(stored.event(), Event::Init { .. }));
    }
}

/// An action that was dispatched but whose result has not yet been recorded.
#[derive(Serialize, Clone, Debug)]
pub struct ActionEntry {
    pub id: Uuid,
    pub variant: &'static str,
    pub payload: Value,
    pub parent_event_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Set when this action produced a result event (Phase 4+).
    pub caused_event_id: Option<Uuid>,
    /// HTTP exchanges captured while this action was running (Phase 6+).
    pub http_exchanges: Vec<HttpExchange>,
}

/// The event currently being processed, including the actions it has produced so far.
#[derive(Serialize, Clone, Debug)]
pub struct ActiveEvent {
    pub stored: StoredEvent,
    pub state_before: SystemState,
    pub started_at: DateTime<Utc>,
    /// Actions that have already been dispatched (started).
    pub actions: Vec<ActionEntry>,
    /// Actions still waiting to be dispatched (serialised payloads only).
    /// Populated before dispatch begins; each entry is removed when the
    /// corresponding action transitions to `actions`.
    pub pending_actions: Vec<Value>,
}

/// An async action that has been dispatched but not yet completed.
/// Stays in `running_actions` until Phase 4 wires in `CausalTx` completion.
#[derive(Serialize, Clone, Debug)]
pub struct RunningAction {
    pub id: Uuid,
    pub variant: &'static str,
    pub payload: Value,
    pub parent_event_id: Uuid,
    pub started_at: DateTime<Utc>,
}

/// A completed event stored in the scrollable trace.
#[derive(Serialize, Clone, Debug)]
pub struct TraceEntry {
    pub event: StoredEvent,
    pub state_before: SystemState,
    pub state_after: SystemState,
    pub actions: Vec<ActionEntry>,
    pub completed_at: DateTime<Utc>,
}

// ── Logs ──────────────────────────────────────────────────────────────────────

/// A single log line captured from a `tracing` macro (Phase 5+).
#[derive(Serialize, Clone, Debug)]
pub struct LogEntry {
    pub at: DateTime<Utc>,
    pub level: String,
    pub target: String,
    pub message: String,
}

// ── Queue commands ────────────────────────────────────────────────────────────

/// Commands sent by HTTP handlers to the main loop for queue manipulation.
/// Implemented in full by `DebugHistory::apply_cmd`; REST endpoints wired in Phase 3.
pub enum DebugCommand {
    RemoveQueuedEvent(Uuid),
    EditQueuedEvent(Uuid, Value, oneshot::Sender<Result<(), String>>),
    InjectEvent {
        payload: Value,
        position: Option<usize>,
        at: DateTime<Utc>,
        reply: oneshot::Sender<Result<Uuid, String>>,
    },
    ReorderQueue(Vec<Uuid>, oneshot::Sender<Result<(), String>>),
}
