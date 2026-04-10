use std::collections::VecDeque;

use chrono::Utc;
use uuid::Uuid;
use windlass_core::actions::Action;
use windlass_core::events::Event;
use windlass_core::types::SystemState;
use windlass_types::HttpExchange;

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

    /// Pushes an already-constructed `StoredEvent` into the queue.
    /// Called by the main loop when draining `queue_rx` in debug mode.
    pub fn push_stored_event(&mut self, stored: StoredEvent) {
        self.event_queue.push_back(stored);
        self.seq += 1;
    }

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
            event: event.clone(),
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
                pending_actions: Vec::new(),
            });
            self.seq += 1;
        }
    }

    /// Sets `current_event` from an already-removed `StoredEvent`.
    /// Used by the Phase 3+ queue path where the shell loop pops the event
    /// from the front of `event_queue` before calling this.
    pub fn event_started_stored(&mut self, stored: StoredEvent, state_before: SystemState) {
        self.current_event = Some(ActiveEvent {
            stored,
            state_before,
            started_at: Utc::now(),
            actions: Vec::new(),
            pending_actions: Vec::new(),
        });
        self.seq += 1;
    }

    // ── Action lifecycle ──────────────────────────────────────────────────────

    /// Records the full action list before dispatch begins, so the frontend
    /// can show all upcoming actions even before they are stepped through.
    /// Each call to `action_started` will pop the front entry from this list.
    pub fn actions_ready(&mut self, actions: &[Action]) {
        if let Some(current) = &mut self.current_event {
            current.pending_actions = actions
                .iter()
                .map(|a| serde_json::to_value(a).unwrap_or(serde_json::Value::Null))
                .collect();
        }
        self.seq += 1;
    }

    /// Records that an action has been dispatched. Returns the action's ID.
    pub fn action_started(&mut self, action: &Action, parent_event_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        let variant = action_variant(action);
        let payload = serde_json::to_value(action).unwrap_or(serde_json::Value::Null);

        if let Some(current) = &mut self.current_event {
            // Remove the front pending entry now that this action has started.
            if !current.pending_actions.is_empty() {
                current.pending_actions.remove(0);
            }
            current.actions.push(ActionEntry {
                id,
                variant,
                payload: payload.clone(),
                parent_event_id,
                started_at: Utc::now(),
                completed_at: None,
                caused_event_id: None,
                http_exchanges: Vec::new(),
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

    /// Records that an async action has completed and links the causal result
    /// event to its `ActionEntry`.
    ///
    /// The action may have completed after its parent event was already moved
    /// to the trace (the typical case for causal actions), so this searches
    /// `current_event` first and then walks the trace backwards.
    pub fn action_completed(&mut self, action_id: Uuid, caused_event_id: Option<Uuid>) {
        self.running_actions.retain(|a| a.id != action_id);
        if let Some(current) = &mut self.current_event {
            if let Some(entry) = current.actions.iter_mut().find(|a| a.id == action_id) {
                entry.completed_at = Some(Utc::now());
                entry.caused_event_id = caused_event_id;
                self.seq += 1;
                return;
            }
        }
        // Action completed after its parent event was finalised — update in trace.
        for trace_entry in self.trace.iter_mut().rev() {
            if let Some(entry) = trace_entry.actions.iter_mut().find(|a| a.id == action_id) {
                entry.completed_at = Some(Utc::now());
                entry.caused_event_id = caused_event_id;
                break;
            }
        }
        self.seq += 1;
    }

    /// Records a causal event — produced by a running action — into the queue.
    ///
    /// Returns the assigned event ID so the caller can immediately invoke
    /// `action_completed(action_id, Some(event_id))` to link them.
    pub fn push_causal_event(&mut self, event: Event, caused_by: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        self.event_queue.push_back(StoredEvent {
            id,
            at: event.at(),
            arrived_at: Utc::now(),
            variant: event_variant(&event),
            payload: serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
            caused_by_action: Some(caused_by),
            event,
        });
        self.seq += 1;
        id
    }

    /// Attaches an HTTP exchange to the action that made the call.
    ///
    /// Searches `current_event.actions` first, then walks the trace backwards,
    /// since the action may have completed before its exchange is processed.
    pub fn action_http_exchange(&mut self, action_id: Uuid, exchange: HttpExchange) {
        if let Some(current) = &mut self.current_event {
            if let Some(entry) = current.actions.iter_mut().find(|a| a.id == action_id) {
                entry.http_exchanges.push(exchange);
                self.seq += 1;
                return;
            }
        }
        for trace_entry in self.trace.iter_mut().rev() {
            if let Some(entry) = trace_entry.actions.iter_mut().find(|a| a.id == action_id) {
                entry.http_exchanges.push(exchange);
                self.seq += 1;
                return;
            }
        }
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
    pub fn apply_cmd(&mut self, cmd: DebugCommand) {
        match cmd {
            DebugCommand::RemoveQueuedEvent(id) => {
                self.event_queue.retain(|e| e.id != id);
                self.seq += 1;
            }
            DebugCommand::EditQueuedEvent(id, payload, reply) => {
                match self.event_queue.iter_mut().find(|e| e.id == id) {
                    None => {
                        let _ = reply.send(Err(format!("Event {id} not found in queue")));
                    }
                    Some(stored) => match serde_json::from_value::<Event>(payload.clone()) {
                        Err(e) => {
                            let _ = reply.send(Err(format!("Invalid payload: {e}")));
                        }
                        Ok(event) => {
                            stored.variant = event_variant(&event);
                            stored.payload = payload;
                            stored.event = event;
                            self.seq += 1;
                            let _ = reply.send(Ok(()));
                        }
                    },
                }
            }
            DebugCommand::InjectEvent {
                payload,
                position,
                at,
                reply,
            } => match serde_json::from_value::<Event>(payload.clone()) {
                Err(e) => {
                    let _ = reply.send(Err(format!("Invalid payload: {e}")));
                }
                Ok(event) => {
                    let id = uuid::Uuid::new_v4();
                    let stored = StoredEvent {
                        id,
                        at,
                        arrived_at: Utc::now(),
                        variant: event_variant(&event),
                        payload,
                        caused_by_action: None,
                        event,
                    };
                    let pos = position.unwrap_or(self.event_queue.len());
                    let pos = pos.min(self.event_queue.len());
                    self.event_queue.insert(pos, stored);
                    self.seq += 1;
                    let _ = reply.send(Ok(id));
                }
            },
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

    // ── Queue accessors (for the shell's debug loop) ──────────────────────────

    /// Returns `true` when no events are waiting in the queue.
    #[must_use]
    pub fn queue_is_empty(&self) -> bool {
        self.event_queue.is_empty()
    }

    /// Returns the variant name of the front queued event, or `None` if empty.
    #[must_use]
    pub fn queue_front_variant(&self) -> Option<&'static str> {
        self.event_queue.front().map(|e| e.variant)
    }

    /// Removes and returns the front queued event, or `None` if empty.
    pub fn pop_queue_front(&mut self) -> Option<StoredEvent> {
        self.event_queue.pop_front()
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// Returns the latest known system state. Always valid — initialised to
    /// `SystemState::initial()` before the first event is processed.
    #[must_use]
    pub fn latest_state(&self) -> &SystemState {
        &self.latest_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tokio::sync::oneshot;
    use windlass_core::events::Event;
    use windlass_types::HttpExchange;

    fn initial_state() -> SystemState {
        SystemState::initial()
    }

    fn make_init_event() -> Event {
        Event::Init {
            at: Utc::now(),
            is_gluetun_healthy: true,
            port_files: Err("not ready".to_string()),
        }
    }

    fn push_one_event(h: &mut DebugHistory) -> Uuid {
        h.event_arrived(&make_init_event(), None)
    }

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn new_history_starts_empty() {
        let h = DebugHistory::new(initial_state());
        assert_eq!(h.seq, 0);
        assert!(h.event_queue.is_empty());
        assert!(h.current_event.is_none());
        assert!(h.running_actions.is_empty());
        assert!(h.trace.is_empty());
        assert!(h.logs.is_empty());
    }

    // ── Event queue ───────────────────────────────────────────────────────────

    #[test]
    fn event_arrived_adds_to_queue_and_bumps_seq() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        assert_eq!(h.event_queue.len(), 1);
        assert_eq!(h.event_queue[0].id, id);
        assert_eq!(h.seq, 1);
    }

    #[test]
    fn queue_is_empty_and_front_variant_reflect_state() {
        let mut h = DebugHistory::new(initial_state());
        assert!(h.queue_is_empty());
        assert!(h.queue_front_variant().is_none());
        push_one_event(&mut h);
        assert!(!h.queue_is_empty());
        assert_eq!(h.queue_front_variant(), Some("Init"));
    }

    #[test]
    fn pop_queue_front_removes_and_returns() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        let popped = h.pop_queue_front().unwrap();
        assert_eq!(popped.id, id);
        assert!(h.queue_is_empty());
    }

    #[test]
    fn push_stored_event_adds_to_queue() {
        let mut h = DebugHistory::new(initial_state());
        let event = make_init_event();
        let stored = StoredEvent {
            id: Uuid::new_v4(),
            at: event.at(),
            arrived_at: Utc::now(),
            variant: "Init",
            payload: serde_json::to_value(&event).unwrap(),
            caused_by_action: None,
            event,
        };
        let seq_before = h.seq;
        h.push_stored_event(stored);
        assert_eq!(h.event_queue.len(), 1);
        assert_eq!(h.seq, seq_before + 1);
    }

    // ── Event lifecycle ───────────────────────────────────────────────────────

    #[test]
    fn event_started_moves_event_to_current() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        h.event_started(id, initial_state());
        assert!(h.event_queue.is_empty());
        assert!(h.current_event.is_some());
        assert_eq!(h.current_event.as_ref().unwrap().stored.id, id);
    }

    #[test]
    fn event_started_unknown_id_is_noop() {
        let mut h = DebugHistory::new(initial_state());
        push_one_event(&mut h);
        let seq_before = h.seq;
        h.event_started(Uuid::new_v4(), initial_state()); // unknown id
        assert_eq!(h.event_queue.len(), 1); // still in queue
        assert!(h.current_event.is_none());
        assert_eq!(h.seq, seq_before); // seq unchanged
    }

    #[test]
    fn event_started_stored_sets_current_event() {
        let mut h = DebugHistory::new(initial_state());
        let event = make_init_event();
        let stored = StoredEvent {
            id: Uuid::new_v4(),
            at: event.at(),
            arrived_at: Utc::now(),
            variant: "Init",
            payload: serde_json::to_value(&event).unwrap(),
            caused_by_action: None,
            event,
        };
        let stored_id = stored.id;
        h.event_started_stored(stored, initial_state());
        assert!(h.current_event.is_some());
        assert_eq!(h.current_event.as_ref().unwrap().stored.id, stored_id);
    }

    #[test]
    fn event_completed_moves_to_trace_and_updates_latest_state() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        h.event_started(id, initial_state());
        h.event_completed(id, initial_state());
        assert!(h.current_event.is_none());
        assert_eq!(h.trace.len(), 1);
    }

    #[test]
    fn event_completed_wrong_id_clears_current_and_does_not_add_to_trace() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        h.event_started(id, initial_state());
        h.event_completed(Uuid::new_v4(), initial_state()); // wrong id
        assert!(h.current_event.is_none()); // always cleared
        assert!(h.trace.is_empty()); // not pushed to trace
    }

    #[test]
    fn trace_capped_at_trace_cap() {
        let mut h = DebugHistory::new(initial_state());
        for _ in 0..=TRACE_CAP {
            let id = push_one_event(&mut h);
            h.event_started(id, initial_state());
            h.event_completed(id, initial_state());
        }
        assert_eq!(h.trace.len(), TRACE_CAP);
    }

    // ── Actions ───────────────────────────────────────────────────────────────

    #[test]
    fn actions_ready_stores_pending_actions() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        h.event_started(id, initial_state());
        let actions = vec![windlass_core::actions::Action::ReadPortFiles];
        h.actions_ready(&actions);
        let pending = &h.current_event.as_ref().unwrap().pending_actions;
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn actions_ready_noop_when_no_current_event() {
        let mut h = DebugHistory::new(initial_state());
        let seq_before = h.seq;
        h.actions_ready(&[windlass_core::actions::Action::ReadPortFiles]);
        assert_eq!(h.seq, seq_before + 1); // seq still bumped
    }

    #[test]
    fn action_started_adds_to_running_and_current_actions() {
        let mut h = DebugHistory::new(initial_state());
        let event_id = push_one_event(&mut h);
        h.event_started(event_id, initial_state());
        let action_id = h.action_started(&windlass_core::actions::Action::ReadPortFiles, event_id);
        assert_eq!(h.running_actions.len(), 1);
        assert_eq!(h.running_actions[0].id, action_id);
        let current = h.current_event.as_ref().unwrap();
        assert_eq!(current.actions.len(), 1);
        assert_eq!(current.actions[0].id, action_id);
    }

    #[test]
    fn action_started_pops_pending_actions() {
        let mut h = DebugHistory::new(initial_state());
        let event_id = push_one_event(&mut h);
        h.event_started(event_id, initial_state());
        h.actions_ready(&[windlass_core::actions::Action::ReadPortFiles]);
        assert_eq!(h.current_event.as_ref().unwrap().pending_actions.len(), 1);
        h.action_started(&windlass_core::actions::Action::ReadPortFiles, event_id);
        assert_eq!(h.current_event.as_ref().unwrap().pending_actions.len(), 0);
    }

    #[test]
    fn action_completed_removes_from_running_and_sets_completed_at() {
        let mut h = DebugHistory::new(initial_state());
        let event_id = push_one_event(&mut h);
        h.event_started(event_id, initial_state());
        let action_id = h.action_started(&windlass_core::actions::Action::ReadPortFiles, event_id);
        h.action_completed(action_id, None);
        assert!(h.running_actions.is_empty());
        let entry = &h.current_event.as_ref().unwrap().actions[0];
        assert!(entry.completed_at.is_some());
    }

    #[test]
    fn action_completed_after_event_finalised_updates_trace() {
        let mut h = DebugHistory::new(initial_state());
        let event_id = push_one_event(&mut h);
        h.event_started(event_id, initial_state());
        let action_id = h.action_started(&windlass_core::actions::Action::ReadPortFiles, event_id);
        h.event_completed(event_id, initial_state());
        // action completes after event is in trace
        h.action_completed(action_id, None);
        let entry = &h.trace.back().unwrap().actions[0];
        assert!(entry.completed_at.is_some());
    }

    // ── HTTP exchanges ────────────────────────────────────────────────────────

    fn make_exchange() -> HttpExchange {
        HttpExchange {
            module: "qbit".to_string(),
            method: "POST".to_string(),
            url: "http://localhost/api".to_string(),
            request_body: None,
            response_status: 200,
            response_body: "Ok".to_string(),
        }
    }

    #[test]
    fn action_http_exchange_attaches_to_current_action() {
        let mut h = DebugHistory::new(initial_state());
        let event_id = push_one_event(&mut h);
        h.event_started(event_id, initial_state());
        let action_id = h.action_started(&windlass_core::actions::Action::ReadPortFiles, event_id);
        h.action_http_exchange(action_id, make_exchange());
        let entry = &h.current_event.as_ref().unwrap().actions[0];
        assert_eq!(entry.http_exchanges.len(), 1);
    }

    #[test]
    fn action_http_exchange_attaches_to_trace_action() {
        let mut h = DebugHistory::new(initial_state());
        let event_id = push_one_event(&mut h);
        h.event_started(event_id, initial_state());
        let action_id = h.action_started(&windlass_core::actions::Action::ReadPortFiles, event_id);
        h.event_completed(event_id, initial_state());
        h.action_http_exchange(action_id, make_exchange());
        let entry = &h.trace.back().unwrap().actions[0];
        assert_eq!(entry.http_exchanges.len(), 1);
    }

    // ── Logs ──────────────────────────────────────────────────────────────────

    #[test]
    fn append_log_stores_entry() {
        let mut h = DebugHistory::new(initial_state());
        h.append_log(LogEntry {
            at: Utc::now(),
            level: "INFO".to_string(),
            target: "test".to_string(),
            message: "hello".to_string(),
        });
        assert_eq!(h.logs.len(), 1);
    }

    #[test]
    fn logs_capped_at_log_cap() {
        let mut h = DebugHistory::new(initial_state());
        for _ in 0..=LOG_CAP {
            h.append_log(LogEntry {
                at: Utc::now(),
                level: "INFO".to_string(),
                target: "t".to_string(),
                message: "m".to_string(),
            });
        }
        assert_eq!(h.logs.len(), LOG_CAP);
    }

    // ── Queue commands ────────────────────────────────────────────────────────

    #[test]
    fn apply_cmd_remove_deletes_event_from_queue() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        h.apply_cmd(DebugCommand::RemoveQueuedEvent(id));
        assert!(h.event_queue.is_empty());
    }

    #[test]
    fn apply_cmd_edit_updates_stored_event() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        let (tx, mut rx) = oneshot::channel();
        let new_payload = serde_json::json!({
            "Init": { "at": "2026-01-01T00:00:00Z", "is_gluetun_healthy": false, "port_files": {"Err": "x"} }
        });
        h.apply_cmd(DebugCommand::EditQueuedEvent(id, new_payload, tx));
        assert!(rx.try_recv().unwrap().is_ok());
    }

    #[test]
    fn apply_cmd_edit_unknown_id_returns_err() {
        let mut h = DebugHistory::new(initial_state());
        let (tx, mut rx) = oneshot::channel();
        h.apply_cmd(DebugCommand::EditQueuedEvent(
            Uuid::new_v4(),
            serde_json::Value::Null,
            tx,
        ));
        assert!(rx.try_recv().unwrap().is_err());
    }

    #[test]
    fn apply_cmd_edit_invalid_payload_returns_err() {
        let mut h = DebugHistory::new(initial_state());
        let id = push_one_event(&mut h);
        let (tx, mut rx) = oneshot::channel();
        h.apply_cmd(DebugCommand::EditQueuedEvent(
            id,
            serde_json::json!({ "NotAnEvent": {} }),
            tx,
        ));
        assert!(rx.try_recv().unwrap().is_err());
    }

    #[test]
    fn apply_cmd_inject_inserts_at_position() {
        let mut h = DebugHistory::new(initial_state());
        push_one_event(&mut h);
        let (tx, mut rx) = oneshot::channel();
        let payload = serde_json::json!({
            "Init": { "at": "2026-01-01T00:00:00Z", "is_gluetun_healthy": true, "port_files": {"Err": "x"} }
        });
        h.apply_cmd(DebugCommand::InjectEvent {
            payload,
            position: Some(0),
            at: Utc::now(),
            reply: tx,
        });
        assert!(rx.try_recv().unwrap().is_ok());
        assert_eq!(h.event_queue.len(), 2);
    }

    #[test]
    fn apply_cmd_inject_invalid_payload_returns_err() {
        let mut h = DebugHistory::new(initial_state());
        let (tx, mut rx) = oneshot::channel();
        h.apply_cmd(DebugCommand::InjectEvent {
            payload: serde_json::json!({ "NotAnEvent": {} }),
            position: None,
            at: Utc::now(),
            reply: tx,
        });
        assert!(rx.try_recv().unwrap().is_err());
    }

    #[test]
    fn apply_cmd_reorder_queue_reorders() {
        let mut h = DebugHistory::new(initial_state());
        let id1 = push_one_event(&mut h);
        let id2 = push_one_event(&mut h);
        let (tx, mut rx) = oneshot::channel();
        h.apply_cmd(DebugCommand::ReorderQueue(vec![id2, id1], tx));
        assert!(rx.try_recv().unwrap().is_ok());
        assert_eq!(h.event_queue[0].id, id2);
        assert_eq!(h.event_queue[1].id, id1);
    }

    #[test]
    fn apply_cmd_reorder_wrong_count_returns_err() {
        let mut h = DebugHistory::new(initial_state());
        push_one_event(&mut h);
        let (tx, mut rx) = oneshot::channel();
        h.apply_cmd(DebugCommand::ReorderQueue(vec![], tx));
        assert!(rx.try_recv().unwrap().is_err());
    }

    #[test]
    fn apply_cmd_reorder_unknown_id_returns_err() {
        let mut h = DebugHistory::new(initial_state());
        push_one_event(&mut h);
        let (tx, mut rx) = oneshot::channel();
        h.apply_cmd(DebugCommand::ReorderQueue(vec![Uuid::new_v4()], tx));
        assert!(rx.try_recv().unwrap().is_err());
    }

    #[test]
    fn latest_state_returns_initial_state_before_any_event() {
        let h = DebugHistory::new(initial_state());
        let s = h.latest_state();
        assert_eq!(s, &initial_state());
    }

    #[test]
    fn push_causal_event_records_caused_by() {
        let mut h = DebugHistory::new(initial_state());
        let action_id = Uuid::new_v4();
        let event_id = h.push_causal_event(make_init_event(), action_id);
        let stored = h.event_queue.back().unwrap();
        assert_eq!(stored.id, event_id);
        assert_eq!(stored.caused_by_action, Some(action_id));
    }
}
