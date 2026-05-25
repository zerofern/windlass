use chrono::Utc;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use uuid::Uuid;
use windlass_core::Observation;
use windlass_core::actions::Action;
use windlass_core::events::Event;

use super::{DebugController, PausedOn, StoredEvent};

// ── QueueSink ─────────────────────────────────────────────────────────────────

/// Controls where the intake task routes incoming events.
///
/// - `Mpsc`: debug mode off — forward the raw `Event` directly to the main loop.
/// - `Queue`: debug mode on — stamp the event as a `StoredEvent` and forward
///   to the `VecDeque` path so it accumulates in `DebugHistory.event_queue`.
///
/// Stored inside an `ArcSwap` so `enable_debug()`/`disable_debug()` can swap
/// the sink atomically without stopping the intake task.
///
/// `mpsc::Sender` does not implement `Debug`, so we implement it manually.
pub enum QueueSink {
    Mpsc(mpsc::Sender<Event>),
    Queue(mpsc::Sender<StoredEvent>),
}

impl std::fmt::Debug for QueueSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mpsc(_) => write!(f, "QueueSink::Mpsc"),
            Self::Queue(_) => write!(f, "QueueSink::Queue"),
        }
    }
}

// ── DebuggableEventStream ─────────────────────────────────────────────────────

/// Wraps the external mpsc receiver with debug-mode pause/step logic.
///
/// An intake task drains the external channel and routes events to either the
/// direct internal channel (non-debug) or the `StoredEvent` queue channel
/// (debug mode), based on the current [`QueueSink`].
///
/// In non-debug mode the main loop calls [`recv`](DebuggableEventStream::recv)
/// which pops from the internal channel and pauses when a breakpoint is hit.
/// In debug mode the main loop drives the queue directly.
pub struct DebuggableEventStream {
    internal_rx: mpsc::Receiver<Event>,
    debug_ctrl: DebugController,
    /// Kept solely for the `MamRateLimitViolation` auto-enable path,
    /// which needs to broadcast `DebugModeChanged(true)` to SSE subscribers.
    obs_tx: broadcast::Sender<Observation>,
}

impl DebuggableEventStream {
    /// Creates the stream, spawns the intake task, and enables debug mode
    /// immediately if `DEBUG_MODE_ON_START=true`.
    ///
    /// `internal_rx` must be the receiver end of the channel whose sender is
    /// stored in `DebugController` as `internal_tx` (created in `new_with_owned`).
    pub fn new(
        external_rx: mpsc::Receiver<Event>,
        internal_rx: mpsc::Receiver<Event>,
        debug_ctrl: DebugController,
        obs_tx: broadcast::Sender<Observation>,
    ) -> Self {
        if std::env::var("DEBUG_MODE_ON_START").is_ok_and(|v| v == "true") {
            debug_ctrl.enable_debug();
            info!("Debug mode enabled from DEBUG_MODE_ON_START");
        }

        let queue_sink = debug_ctrl.queue_sink.clone();
        tokio::spawn(async move {
            let mut rx = external_rx;
            while let Some(event) = rx.recv().await {
                match &**queue_sink.load() {
                    QueueSink::Mpsc(tx) => {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    QueueSink::Queue(tx) => {
                        let id = Uuid::new_v4();
                        let at = event.at();
                        let variant = event_variant(&event);
                        let payload =
                            serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
                        let stored = StoredEvent {
                            id,
                            at,
                            arrived_at: Utc::now(),
                            variant,
                            payload,
                            caused_by_action: None,
                            event,
                        };
                        if tx.send(stored).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Self {
            internal_rx,
            debug_ctrl,
            obs_tx,
        }
    }

    /// Returns the next event, pausing if a breakpoint is set for this event's
    /// variant. If the step is skipped the event is discarded and the next one
    /// is returned instead.
    ///
    /// Used only in **non-debug mode**. In debug mode the main loop reads
    /// directly from `DebugHistory.event_queue` via the queue channel.
    ///
    /// `MamRateLimitViolation` automatically enters debug mode before pausing.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            let event = self.internal_rx.recv().await?;

            if matches!(event, Event::MamRateLimitViolation { .. }) {
                warn!("MAM rate-limit violation detected — entering debug mode");
                self.debug_ctrl.enable_debug();
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

pub const fn event_variant(event: &Event) -> &'static str {
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
        Event::QbitTorrentDetailsReceived { .. } => "QbitTorrentDetailsReceived",
        Event::QbitPreferencesReceived { .. } => "QbitPreferencesReceived",
        Event::QbitPreferencesFailed { .. } => "QbitPreferencesFailed",
        Event::DeleteTorrentRequested { .. } => "DeleteTorrentRequested",
        Event::ManualDownloadRequested { .. } => "ManualDownloadRequested",
        Event::TorrentAddedToQbit { .. } => "TorrentAddedToQbit",
        Event::TorrentAddFailed { .. } => "TorrentAddFailed",
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
        Action::SendAlert { .. } => "SendAlert",
        Action::FetchTorrentDetails(_) => "FetchTorrentDetails",
        Action::FetchQbitPreferences(_) => "FetchQbitPreferences",
        Action::PauseTorrent(_, _) => "PauseTorrent",
        Action::ForceResumeTorrent(_, _) => "ForceResumeTorrent",
        Action::DeleteTorrent(_, _) => "DeleteTorrent",
        Action::SetAllFilesPriority(_, _) => "SetAllFilesPriority",
        Action::UpsertTorrentRecords(_) => "UpsertTorrentRecords",
        Action::BlacklistMamId(_) => "BlacklistMamId",
        Action::WriteActivity { .. } => "WriteActivity",
        Action::FetchAndAddTorrent { .. } => "FetchAndAddTorrent",
    }
}
