#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::{Semaphore, broadcast, mpsc};
use tracing::{info, warn};
use windlass_core::Observation;
use windlass_core::actions::Action;
use windlass_core::events::Event;

// ── DebugState ────────────────────────────────────────────────────────────────

/// Serialisable snapshot of the current debug state, served by `GET /api/v1/debug`.
#[derive(Serialize)]
pub struct DebugState {
    pub debug_mode: bool,
    pub pending_actions: Vec<serde_json::Value>,
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

    /// Disable debug mode. Clears the obs channel and releases any blocked step
    /// waiters so the system resumes normal operation.
    ///
    /// # Panics
    /// Panics if any internal mutex is poisoned.
    pub fn disable_debug(&self) {
        self.0.debug_mode.store(false, Ordering::SeqCst);
        self.0.skip.store(false, Ordering::SeqCst);
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
            pending_actions: vec![], // populated in step 3 via ArcSwap
            event_breakpoints,
            action_breakpoints,
        }
    }
}

// ── DebuggableEventStream ─────────────────────────────────────────────────────

/// Wraps the external mpsc receiver with debug-mode pause/step logic.
///
/// Two concurrent tasks are always running:
/// - The **intake task** drains the external channel, broadcasting
///   `Observation::EventArrived` for every event so the UI can see the full
///   queue in real time, then forwards events to an internal channel.
/// - The **main loop** calls [`recv`](DebuggableEventStream::recv) which pops
///   from the internal channel and pauses when debug mode is active or a
///   breakpoint is hit.
pub struct DebuggableEventStream {
    internal_rx: mpsc::Receiver<Event>,
    debug_ctrl: DebugController,
    obs_tx: broadcast::Sender<Observation>,
}

impl DebuggableEventStream {
    /// Creates the stream, spawns the intake task, and enables debug mode
    /// immediately if `DEBUG_MODE_ON_START=true`.
    pub fn new(
        external_rx: mpsc::Receiver<Event>,
        debug_ctrl: DebugController,
        obs_tx: broadcast::Sender<Observation>,
    ) -> Self {
        if std::env::var("DEBUG_MODE_ON_START").is_ok_and(|v| v == "true") {
            debug_ctrl.enable_debug(obs_tx.clone());
            info!("Debug mode enabled from DEBUG_MODE_ON_START");
        }

        let (internal_tx, internal_rx) = mpsc::channel(128);
        let obs_tx_intake = obs_tx.clone();

        tokio::spawn(async move {
            let mut rx = external_rx;
            while let Some(event) = rx.recv().await {
                let _ = obs_tx_intake.send(Observation::EventArrived(event.clone()));
                if internal_tx.send(event).await.is_err() {
                    break;
                }
            }
        });

        Self {
            internal_rx,
            debug_ctrl,
            obs_tx,
        }
    }

    /// Returns the next event, pausing if debug mode is active or a breakpoint
    /// is set for this event's variant. If the step is skipped the event is
    /// discarded and the next one is returned instead.
    ///
    /// `MamRateLimitViolation` automatically enters debug mode before pausing —
    /// the event still reaches the core unchanged.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            let event = self.internal_rx.recv().await?;

            if matches!(event, Event::MamRateLimitViolation) {
                warn!("MAM rate-limit violation detected — entering debug mode");
                self.debug_ctrl.enable_debug(self.obs_tx.clone());
            }

            if self.debug_ctrl.should_pause_on_event(event_variant(&event))
                && !self.debug_ctrl.acquire_step().await
            {
                continue; // skipped — fetch the next event
            }

            return Some(event);
        }
    }
}

const fn event_variant(event: &Event) -> &'static str {
    match event {
        Event::Init { .. } => "Init",
        Event::ManualReset => "ManualReset",
        Event::DockerGluetunDied => "DockerGluetunDied",
        Event::DockerGluetunHealthy => "DockerGluetunHealthy",
        Event::PortFileReadResult(_) => "PortFileReadResult",
        Event::QbitAuthSuccess(_) => "QbitAuthSuccess",
        Event::QbitAuthFailed => "QbitAuthFailed",
        Event::QbitConnectionRefused => "QbitConnectionRefused",
        Event::QbitApiError(_) => "QbitApiError",
        Event::QbitPortSyncSuccess => "QbitPortSyncSuccess",
        Event::QbitPortSyncFailed(_) => "QbitPortSyncFailed",
        Event::MamUpdateSuccess => "MamUpdateSuccess",
        Event::MamAsnMismatch(_) => "MamAsnMismatch",
        Event::MamStatusObserved(_) => "MamStatusObserved",
        Event::DiskSpaceObserved(_) => "DiskSpaceObserved",
        Event::NewTorrentsObserved(_) => "NewTorrentsObserved",
        Event::LogsDumped => "LogsDumped",
        Event::Wakeup(_) => "Wakeup",
        Event::MamRateLimitViolation => "MamRateLimitViolation",
    }
}

/// Returns the variant name of an [`Action`] as a static string.
/// Used to look up breakpoints without heap allocation.
#[must_use]
pub const fn action_variant(action: &Action) -> &'static str {
    match action {
        Action::ScheduleWakeup(_, _) => "ScheduleWakeup",
        Action::ReadPortFiles => "ReadPortFiles",
        Action::FetchAndDumpAllLogs => "FetchAndDumpAllLogs",
        Action::StopDependentContainers => "StopDependentContainers",
        Action::StartDependentContainers => "StartDependentContainers",
        Action::RestartGluetun => "RestartGluetun",
        Action::AuthenticateQbit => "AuthenticateQbit",
        Action::SyncQbitPort(_, _) => "SyncQbitPort",
        Action::UpdateMam(_) => "UpdateMam",
        Action::CheckMamConnectability => "CheckMamConnectability",
        Action::CheckDiskSpace => "CheckDiskSpace",
        Action::CheckNewTorrents(_) => "CheckNewTorrents",
        Action::SendGotifyAlert(_, _) => "SendGotifyAlert",
    }
}
