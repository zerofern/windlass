#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod actions;
pub mod events;
pub mod observation;
pub mod torrent;
pub mod types;

pub use observation::{HttpObserver, Observation};

/// Returned by [`SystemState::process_event`].
pub struct EventOutcome {
    pub actions: Vec<Action>,
    /// `true` if the state changed as a result of this event.
    /// The shell uses this to avoid cloning and broadcasting a `StateSnapshot`
    /// when nothing has actually changed (e.g. no-op wakeups).
    pub state_changed: bool,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod prop_tests;

mod handlers;

use actions::Action;
use chrono::{DateTime, Utc};
use events::Event;
use types::SystemState;
use windlass_types::WakeupId;

/// The pure functional core. No I/O, no async, no side effects.
/// All state transitions and action scheduling happen here.
impl SystemState {
    pub fn process_event(&mut self, event: Event, now: DateTime<Utc>) -> EventOutcome {
        let _ = now;
        let before_version = self.version;

        #[cfg(debug_assertions)]
        let before_state = self.clone();

        let actions = match event {
            // ── §36 step 1: legacy VPN handlers retired ───────────────────────
            // Initialisation, Workflow A (VPN drop recovery), and the
            // Workflow B port-file event are now owned by the new cores:
            //   * `service_events.rs` translates these legacy events into
            //     `VpnEvent::Init / ContainerHealthy / ContainerUnhealthy /
            //     PortFileChanged / StateReadFailed` for `VpnMachine`.
            //   * Crash recovery (log dump, fleet stop/restart/start,
            //     "Gluetun died" Critical) is driven by §38's domain
            //     DOM-27/DOM-28 path on `VpnPublish::Crashed/Recovered`.
            // Legacy `state.vpn` stays at `VpnState::Stopped`; remaining
            // legacy handlers that gate on `VpnState::Connected` (qbit.rs,
            // mam.rs) no-op those branches.  Those handlers are retired in
            // §36 steps 2-5.
            Event::Init { .. }
            | Event::DockerGluetunDied { .. }
            | Event::LogsDumped { .. }
            | Event::DockerGluetunHealthy { .. }
            | Event::PortFileReadResult { .. } => Vec::new(),
            // ── §36 step 3: legacy qBit handlers retired ──────────────────────
            // `service_events.rs` translates these events into
            // `QbitEvent::AuthSucceeded / AuthFailed / AuthRejected /
            // ListenPortSet / ListenPortSetFailed / PreferencesRead /
            // PreferencesFailed` for `QbitMachine`.  Credentials-rejection
            // Critical alert: domain DOM-30 on `QbitPublish::AuthRejected`.
            // Port-sync persistent-failure Warning: domain DOM-31 on
            // `QbitPublish::ListenPortPersistentFailure`.  Activity
            // entries (`qbit_authenticated`, `port_synced`): domain
            // DOM-29/DOM-32 rising-edge on Ready / ListenPortReady.
            // `max_active_torrents` arrives via QbitMachine's own
            // `ReadPreferences` action and is stored on the machine —
            // separate from the legacy event and not affected by this
            // retirement.
            Event::QbitAuthSuccess { .. }
            | Event::QbitConnectionRefused { .. }
            | Event::QbitAuthFailed { .. }
            | Event::QbitApiError { .. }
            | Event::QbitPortSyncSuccess { .. }
            | Event::QbitPortSyncFailed { .. } => Vec::new(),

            // ── §36 step 2: legacy MAM handlers retired ───────────────────────
            // `service_events.rs` translates these events into
            // `MamEvent::SeedboxUpdated / StatusFailed / StatusFetched /
            // Unreachable / RateLimited` for `MamMachine`.  Critical alerts
            // (ASN mismatch, NotConnectable, Unreachable) are emitted by
            // domain on `MamPublish::*` via DOM-15/16/17/20.  The legacy
            // "NAT frozen" hard-recovery path is intentionally retired —
            // §38's DOM-27 owns Gluetun restart on real crashes, and the
            // operator no longer needs MAM-NotConnectable to also restart.
            Event::MamUpdateSuccess { .. }
            | Event::MamAsnMismatch { .. }
            | Event::MamStatusObserved { .. }
            | Event::MamUnreachable { .. } => Vec::new(),

            // ── §36 step 4: legacy monitoring handlers retired ────────────────
            // `service_events.rs` bridges `Event::DiskSpaceObserved` to
            // `DiskEvent::DiskSpaceObserved` for `DiskMachine`; domain DOM-9
            // emits the Warning alert and EvictOneForDiskPressure.  The
            // `Event::NewTorrentsObserved` legacy poll has no new-path
            // equivalent — QbitMachine's `TorrentRefresh` chain drives the
            // canonical torrent feed and publishes `NewTorrentsAdded`
            // (DOM-33 fires the Info alert).  `Event::MamRateLimitViolation`
            // routes via the bridge to `MamEvent::RateLimited` →
            // `MamPublish::RateLimited` → DOM-34 (Critical alert).
            // `Event::Wakeup` is now a no-op: every legacy `WakeupId` has
            // either a self-driving timer in the relevant core or no
            // remaining consumer.
            Event::DiskSpaceObserved { .. }
            | Event::NewTorrentsObserved { .. }
            | Event::Wakeup { .. }
            | Event::MamRateLimitViolation { .. } => Vec::new(),

            // ── Compliance ────────────────────────────────────────────────────
            Event::QbitTorrentDetailsReceived { torrents, .. } => {
                self.on_qbit_torrent_details_received(torrents)
            }
            // §36 step 3: legacy `state.max_active_torrents` updates
            // retired.  `QbitMachine` reads max_active_torrents from its
            // own `ReadPreferences` action.  Legacy `compliance.rs::
            // check_active_limit` will see `state.max_active_torrents = 5`
            // (SystemState::initial default) until compliance.rs is
            // retired in §36 step 7; in practice this means legacy
            // queue orchestration is inert with fewer than 5 active
            // torrents — the new core's QBIT-14/15/16 path is authoritative.
            Event::QbitPreferencesReceived { .. } | Event::QbitPreferencesFailed { .. } => {
                Vec::new()
            }
            Event::DeleteTorrentRequested { hash, .. } => self.on_delete_torrent_requested(hash),

            // ── §36 step 5: legacy manual-download handlers retired ───────────
            // The web route now sends `WindlassCommand::ManualDownload`
            // directly to the domain runtime, bypassing the legacy event
            // channel entirely.  These event variants stay for the debug
            // history shape but never fire from production code paths
            // and produce no actions when bridged from tests.
            Event::ManualDownloadRequested { .. }
            | Event::TorrentAddedToQbit { .. }
            | Event::TorrentAddFailed { .. } => Vec::new(),
        };

        let actions = retire_service_orchestration(actions);
        let state_changed = self.version != before_version;

        #[cfg(debug_assertions)]
        debug_assert!(
            *self == before_state || state_changed,
            "state changed but mark_changed() was not called"
        );

        EventOutcome {
            actions,
            state_changed,
        }
    }
}

fn retire_service_orchestration(actions: Vec<Action>) -> Vec<Action> {
    actions
        .into_iter()
        .filter(|action| !is_service_orchestration_action(action))
        .collect()
}

const fn is_service_orchestration_action(action: &Action) -> bool {
    matches!(
        action,
        Action::AuthenticateQbit
            | Action::SyncQbitPort(_, _)
            | Action::UpdateMam(_)
            | Action::CheckMamConnectability
            | Action::CheckNewTorrents(_)
            | Action::ScheduleWakeup(
                WakeupId::QbitAuthRetry
                    | WakeupId::QbitSyncRetry
                    | WakeupId::Heartbeat
                    | WakeupId::TorrentCheck,
                _
            )
    )
}
