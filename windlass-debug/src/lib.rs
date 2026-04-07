#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

mod stream;

pub use stream::{DebuggableEventStream, action_variant};

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Semaphore, broadcast};
use windlass_core::Observation;
use windlass_core::actions::Action;

// ── PausedOn ──────────────────────────────────────────────────────────────────

/// Describes what the debug loop is currently paused on.
/// Serialised into `DebugState` and returned by `GET /api/v1/debug`.
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind")]
pub enum PausedOn {
    /// Paused before processing an event.
    Event { variant: &'static str },
    /// Paused before dispatching an action within a batch.
    Action {
        variant: &'static str,
        /// 1-based position within the current batch.
        index: usize,
        /// Total number of actions in the batch.
        of: usize,
    },
}

// ── DebugState ────────────────────────────────────────────────────────────────

/// Serialisable snapshot of the current debug state, served by `GET /api/v1/debug`.
#[derive(Serialize)]
pub struct DebugState {
    pub debug_mode: bool,
    /// What the system is currently paused on, or `null` when running freely.
    pub paused_on: Option<PausedOn>,
    /// The current action batch, pre-serialised; empty when not dispatching.
    pub pending_actions: Vec<Value>,
    pub event_breakpoints: Vec<String>,
    pub action_breakpoints: Vec<String>,
}

// ── DebugController ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct DebugController(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    /// Manually enabled/disabled from the UI or by the MAM rate-limit guardrail.
    /// When enabled, each event/action waits for an explicit step permit.
    debug_mode: AtomicBool,
    event_breakpoints: Mutex<HashSet<String>>,
    action_breakpoints: Mutex<HashSet<String>>,
    /// One permit = one event or action may proceed.
    /// Released by `POST /debug/step`; consumed by `acquire_step`.
    step: Semaphore,
    /// When set, the next `acquire_step` caller skips its item instead of executing it.
    skip: AtomicBool,
    /// Present only while debug mode is active — lets clients emit `HttpExchange` observations.
    obs_tx: Mutex<Option<broadcast::Sender<Observation>>>,
    /// What the system is currently paused on; `None` when running freely.
    paused_on: ArcSwap<Option<PausedOn>>,
    /// The current action batch, pre-serialised; empty when not dispatching.
    pending_actions: ArcSwap<Vec<Value>>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            debug_mode: AtomicBool::new(false),
            event_breakpoints: Mutex::new(HashSet::new()),
            action_breakpoints: Mutex::new(HashSet::new()),
            // Start with zero permits — nothing steps until explicitly requested.
            step: Semaphore::new(0),
            skip: AtomicBool::new(false),
            obs_tx: Mutex::new(None),
            paused_on: ArcSwap::from_pointee(None),
            pending_actions: ArcSwap::from_pointee(vec![]),
        }
    }
}

impl DebugController {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// Disable debug mode. Clears all debug state and releases any blocked
    /// step waiters so the system resumes normal operation.
    ///
    /// # Panics
    /// Panics if any internal mutex is poisoned.
    pub fn disable_debug(&self) {
        self.0.debug_mode.store(false, Ordering::SeqCst);
        self.0.skip.store(false, Ordering::SeqCst);
        self.0.paused_on.store(Arc::new(None));
        self.0.pending_actions.store(Arc::new(vec![]));
        *self.0.obs_tx.lock().unwrap() = None;
        // Release any task currently blocked on the step semaphore.
        self.0.step.add_permits(usize::MAX / 2);
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

    // ── Step permits ──────────────────────────────────────────────────────────

    /// Blocks until a step permit is available.
    /// Returns `true` if the event or action should execute, `false` if it should be skipped.
    ///
    /// # Panics
    /// Panics if the semaphore has been closed, which never happens in normal operation.
    pub async fn acquire_step(&self) -> bool {
        self.0.step.acquire().await.unwrap().forget();
        !self.0.skip.swap(false, Ordering::SeqCst)
    }

    /// Releases one step permit, allowing the currently-paused event or action to execute.
    pub fn release_step(&self) {
        self.0.step.add_permits(1);
    }

    /// Skips the currently-paused event or action without executing it.
    /// Sets the skip flag then releases one permit so the waiter wakes up.
    pub fn request_skip(&self) {
        self.0.skip.store(true, Ordering::SeqCst);
        self.0.step.add_permits(1);
    }

    // ── Pause state ───────────────────────────────────────────────────────────

    /// Sets or clears what the system is currently paused on.
    /// Called by [`DebuggableEventStream`] and [`DebuggableShell`] around step waits.
    pub fn set_paused_on(&self, p: Option<PausedOn>) {
        self.0.paused_on.store(Arc::new(p));
    }

    /// Stores the current action batch as pre-serialised JSON.
    /// Called by `DebuggableShell` at the start of each dispatch cycle.
    pub fn set_pending_actions(&self, actions: &[Action]) {
        let json: Vec<Value> = actions
            .iter()
            .filter_map(|a| serde_json::to_value(a).ok())
            .collect();
        self.0.pending_actions.store(Arc::new(json));
    }

    /// Clears the pending-actions batch.
    /// Called by `DebuggableShell` when the batch is fully dispatched.
    pub fn clear_pending_actions(&self) {
        self.0.pending_actions.store(Arc::new(vec![]));
    }

    // ── HTTP observation channel ──────────────────────────────────────────────

    /// Returns the observation sender when debug mode is active.
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
        let paused_on = self.0.paused_on.load_full();
        let paused_on: Option<PausedOn> = paused_on.as_ref().clone();

        let pending_actions = self.0.pending_actions.load_full();
        let pending_actions: Vec<Value> = pending_actions.as_ref().clone();

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
            debug_mode: self.is_debug_mode(),
            paused_on,
            pending_actions,
            event_breakpoints,
            action_breakpoints,
        }
    }
}
