use std::collections::VecDeque;

use chrono::Utc;
use uuid::Uuid;
use windlass_core::actions::Action;
use windlass_core::events::Event;
use windlass_core::types::SystemState;

use crate::stream::{action_variant, event_variant};
use crate::types::{
    ActionEntry, ActiveEvent, DebugCommand, LogEntry, RunningAction, StoredEvent, TraceEntry,
};

const TRACE_CAP: usize = 200;
const LOG_CAP: usize = 500;

// ── DebugHistory ──────────────────────────────────────────────────────────────

/// The mutable history half of the debug system — owned exclusively by the
/// main event loop. All methods take `&mut self`; no locking required.
///
/// HTTP handlers communicate via `DebugCommand` channels; the main loop drains
/// them and calls `apply_cmd`. Reads are served from an `ArcSwap` snapshot
/// that the main loop publishes after each mutation.
pub struct DebugHistory {
    /// Monotonic counter incremented on every mutation, embedded in snapshots
    /// so the frontend can discard stale SSE events.
    pub(crate) seq: u64,
    /// Events that have arrived but not yet started processing.
    pub(crate) event_queue: VecDeque<StoredEvent>,
    /// The event currently being processed.
    pub(crate) current_event: Option<ActiveEvent>,
    /// Actions that have been dispatched but not yet completed.
    pub(crate) running_actions: Vec<RunningAction>,
    /// Completed events, capped at [`TRACE_CAP`].
    pub(crate) trace: VecDeque<TraceEntry>,
    /// Captured log lines, capped at [`LOG_CAP`].
    pub(crate) logs: VecDeque<LogEntry>,
    /// The state after the most-recently completed event. Initialised to
    /// `SystemState::initial()` so `latest_state()` is always valid.
    pub(crate) latest_state: SystemState,
}

impl DebugHistory {
    pub fn new(initial_state: SystemState) -> Self {
        Self {
            seq: 0,
            event_queue: VecDeque::new(),
            current_event: None,
            running_actions: Vec::new(),
            trace: VecDeque::new(),
            logs: VecDeque::new(),
            latest_state: initial_state,
        }
    }

    // ── Event lifecycle ───────────────────────────────────────────────────────

    /// Records that an event has arrived in the intake queue. Returns its ID.
    pub fn event_arrived(&mut self, event: &Event, caused_by: Option<Uuid>) -> Uuid {
        let id = Uuid::new_v4();
        self.event_queue.push_back(StoredEvent {
            id,
            at: event.at(),
            arrived_at: Utc::now(),
            variant: event_variant(event),
            payload: serde_json::to_value(event).unwrap_or(serde_json::Value::Null),
            caused_by_action: caused_by,
        });
        self.seq += 1;
        id
    }

    /// Moves the identified event from the queue to `current_event`, recording
    /// the state snapshot taken before processing begins.
    pub fn event_started(&mut self, event_id: Uuid, state_before: SystemState) {
        let pos = self.event_queue.iter().position(|e| e.id == event_id);
        if let Some(pos) = pos {
            let stored = self.event_queue.remove(pos).unwrap();
            self.current_event = Some(ActiveEvent {
                stored,
                state_before,
                started_at: Utc::now(),
                actions: Vec::new(),
            });
            self.seq += 1;
        }
    }

    // ── Action lifecycle ──────────────────────────────────────────────────────

    /// Records that an action has been dispatched. Returns the action's ID.
    pub fn action_started(&mut self, action: &Action, parent_event_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        let variant = action_variant(action);
        let payload = serde_json::to_value(action).unwrap_or(serde_json::Value::Null);

        if let Some(current) = &mut self.current_event {
            current.actions.push(ActionEntry {
                id,
                variant,
                payload: payload.clone(),
                parent_event_id,
                started_at: Utc::now(),
                completed_at: None,
                caused_event_id: None,
            });
        }
        self.running_actions.push(RunningAction {
            id,
            variant,
            payload,
            parent_event_id,
            started_at: Utc::now(),
        });
        self.seq += 1;
        id
    }

    /// Records that an async action has completed (wired fully in Phase 4).
    pub fn action_completed(&mut self, action_id: Uuid, caused_event_id: Option<Uuid>) {
        self.running_actions.retain(|a| a.id != action_id);
        if let Some(current) = &mut self.current_event {
            if let Some(entry) = current.actions.iter_mut().find(|a| a.id == action_id) {
                entry.completed_at = Some(Utc::now());
                entry.caused_event_id = caused_event_id;
            }
        }
        self.seq += 1;
    }

    /// Finalises the current event: updates `latest_state` and appends to trace.
    pub fn event_completed(&mut self, event_id: Uuid, state_after: SystemState) {
        self.latest_state = state_after.clone();
        if let Some(active) = self.current_event.take() {
            if active.stored.id == event_id {
                if self.trace.len() >= TRACE_CAP {
                    self.trace.pop_front();
                }
                self.trace.push_back(TraceEntry {
                    event: active.stored,
                    state_before: active.state_before,
                    state_after,
                    actions: active.actions,
                    completed_at: Utc::now(),
                });
            }
        }
        self.seq += 1;
    }

    // ── Logs ──────────────────────────────────────────────────────────────────

    pub fn append_log(&mut self, entry: LogEntry) {
        if self.logs.len() >= LOG_CAP {
            self.logs.pop_front();
        }
        self.logs.push_back(entry);
        self.seq += 1;
    }

    // ── Queue commands ────────────────────────────────────────────────────────

    /// Applies a queue-manipulation command from an HTTP handler.
    /// Full Event injection/editing requires `Event: Deserialize` (Phase 3).
    pub fn apply_cmd(&mut self, cmd: DebugCommand) {
        match cmd {
            DebugCommand::RemoveQueuedEvent(id) => {
                self.event_queue.retain(|e| e.id != id);
                self.seq += 1;
            }
            DebugCommand::EditQueuedEvent(_id, _payload, reply) => {
                let _ = reply.send(Err("EditQueuedEvent available from Phase 3".to_string()));
            }
            DebugCommand::InjectEvent { reply, .. } => {
                let _ = reply.send(Err("InjectEvent available from Phase 3".to_string()));
            }
            DebugCommand::ReorderQueue(ids, reply) => {
                if ids.len() != self.event_queue.len() {
                    let _ = reply.send(Err(format!(
                        "ID count mismatch: got {} but queue has {}",
                        ids.len(),
                        self.event_queue.len()
                    )));
                    return;
                }
                let mut new_queue = VecDeque::with_capacity(ids.len());
                for id in &ids {
                    match self.event_queue.iter().position(|e| &e.id == id) {
                        Some(pos) => new_queue.push_back(self.event_queue.remove(pos).unwrap()),
                        None => {
                            let _ = reply.send(Err(format!("Unknown event ID: {id}")));
                            return;
                        }
                    }
                }
                self.event_queue = new_queue;
                self.seq += 1;
                let _ = reply.send(Ok(()));
            }
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// Returns the latest known system state. Always valid — initialised to
    /// `SystemState::initial()` before the first event is processed.
    #[must_use]
    pub fn latest_state(&self) -> &SystemState {
        &self.latest_state
    }
}
