#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::{Semaphore, broadcast};
use windlass_core::Observation;
use windlass_core::actions::Action;
use windlass_core::events::Event;

// ── DebugState ────────────────────────────────────────────────────────────────

/// Serialisable snapshot of the current debug state, served by `GET /api/v1/debug`.
#[derive(Serialize)]
pub struct DebugState {
    pub frozen: bool,
    pub debug_mode: bool,
    pub pending_event: Option<serde_json::Value>,
    pub pending_actions: Vec<serde_json::Value>,
    pub event_breakpoints: Vec<String>,
    pub action_breakpoints: Vec<String>,
}

// ── DebugController ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct DebugController(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    /// Emergency freeze flag (rate-limit guard). Drops all incoming events.
    /// Absorbed from the old `DebugGate`.
    frozen: AtomicBool,
    /// Manually enabled/disabled from the UI. When enabled, each event/action
    /// is queued and waits for an explicit step permit.
    debug_mode: AtomicBool,
    event_queue: Mutex<VecDeque<Event>>,
    action_queue: Mutex<VecDeque<Action>>,
    event_breakpoints: Mutex<HashSet<String>>,
    action_breakpoints: Mutex<HashSet<String>>,
    /// One permit = one event may be processed. POST /debug/step/event adds a permit.
    step_event: Semaphore,
    /// One permit = one action may be dispatched. POST /debug/step/action adds a permit.
    step_action: Semaphore,
    /// Present only when debug mode is active — lets clients emit `HttpExchange` observations.
    obs_tx: Mutex<Option<broadcast::Sender<Observation>>>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            frozen: AtomicBool::new(false),
            debug_mode: AtomicBool::new(false),
            event_queue: Mutex::new(VecDeque::new()),
            action_queue: Mutex::new(VecDeque::new()),
            event_breakpoints: Mutex::new(HashSet::new()),
            action_breakpoints: Mutex::new(HashSet::new()),
            // Start with zero permits — nothing steps until explicitly requested.
            step_event: Semaphore::new(0),
            step_action: Semaphore::new(0),
            obs_tx: Mutex::new(None),
        }
    }
}

impl DebugController {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── Freeze (rate-limit emergency) ──────────────────────────────────────────

    /// Freeze the event loop permanently (rate-limit guard). Idempotent.
    pub fn freeze(&self) {
        self.0.frozen.store(true, Ordering::SeqCst);
    }

    /// Unfreeze the event loop. Idempotent.
    pub fn unfreeze(&self) {
        self.0.frozen.store(false, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_frozen(&self) -> bool {
        self.0.frozen.load(Ordering::SeqCst)
    }

    // ── Debug mode ────────────────────────────────────────────────────────────

    /// Enable debug mode. Wires up `obs_tx` so that clients can send `HttpExchange`
    /// observations while debug mode is active.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn enable_debug(&self, obs_tx: broadcast::Sender<Observation>) {
        self.0.debug_mode.store(true, Ordering::SeqCst);
        *self.0.obs_tx.lock().unwrap() = Some(obs_tx);
    }

    /// Disable debug mode. Clears the obs channel and discards any queued
    /// events/actions so the system can resume normal operation.
    ///
    /// # Panics
    /// Panics if any internal mutex is poisoned.
    pub fn disable_debug(&self) {
        self.0.debug_mode.store(false, Ordering::SeqCst);
        *self.0.obs_tx.lock().unwrap() = None;
        self.0.event_queue.lock().unwrap().clear();
        self.0.action_queue.lock().unwrap().clear();
        // Release any tasks currently blocked on step permits so they can
        // drain naturally once debug mode is disabled.
        self.0.step_event.add_permits(usize::MAX / 2);
        self.0.step_action.add_permits(usize::MAX / 2);
    }

    #[must_use]
    pub fn is_debug_mode(&self) -> bool {
        self.0.debug_mode.load(Ordering::SeqCst)
    }

    // ── Breakpoints ───────────────────────────────────────────────────────────

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn add_event_breakpoint(&self, variant: impl Into<String>) {
        self.0
            .event_breakpoints
            .lock()
            .unwrap()
            .insert(variant.into());
    }

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn remove_event_breakpoint(&self, variant: &str) {
        self.0.event_breakpoints.lock().unwrap().remove(variant);
    }

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn add_action_breakpoint(&self, variant: impl Into<String>) {
        self.0
            .action_breakpoints
            .lock()
            .unwrap()
            .insert(variant.into());
    }

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn remove_action_breakpoint(&self, variant: &str) {
        self.0.action_breakpoints.lock().unwrap().remove(variant);
    }

    /// Returns `true` if the event loop should pause before processing `variant`.
    /// Pauses when debug mode is fully enabled, or the variant name is breakpointed.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn should_pause_on_event(&self, variant: &str) -> bool {
        self.is_debug_mode() || self.0.event_breakpoints.lock().unwrap().contains(variant)
    }

    /// Returns `true` if the event loop should pause before dispatching `variant`.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn should_pause_on_action(&self, variant: &str) -> bool {
        self.is_debug_mode() || self.0.action_breakpoints.lock().unwrap().contains(variant)
    }

    // ── Event queue ───────────────────────────────────────────────────────────

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn enqueue_event(&self, event: Event) {
        self.0.event_queue.lock().unwrap().push_back(event);
    }

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn dequeue_event(&self) -> Option<Event> {
        self.0.event_queue.lock().unwrap().pop_front()
    }

    // ── Action queue ──────────────────────────────────────────────────────────

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn enqueue_action(&self, action: Action) {
        self.0.action_queue.lock().unwrap().push_back(action);
    }

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn dequeue_action(&self) -> Option<Action> {
        self.0.action_queue.lock().unwrap().pop_front()
    }

    // ── Step permits ──────────────────────────────────────────────────────────

    /// Blocks until a step permit is available (released by `POST /debug/step/event`).
    ///
    /// # Panics
    /// Panics if the semaphore has been closed, which never happens in normal operation.
    pub async fn acquire_event_step(&self) {
        // `acquire` is cancel-safe and returns a permit that auto-decrements on drop.
        // We forget it immediately since we only need the counting behaviour.
        self.0.step_event.acquire().await.unwrap().forget();
    }

    /// Adds one step permit for an event, unblocking the event loop once.
    pub fn release_event_step(&self) {
        self.0.step_event.add_permits(1);
    }

    /// Blocks until a step permit is available (released by `POST /debug/step/action`).
    ///
    /// # Panics
    /// Panics if the semaphore has been closed, which never happens in normal operation.
    pub async fn acquire_action_step(&self) {
        self.0.step_action.acquire().await.unwrap().forget();
    }

    /// Adds one step permit for an action, unblocking the dispatch loop once.
    pub fn release_action_step(&self) {
        self.0.step_action.add_permits(1);
    }

    // ── HTTP observation channel ──────────────────────────────────────────────

    /// Returns the observation sender when debug mode is active.
    /// Clients use this to emit `Observation::HttpExchange` messages.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn obs_sender(&self) -> Option<broadcast::Sender<Observation>> {
        self.0.obs_tx.lock().unwrap().clone()
    }

    // ── State snapshot ────────────────────────────────────────────────────────

    /// Returns a serialisable snapshot of the current debug state.
    ///
    /// # Panics
    /// Panics if any internal mutex is poisoned.
    #[must_use]
    pub fn debug_state(&self) -> DebugState {
        let pending_event = self
            .0
            .event_queue
            .lock()
            .unwrap()
            .front()
            .and_then(|e| serde_json::to_value(e).ok());

        let pending_actions = self
            .0
            .action_queue
            .lock()
            .unwrap()
            .iter()
            .filter_map(|a| serde_json::to_value(a).ok())
            .collect();

        let event_breakpoints = self
            .0
            .event_breakpoints
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();

        let action_breakpoints = self
            .0
            .action_breakpoints
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();

        DebugState {
            frozen: self.is_frozen(),
            debug_mode: self.is_debug_mode(),
            pending_event,
            pending_actions,
            event_breakpoints,
            action_breakpoints,
        }
    }
}
