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
use windlass_types::{MamStatus, WakeupId};

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
            Event::QbitAuthSuccess { cookie, .. } => self.on_qbit_auth_success(cookie),
            Event::QbitConnectionRefused { .. } => self.on_qbit_connection_refused(),
            Event::QbitAuthFailed { .. } => self.on_qbit_auth_failed(),
            Event::QbitApiError { code, .. } => self.on_qbit_api_error(code),
            Event::QbitPortSyncSuccess { .. } => self.on_qbit_port_sync_success(),
            Event::QbitPortSyncFailed { code, .. } => self.on_qbit_port_sync_failed(code),

            // ── MAM ───────────────────────────────────────────────────────────
            Event::MamUpdateSuccess { .. } => self.on_mam_update_success(),
            Event::MamAsnMismatch { ip, .. } => self.on_mam_asn_mismatch(ip),

            // ── Workflow C: Heartbeat & Recovery ──────────────────────────────
            Event::MamStatusObserved {
                status: MamStatus::Connectable,
                ..
            } => self.on_mam_connectable(),
            // §28: legacy bridge treats `Event::MamUnreachable` (new) the
            // same as the existing `MamStatus::NotConnectable | Unreachable`
            // bucket — degraded MAM service.  The new per-system MAM core
            // (which is retiring this legacy path per story 32) handles the
            // distinct Unreachable vs NotConnectable signals properly via
            // MAM-11/12 and DOM-15/16.
            Event::MamStatusObserved {
                status: MamStatus::NotConnectable | MamStatus::Unreachable,
                ..
            }
            | Event::MamUnreachable { .. } => self.on_mam_not_connectable(),

            // ── Monitoring ────────────────────────────────────────────────────
            Event::DiskSpaceObserved { space, .. } => handlers::on_disk_space_observed(space),
            Event::NewTorrentsObserved { torrents, .. } => self.on_new_torrents_observed(torrents),
            Event::Wakeup { id, .. } => self.on_wakeup(id),
            Event::MamRateLimitViolation { .. } => handlers::on_mam_rate_limit_violation(),

            // ── Compliance ────────────────────────────────────────────────────
            Event::QbitTorrentDetailsReceived { torrents, .. } => {
                self.on_qbit_torrent_details_received(torrents)
            }
            Event::QbitPreferencesReceived {
                max_active_torrents,
                ..
            } => self.on_qbit_preferences_received(max_active_torrents),
            Event::QbitPreferencesFailed { .. } => Vec::new(),
            Event::DeleteTorrentRequested { hash, .. } => self.on_delete_torrent_requested(hash),

            // ── Manual download ───────────────────────────────────────────────
            Event::ManualDownloadRequested { mam_id, .. } => {
                self.on_manual_download_requested(mam_id)
            }
            Event::TorrentAddedToQbit { mam_id, hash, at } => {
                handlers::on_torrent_added_to_qbit(mam_id, &hash, at)
            }
            Event::TorrentAddFailed { mam_id, reason, .. } => {
                handlers::on_torrent_add_failed(mam_id, &reason)
            }
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
