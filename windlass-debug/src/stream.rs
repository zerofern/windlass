use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use windlass_core::Observation;
use windlass_core::actions::Action;
use windlass_core::events::Event;

use super::{DebugController, PausedOn};

// ── DebuggableEventStream ─────────────────────────────────────────────────────

/// Wraps the external mpsc receiver with debug-mode pause/step logic.
///
/// An intake task drains the external channel, broadcasting
/// `Observation::EventArrived` for every event so the UI can see what's
/// queued in real time, then forwards events to an internal channel.
/// The main loop calls [`recv`](DebuggableEventStream::recv) which pops from
/// the internal channel and pauses when debug mode is active or a breakpoint
/// is hit.
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

            if matches!(event, Event::MamRateLimitViolation { .. }) {
                warn!("MAM rate-limit violation detected — entering debug mode");
                self.debug_ctrl.enable_debug(self.obs_tx.clone());
                let _ = self.obs_tx.send(Observation::DebugModeChanged(true));
            }

            if self.debug_ctrl.should_pause_on_event(event_variant(&event)) {
                self.debug_ctrl.set_paused_on(Some(PausedOn::Event {
                    variant: event_variant(&event),
                }));
                let execute = self.debug_ctrl.acquire_step().await;
                self.debug_ctrl.set_paused_on(None);
                if !execute {
                    continue; // skipped — fetch the next event
                }
            }

            return Some(event);
        }
    }
}

// ── Variant name helpers ──────────────────────────────────────────────────────

const fn event_variant(event: &Event) -> &'static str {
    match event {
        Event::Init { .. } => "Init",
        Event::DockerGluetunDied { .. } => "DockerGluetunDied",
        Event::DockerGluetunHealthy { .. } => "DockerGluetunHealthy",
        Event::PortFileReadResult { .. } => "PortFileReadResult",
        Event::QbitAuthSuccess { .. } => "QbitAuthSuccess",
        Event::QbitAuthFailed { .. } => "QbitAuthFailed",
        Event::QbitConnectionRefused { .. } => "QbitConnectionRefused",
        Event::QbitApiError { .. } => "QbitApiError",
        Event::QbitPortSyncSuccess { .. } => "QbitPortSyncSuccess",
        Event::QbitPortSyncFailed { .. } => "QbitPortSyncFailed",
        Event::MamUpdateSuccess { .. } => "MamUpdateSuccess",
        Event::MamAsnMismatch { .. } => "MamAsnMismatch",
        Event::MamStatusObserved { .. } => "MamStatusObserved",
        Event::DiskSpaceObserved { .. } => "DiskSpaceObserved",
        Event::NewTorrentsObserved { .. } => "NewTorrentsObserved",
        Event::LogsDumped { .. } => "LogsDumped",
        Event::Wakeup { .. } => "Wakeup",
        Event::MamRateLimitViolation { .. } => "MamRateLimitViolation",
    }
}

/// Returns the variant name of an [`Action`] as a static string.
/// Used to look up breakpoints and populate [`PausedOn`] without heap allocation.
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
