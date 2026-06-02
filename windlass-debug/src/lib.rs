#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod causal_tx;
pub mod history;
pub mod log_layer;
pub mod types;

pub use causal_tx::CausalTx;
pub use history::DebugHistory;
pub use log_layer::DebugLogLayer;
pub use types::{
    ActionEntry, ActiveEvent, DebugCommand, LogEntry, RunningAction, StoredEvent, TraceEntry,
};

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast, mpsc};
use uuid::Uuid;
use windlass_core::HttpObserver;
use windlass_core::types::SystemState;
use windlass_types::HttpExchange;

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
    /// The system state after the most-recently completed event. Always valid —
    /// initialised to `SystemState::initial()`. Used by the dryrun endpoint.
    pub latest_state: SystemState,
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
            latest_state: SystemState::initial(),
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
    /// Vestigial from the legacy debug-mode toggle; kept so existing
    /// callers compile but never flips to true.  Removed entirely with
    /// the §37j rename pass.
    debug_mode: Arc<AtomicBool>,
    event_breakpoints: Arc<ArcSwap<HashSet<String>>>,
    action_breakpoints: Arc<ArcSwap<HashSet<String>>>,
    /// One permit = one event or action may proceed.  Now unused after
    /// §37d closeout; retained so `DebugDispatcher`-shaped tests still
    /// link.
    step: Arc<Semaphore>,
    /// When set, the next `acquire_step` caller skips its item.
    skip: Arc<AtomicBool>,
    /// What the system is currently paused on; `None` when running freely.
    paused_on: Arc<ArcSwap<Option<PausedOn>>>,
    // ── Shared handles for history / snapshot ─────────────────────────────────
    /// Latest published snapshot of `DebugState`.  The SSE stream and
    /// health endpoint still read this until §37h wires the new
    /// observability page.
    pub snapshot: Arc<ArcSwap<DebugState>>,
    /// HTTP handlers send queue-manipulation commands here; main loop drains `cmd_rx`.
    pub cmd_tx: mpsc::Sender<DebugCommand>,
    /// `DebugLogLayer` sends captured log lines here; main loop drains `log_rx`.
    pub log_tx: mpsc::Sender<LogEntry>,
    /// `make_http_observer` sends `(action_id, exchange)` here in debug mode.
    exchange_tx: mpsc::Sender<(Uuid, HttpExchange)>,
    /// Broadcasts the new `seq` on every snapshot update; SSE handler subscribes.
    pub notify_tx: broadcast::Sender<u64>,
}

/// The receiver halves of the history channels, owned exclusively by the main
/// event loop. Returned alongside `DebugController` from `new_with_owned`.
pub struct DebugOwnedPart {
    pub cmd_rx: mpsc::Receiver<DebugCommand>,
    pub log_rx: mpsc::Receiver<LogEntry>,
    /// Receives `(action_id, HttpExchange)` pairs from `make_http_observer`
    /// when debug mode is active.
    pub exchange_rx: mpsc::Receiver<(Uuid, HttpExchange)>,
}

impl DebugController {
    /// Creates a `DebugController` alongside the receiver halves that the main
    /// event loop must own. Pass the returned `DebugOwnedPart` to `shell::run`.
    #[must_use]
    pub fn new_with_owned() -> (Self, DebugOwnedPart) {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (log_tx, log_rx) = mpsc::channel(1024);
        let (exchange_tx, exchange_rx) = mpsc::channel(1024);
        let (notify_tx, _) = broadcast::channel(256);

        let ctrl = Self {
            debug_mode: Arc::new(AtomicBool::new(false)),
            event_breakpoints: Arc::new(ArcSwap::from_pointee(HashSet::new())),
            action_breakpoints: Arc::new(ArcSwap::from_pointee(HashSet::new())),
            step: Arc::new(Semaphore::new(0)),
            skip: Arc::new(AtomicBool::new(false)),
            paused_on: Arc::new(ArcSwap::from_pointee(None)),
            snapshot: Arc::new(ArcSwap::from_pointee(DebugState::initial())),
            cmd_tx,
            log_tx,
            exchange_tx,
            notify_tx,
        };
        let owned = DebugOwnedPart {
            cmd_rx,
            log_rx,
            exchange_rx,
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

    /// Vestigial post-§37d.  The legacy queue-routing path is gone;
    /// nothing actually pauses now.  Kept so existing callers compile
    /// until §37j retires `windlass-debug`.
    pub fn enable_debug(&self) {
        self.debug_mode.store(true, Ordering::SeqCst);
        let current = self.snapshot.load_full();
        let seq = current.seq + 1;
        let enabled_state = Arc::new(DebugState {
            seq,
            debug_mode: true,
            ..(*current).clone()
        });
        self.snapshot.store(enabled_state);
        let _ = self.notify_tx.send(seq);
    }

    pub fn disable_debug(&self) {
        self.debug_mode.store(false, Ordering::SeqCst);
        self.skip.store(false, Ordering::SeqCst);
        self.paused_on.store(Arc::new(None));
        let available = self.step.available_permits();
        let to_add = Semaphore::MAX_PERMITS.saturating_sub(available);
        if to_add > 0 {
            self.step.add_permits(to_add);
        }
        let current = self.snapshot.load_full();
        let seq = current.seq + 1;
        let final_state = Arc::new(DebugState {
            seq,
            debug_mode: false,
            paused_on: None,
            ..(*current).clone()
        });
        self.snapshot.store(final_state);
        let _ = self.notify_tx.send(seq);
    }

    pub fn update_latest_state(&self, latest_state: SystemState) {
        let current = self.snapshot.load_full();
        let seq = current.seq + 1;
        let updated_state = Arc::new(DebugState {
            seq,
            debug_mode: self.is_debug_mode(),
            latest_state,
            ..(*current).clone()
        });
        self.snapshot.store(updated_state);
        let _ = self.notify_tx.send(seq);
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
            latest_state: history.latest_state.clone(),
        });
        self.snapshot.store(state);
        let _ = self.notify_tx.send(history.seq);
    }

    /// Patches only the `paused_on` field of the current snapshot and
    /// rebroadcasts. Used by [`DebugDispatcher`] which has no access to
    /// `DebugHistory` but must notify the frontend when it pauses on an action.
    pub fn publish_paused(&self) {
        if !self.is_debug_mode() {
            return;
        }
        let current = self.snapshot.load_full();
        let seq = current.seq + 1;
        let updated = Arc::new(DebugState {
            seq,
            paused_on: self.paused_on.load_full().as_ref().clone(),
            ..(*current).clone()
        });
        self.snapshot.store(updated);
        let _ = self.notify_tx.send(seq);
    }

    // ── HTTP observation ──────────────────────────────────────────────────────

    /// Returns an [`HttpObserver`] that, when debug mode is active, reads
    /// `CURRENT_ACTION_ID` from the task-local set by `CausalTx::run` and
    /// routes the exchange to the main loop via `exchange_tx`. No-op when
    /// debug mode is off (single atomic load, no allocation).
    #[must_use]
    pub fn make_http_observer(&self) -> HttpObserver {
        let exchange_tx = self.exchange_tx.clone();
        let debug_mode = self.debug_mode.clone();
        Arc::new(move |exchange: HttpExchange| {
            if !debug_mode.load(Ordering::Relaxed) {
                return;
            }
            if let Some(action_id) = causal_tx::CURRENT_ACTION_ID
                .try_with(|id| *id)
                .ok()
                .flatten()
            {
                let _ = exchange_tx.try_send((action_id, exchange));
            }
        })
    }
}

impl Default for DebugController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_core::types::SystemState;

    fn ctrl() -> DebugController {
        DebugController::new()
    }

    #[test]
    fn new_starts_with_debug_mode_off() {
        assert!(!ctrl().is_debug_mode());
    }

    #[test]
    fn enable_then_disable_toggles_debug_mode() {
        let c = ctrl();
        c.enable_debug();
        assert!(c.is_debug_mode());
        c.disable_debug();
        assert!(!c.is_debug_mode());
    }

    #[test]
    fn debug_mode_flag_reflects_state() {
        let c = ctrl();
        let flag = c.debug_mode_flag();
        assert!(!flag.load(Ordering::SeqCst));
        c.enable_debug();
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn should_pause_on_event_respects_debug_mode() {
        let c = ctrl();
        assert!(!c.should_pause_on_event("Init"));
        c.enable_debug();
        assert!(c.should_pause_on_event("Init"));
    }

    #[test]
    fn event_breakpoint_add_remove() {
        let c = ctrl();
        c.add_event_breakpoint("Init");
        assert!(c.should_pause_on_event("Init"));
        assert!(!c.should_pause_on_event("Wakeup"));
        c.remove_event_breakpoint("Init");
        assert!(!c.should_pause_on_event("Init"));
    }

    #[test]
    fn action_breakpoint_add_remove() {
        let c = ctrl();
        c.add_action_breakpoint("ReadPortFiles");
        assert!(c.should_pause_on_action("ReadPortFiles"));
        assert!(!c.should_pause_on_action("AuthenticateQbit"));
        c.remove_action_breakpoint("ReadPortFiles");
        assert!(!c.should_pause_on_action("ReadPortFiles"));
    }

    #[test]
    fn release_step_increases_permits() {
        let c = ctrl();
        c.release_step();
        // If no permits were available before, the semaphore should now have 1.
        assert_eq!(c.step.available_permits(), 1);
    }

    #[test]
    fn request_skip_sets_skip_flag_and_adds_permit() {
        let c = ctrl();
        c.request_skip();
        assert!(c.skip.load(Ordering::SeqCst));
        assert_eq!(c.step.available_permits(), 1);
    }

    #[test]
    fn set_paused_on_stores_value() {
        let c = ctrl();
        let p = PausedOn::Action {
            variant: "Init",
            index: 1,
            of: 1,
        };
        c.set_paused_on(Some(p));
        assert!(c.paused_on.load_full().is_some());
        c.set_paused_on(None);
        assert!(c.paused_on.load_full().is_none());
    }

    #[test]
    fn publish_noop_when_debug_mode_off() {
        let c = ctrl();
        let h = crate::history::DebugHistory::new(SystemState::initial());
        let seq_before = c.snapshot.load_full().seq;
        c.publish(&h); // should be noop
        assert_eq!(c.snapshot.load_full().seq, seq_before);
    }

    #[test]
    fn publish_updates_snapshot_when_debug_mode_on() {
        let c = ctrl();
        c.enable_debug();
        let mut h = crate::history::DebugHistory::new(SystemState::initial());
        h.seq = 42;
        c.publish(&h);
        assert_eq!(c.snapshot.load_full().seq, 42);
        assert!(c.snapshot.load_full().debug_mode);
    }

    #[test]
    fn publish_paused_noop_when_debug_mode_off() {
        let c = ctrl();
        let seq_before = c.snapshot.load_full().seq;
        c.publish_paused();
        assert_eq!(c.snapshot.load_full().seq, seq_before);
    }

    #[test]
    fn publish_paused_bumps_seq_when_debug_mode_on() {
        let c = ctrl();
        c.enable_debug();
        let seq_before = c.snapshot.load_full().seq;
        c.publish_paused();
        assert_eq!(c.snapshot.load_full().seq, seq_before + 1);
    }

    #[test]
    fn disable_debug_publishes_final_snapshot_with_debug_mode_false() {
        let c = ctrl();
        c.enable_debug();
        // Publish a snapshot with debug_mode=true (via history).
        let h = crate::history::DebugHistory::new(SystemState::initial());
        c.publish(&h);
        assert!(c.snapshot.load_full().debug_mode);
        c.disable_debug();
        assert!(!c.snapshot.load_full().debug_mode);
    }

    #[test]
    fn make_http_observer_noop_when_debug_mode_off() {
        let c = ctrl();
        let observer = c.make_http_observer();
        // Should not panic and should be a no-op.
        observer(windlass_types::HttpExchange {
            module: "test".to_string(),
            method: "GET".to_string(),
            url: "http://example.com".to_string(),
            request_body: None,
            response_status: 200,
            response_body: "ok".to_string(),
        });
    }

    #[tokio::test]
    async fn acquire_step_returns_true_when_permit_available() {
        let c = ctrl();
        c.release_step();
        assert!(c.acquire_step().await);
    }

    #[tokio::test]
    async fn acquire_step_returns_false_when_skip_requested() {
        let c = ctrl();
        c.request_skip();
        assert!(!c.acquire_step().await);
    }

    #[tokio::test]
    async fn make_http_observer_sends_exchange_when_debug_mode_on() {
        let (c, mut owned) = DebugController::new_with_owned();
        c.enable_debug();
        let observer = c.make_http_observer();
        let exchange = windlass_types::HttpExchange {
            module: "test".to_string(),
            method: "GET".to_string(),
            url: "http://example.com".to_string(),
            request_body: None,
            response_status: 200,
            response_body: "ok".to_string(),
        };
        let action_id = uuid::Uuid::new_v4();
        causal_tx::CURRENT_ACTION_ID
            .scope(Some(action_id), async { observer(exchange) })
            .await;
        let received = owned
            .exchange_rx
            .try_recv()
            .expect("exchange should be sent");
        assert_eq!(received.0, action_id);
        assert_eq!(received.1.url, "http://example.com");
    }
}
