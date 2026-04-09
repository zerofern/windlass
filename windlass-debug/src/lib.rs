#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod causal_tx;
mod dispatcher;
pub mod history;
pub mod log_layer;
mod stream;
pub mod types;

pub use causal_tx::CausalTx;
pub use dispatcher::DebugDispatcher;
pub use history::DebugHistory;
pub use log_layer::DebugLogLayer;
pub use stream::DebuggableEventStream;
pub use types::{
    ActionEntry, ActiveEvent, DebugCommand, LogEntry, RunningAction, StoredEvent, TraceEntry,
};

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast, mpsc};
use windlass_core::events::Event;
use windlass_core::{HttpObserver, Observation};

pub(crate) use stream::QueueSink;

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

/// Serialisable snapshot of the current debug state, served by `GET /api/v1/debug`
/// and pushed on every change over `/api/v1/debug/stream` (Phase 2+).
#[derive(Serialize, Clone, Debug)]
pub struct DebugState {
    /// Monotonic counter incremented on every mutation. Used by the frontend
    /// to discard stale SSE events that arrived before the initial GET snapshot.
    pub seq: u64,
    pub debug_mode: bool,
    /// What the system is currently paused on, or `null` when running freely.
    pub paused_on: Option<PausedOn>,
    pub event_breakpoints: Vec<String>,
    pub action_breakpoints: Vec<String>,
    // ── History fields (populated when debug mode is active) ──────────────────
    pub event_queue: Vec<StoredEvent>,
    pub current_event: Option<ActiveEvent>,
    pub running_actions: Vec<RunningAction>,
    /// Last 200 completed events with full before/after state and actions.
    pub trace: Vec<TraceEntry>,
    /// Last 500 log lines (populated from Phase 5 onwards).
    pub logs: Vec<LogEntry>,
}

impl DebugState {
    fn initial() -> Self {
        Self {
            seq: 0,
            debug_mode: false,
            paused_on: None,
            event_breakpoints: Vec::new(),
            action_breakpoints: Vec::new(),
            event_queue: Vec::new(),
            current_event: None,
            running_actions: Vec::new(),
            trace: Vec::new(),
            logs: Vec::new(),
        }
    }
}

// ── DebugController ───────────────────────────────────────────────────────────

/// The debug controller, cloned freely into HTTP handlers and SSE tasks.
/// All fields are `Arc`-wrapped so cloning is cheap.
///
/// The `DebugHistory` (the mutable history half) lives separately in the main
/// event loop as `&mut DebugHistory`. HTTP handlers communicate with it via
/// `cmd_tx`; the main loop drains `cmd_rx` between events.
#[derive(Clone, Debug)]
pub struct DebugController {
    /// Manually enabled/disabled from the UI or by the MAM rate-limit guardrail.
    /// When enabled, each event/action waits for an explicit step permit.
    debug_mode: Arc<AtomicBool>,
    event_breakpoints: Arc<ArcSwap<HashSet<String>>>,
    action_breakpoints: Arc<ArcSwap<HashSet<String>>>,
    /// One permit = one event or action may proceed.
    /// Released by `POST /debug/step`; consumed by `acquire_step`.
    step: Arc<Semaphore>,
    /// When set, the next `acquire_step` caller skips its item instead of executing it.
    skip: Arc<AtomicBool>,
    /// Present only while debug mode is active — lets clients emit `HttpExchange` observations.
    obs_tx: Arc<ArcSwap<Option<broadcast::Sender<Observation>>>>,
    /// What the system is currently paused on; `None` when running freely.
    paused_on: Arc<ArcSwap<Option<PausedOn>>>,
    // ── Queue routing ─────────────────────────────────────────────────────────
    /// Controls where the intake task routes incoming events.
    /// Swapped atomically on enable/disable debug.
    pub(crate) queue_sink: Arc<ArcSwap<QueueSink>>,
    /// Sender for the non-debug (mpsc) path — restored on `disable_debug()`.
    internal_tx: mpsc::Sender<Event>,
    /// Sender for the debug (VecDeque) path — activated on `enable_debug()`.
    stored_tx: mpsc::Sender<StoredEvent>,
    // ── Shared handles for history / snapshot ─────────────────────────────────
    /// Latest published snapshot of `DebugState`. Read by GET /api/v1/debug.
    pub snapshot: Arc<ArcSwap<DebugState>>,
    /// HTTP handlers send queue-manipulation commands here; main loop drains `cmd_rx`.
    pub cmd_tx: mpsc::Sender<DebugCommand>,
    /// `DebugLogLayer` sends captured log lines here (Phase 5+); main loop drains `log_rx`.
    pub log_tx: mpsc::Sender<LogEntry>,
    /// Broadcasts the new `seq` on every snapshot update; SSE handler subscribes.
    pub notify_tx: broadcast::Sender<u64>,
}

/// The receiver halves of the history channels, owned exclusively by the main
/// event loop. Returned alongside `DebugController` from `new_with_owned`.
pub struct DebugOwnedPart {
    /// Receives events from the intake task in non-debug mode.
    pub internal_rx: mpsc::Receiver<Event>,
    /// Receives `StoredEvent`s from the intake task in debug mode.
    pub queue_rx: mpsc::Receiver<StoredEvent>,
    pub cmd_rx: mpsc::Receiver<DebugCommand>,
    pub log_rx: mpsc::Receiver<LogEntry>,
}

impl DebugController {
    /// Creates a `DebugController` alongside the receiver halves that the main
    /// event loop must own. Pass the returned `DebugOwnedPart` to `shell::run`.
    pub fn new_with_owned() -> (Self, DebugOwnedPart) {
        let (internal_tx, internal_rx) = mpsc::channel::<Event>(128);
        let (stored_tx, queue_rx) = mpsc::channel::<StoredEvent>(128);
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (log_tx, log_rx) = mpsc::channel(1024);
        let (notify_tx, _) = broadcast::channel(256);

        // Start in non-debug mode: intake task routes to internal_tx.
        let queue_sink = Arc::new(ArcSwap::from_pointee(QueueSink::Mpsc(internal_tx.clone())));

        let ctrl = Self {
            debug_mode: Arc::new(AtomicBool::new(false)),
            event_breakpoints: Arc::new(ArcSwap::from_pointee(HashSet::new())),
            action_breakpoints: Arc::new(ArcSwap::from_pointee(HashSet::new())),
            step: Arc::new(Semaphore::new(0)),
            skip: Arc::new(AtomicBool::new(false)),
            obs_tx: Arc::new(ArcSwap::from_pointee(None)),
            paused_on: Arc::new(ArcSwap::from_pointee(None)),
            queue_sink,
            internal_tx,
            stored_tx,
            snapshot: Arc::new(ArcSwap::from_pointee(DebugState::initial())),
            cmd_tx,
            log_tx,
            notify_tx,
        };
        let owned = DebugOwnedPart {
            internal_rx,
            queue_rx,
            cmd_rx,
            log_rx,
        };
        (ctrl, owned)
    }

    /// Convenience constructor that discards the owned channels.
    /// Suitable for test code or contexts where history is not needed.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_owned().0
    }

    // ── Debug mode ────────────────────────────────────────────────────────────

    pub fn enable_debug(&self, obs_tx: broadcast::Sender<Observation>) {
        self.debug_mode.store(true, Ordering::SeqCst);
        self.obs_tx.store(Arc::new(Some(obs_tx)));
        // Swap intake routing to the VecDeque path.
        self.queue_sink
            .store(Arc::new(QueueSink::Queue(self.stored_tx.clone())));
    }

    pub fn disable_debug(&self) {
        self.debug_mode.store(false, Ordering::SeqCst);
        self.skip.store(false, Ordering::SeqCst);
        self.obs_tx.store(Arc::new(None));
        self.paused_on.store(Arc::new(None));
        self.step.add_permits(usize::MAX / 2);
        // Restore intake routing to the direct mpsc path.
        self.queue_sink
            .store(Arc::new(QueueSink::Mpsc(self.internal_tx.clone())));
    }

    #[must_use]
    pub fn is_debug_mode(&self) -> bool {
        self.debug_mode.load(Ordering::SeqCst)
    }

    /// Returns a clone of the `Arc<AtomicBool>` flag so external components
    /// (e.g. `DebugLogLayer`) can check debug mode with a single atomic load.
    #[must_use]
    pub fn debug_mode_flag(&self) -> Arc<AtomicBool> {
        self.debug_mode.clone()
    }

    // ── Breakpoints ───────────────────────────────────────────────────────────

    pub fn add_event_breakpoint(&self, variant: impl Into<String>) {
        let v = variant.into();
        self.event_breakpoints.rcu(|set| {
            let mut new_set = (**set).clone();
            new_set.insert(v.clone());
            new_set
        });
    }

    pub fn remove_event_breakpoint(&self, variant: &str) {
        let v = variant.to_owned();
        self.event_breakpoints.rcu(|set| {
            let mut new_set = (**set).clone();
            new_set.remove(&v);
            new_set
        });
    }

    pub fn add_action_breakpoint(&self, variant: impl Into<String>) {
        let v = variant.into();
        self.action_breakpoints.rcu(|set| {
            let mut new_set = (**set).clone();
            new_set.insert(v.clone());
            new_set
        });
    }

    pub fn remove_action_breakpoint(&self, variant: &str) {
        let v = variant.to_owned();
        self.action_breakpoints.rcu(|set| {
            let mut new_set = (**set).clone();
            new_set.remove(&v);
            new_set
        });
    }

    #[must_use]
    pub fn should_pause_on_event(&self, variant: &str) -> bool {
        self.is_debug_mode() || self.event_breakpoints.load().contains(variant)
    }

    #[must_use]
    pub fn should_pause_on_action(&self, variant: &str) -> bool {
        self.is_debug_mode() || self.action_breakpoints.load().contains(variant)
    }

    // ── Step permits ──────────────────────────────────────────────────────────

    /// Blocks until a step permit is available.
    /// Returns `true` if the event or action should execute, `false` if it should be skipped.
    ///
    /// # Panics
    /// Panics if the semaphore has been closed, which never happens in normal operation.
    pub async fn acquire_step(&self) -> bool {
        self.step.acquire().await.unwrap().forget();
        !self.skip.swap(false, Ordering::SeqCst)
    }

    pub fn release_step(&self) {
        self.step.add_permits(1);
    }

    pub fn request_skip(&self) {
        self.skip.store(true, Ordering::SeqCst);
        self.step.add_permits(1);
    }

    // ── Pause state ───────────────────────────────────────────────────────────

    /// Sets or clears what the system is currently paused on.
    pub fn set_paused_on(&self, p: Option<PausedOn>) {
        self.paused_on.store(Arc::new(p));
    }

    // ── Snapshot / publish ────────────────────────────────────────────────────

    /// Serialises the current history into a new `DebugState` snapshot,
    /// stores it in `self.snapshot`, and broadcasts the new `seq` so SSE
    /// subscribers know to refresh. No-op when debug mode is off.
    pub fn publish(&self, history: &DebugHistory) {
        if !self.is_debug_mode() {
            return;
        }
        let state = Arc::new(DebugState {
            seq: history.seq,
            debug_mode: true,
            paused_on: self.paused_on.load_full().as_ref().clone(),
            event_queue: history.event_queue.iter().cloned().collect(),
            current_event: history.current_event.clone(),
            running_actions: history.running_actions.clone(),
            trace: history.trace.iter().cloned().collect(),
            logs: history.logs.iter().cloned().collect(),
            event_breakpoints: self.event_breakpoints.load().iter().cloned().collect(),
            action_breakpoints: self.action_breakpoints.load().iter().cloned().collect(),
        });
        self.snapshot.store(state);
        let _ = self.notify_tx.send(history.seq);
    }

    // ── HTTP observation ──────────────────────────────────────────────────────

    /// Returns an [`HttpObserver`] that forwards observations to the SSE
    /// channel when debug mode is active, and is a no-op when it is not.
    #[must_use]
    pub fn make_http_observer(&self) -> HttpObserver {
        let obs_tx = Arc::clone(&self.obs_tx);
        Arc::new(move |obs| {
            if let Some(tx) = obs_tx.load_full().as_ref().as_ref() {
                let _ = tx.send(obs);
            }
        })
    }
}
