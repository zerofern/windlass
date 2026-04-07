#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod actions;
pub mod events;
pub mod observation;
pub mod types;

pub use observation::{HttpObserver, Observation};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod prop_tests;

mod handlers;

use actions::Action;
use events::Event;
use types::SystemState;
use windlass_types::MamStatus;

/// The pure functional core. No I/O, no async, no side effects.
/// All state transitions and action scheduling happen here.
impl SystemState {
    pub fn process_event(&mut self, event: Event) -> Vec<Action> {
        match event {
            // ── Initialisation ────────────────────────────────────────────────
            Event::Init {
                is_gluetun_healthy,
                port_files,
            } => self.on_init(is_gluetun_healthy, port_files),

            // ── Workflow A: VPN Drop Recovery ─────────────────────────────────
            Event::DockerGluetunDied => self.on_docker_gluetun_died(),
            Event::LogsDumped => self.on_logs_dumped(),
            Event::DockerGluetunHealthy => self.on_docker_gluetun_healthy(),

            // ── Workflow B: Port Sync & Tracker Update ────────────────────────
            Event::PortFileReadResult(Ok((ip, port))) => self.on_port_file_read_ok(ip, port),
            Event::PortFileReadResult(Err(e)) => handlers::on_port_file_read_err(&e),
            Event::QbitAuthSuccess(cookie) => self.on_qbit_auth_success(cookie),
            Event::QbitConnectionRefused => self.on_qbit_connection_refused(),
            Event::QbitAuthFailed => self.on_qbit_auth_failed(),
            Event::QbitApiError(code) => self.on_qbit_api_error(code),
            Event::QbitPortSyncSuccess => self.on_qbit_port_sync_success(),
            Event::QbitPortSyncFailed(code) => self.on_qbit_port_sync_failed(code),

            // ── MAM ───────────────────────────────────────────────────────────
            Event::MamUpdateSuccess => self.on_mam_update_success(),
            Event::MamAsnMismatch(ip) => self.on_mam_asn_mismatch(ip),

            // ── Workflow C: Heartbeat & Recovery ──────────────────────────────
            Event::MamStatusObserved(MamStatus::Connectable) => self.on_mam_connectable(),
            Event::MamStatusObserved(MamStatus::NotConnectable | MamStatus::Unreachable) => {
                self.on_mam_not_connectable()
            }

            // ── Monitoring ────────────────────────────────────────────────────
            Event::DiskSpaceObserved(space) => handlers::on_disk_space_observed(space),
            Event::NewTorrentsObserved(current) => self.on_new_torrents_observed(current),
            Event::Wakeup(id) => self.on_wakeup(id),
            Event::MamRateLimitViolation => handlers::on_mam_rate_limit_violation(),
        }
    }
}
