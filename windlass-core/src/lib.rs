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
use windlass_types::MamStatus;

/// The pure functional core. No I/O, no async, no side effects.
/// All state transitions and action scheduling happen here.
impl SystemState {
    pub fn process_event(&mut self, event: Event, now: DateTime<Utc>) -> EventOutcome {
        let _ = now;
        let before_version = self.version;

        #[cfg(debug_assertions)]
        let before_state = self.clone();

        let actions = match event {
            // ── Initialisation ────────────────────────────────────────────────
            Event::Init {
                is_gluetun_healthy,
                port_files,
                ..
            } => self.on_init(is_gluetun_healthy, port_files),

            // ── Workflow A: VPN Drop Recovery ─────────────────────────────────
            Event::DockerGluetunDied { .. } => self.on_docker_gluetun_died(),
            Event::LogsDumped { .. } => self.on_logs_dumped(),
            Event::DockerGluetunHealthy { .. } => self.on_docker_gluetun_healthy(),

            // ── Workflow B: Port Sync & Tracker Update ────────────────────────
            Event::PortFileReadResult {
                result: Ok((ip, port)),
                ..
            } => self.on_port_file_read_ok(ip, port),
            Event::PortFileReadResult { result: Err(e), .. } => handlers::on_port_file_read_err(&e),
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
            Event::MamStatusObserved {
                status: MamStatus::NotConnectable | MamStatus::Unreachable,
                ..
            } => self.on_mam_not_connectable(),

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
