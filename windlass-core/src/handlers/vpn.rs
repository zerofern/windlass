use crate::actions::Action;
use crate::types::{MamState, QbitState, SystemState, VpnState};
use tracing::{debug, info, warn};
use windlass_types::{AlertPriority, RetryCount, VpnIp, VpnPort, WakeupId};

use super::{
    DISK_CHECK_INTERVAL, HEARTBEAT_INTERVAL, PORT_READ_RETRY_DELAY, TORRENT_CHECK_INTERVAL,
};

impl SystemState {
    pub(crate) fn on_init(
        &mut self,
        is_gluetun_healthy: bool,
        port_files: Result<(VpnIp, VpnPort), String>,
    ) -> Vec<Action> {
        info!(gluetun_healthy = is_gluetun_healthy, "initialising");
        let mut actions = vec![
            Action::ScheduleWakeup(WakeupId::Heartbeat, HEARTBEAT_INTERVAL.into()),
            Action::ScheduleWakeup(WakeupId::DiskCheck, DISK_CHECK_INTERVAL.into()),
            Action::ScheduleWakeup(WakeupId::TorrentCheck, TORRENT_CHECK_INTERVAL.into()),
        ];

        if is_gluetun_healthy {
            match port_files {
                Ok((ip, port)) => {
                    info!(ip = %ip.0, port = port.into_inner(), "boot: VPN already up, fast-forwarding");
                    self.vpn = VpnState::Connected { ip, port };
                    self.qbit = QbitState::Authenticating {
                        attempt: RetryCount(0),
                    };
                    actions.push(Action::AuthenticateQbit);
                }
                Err(e) => {
                    // Gluetun healthy but files not ready yet — watcher will fire soon.
                    debug!(err = %e, "boot: VPN files not yet readable, waiting for watcher");
                    self.vpn = VpnState::AwaitingTunnel;
                }
            }
        } else {
            self.vpn = VpnState::DumpingLogs;
            actions.push(Action::FetchAndDumpAllLogs);
        }

        actions
    }

    pub(crate) fn on_docker_gluetun_died(&mut self) -> Vec<Action> {
        let mut actions = vec![];
        match &self.vpn {
            // Unexpected crash — dump logs then stop dependents.
            VpnState::Connected { .. } | VpnState::AwaitingTunnel => {
                warn!(vpn = %self.vpn, qbit = %self.qbit, "Gluetun died unexpectedly — beginning recovery");
                self.vpn = VpnState::DumpingLogs;
                actions.push(Action::FetchAndDumpAllLogs);
                actions.push(Action::SendGotifyAlert(
                    AlertPriority::Critical,
                    "💀 Gluetun died unexpectedly. Dumping logs and recovering.".into(),
                ));
            }
            // Intentional restart from Hard Recovery — skip the dump.
            VpnState::Starting | VpnState::DumpingLogs => {
                debug!("Gluetun died during planned recovery — stopping dependents");
                actions.push(Action::StopDependentContainers);
            }
            VpnState::Stopped => {}
        }
        // qBit and MAM are unreachable until VPN is back.
        self.qbit = QbitState::Offline;
        self.mam = MamState::Unknown;
        actions
    }

    pub(crate) fn on_logs_dumped(&mut self) -> Vec<Action> {
        // Fires after both unexpected crashes and Hard Recovery dumps.
        // Always stop dependents and restart Gluetun — the double-dump guard
        // in DockerGluetunDied ensures we don't loop.
        self.vpn = VpnState::Starting;
        vec![Action::StopDependentContainers, Action::RestartGluetun]
    }

    pub(crate) fn on_docker_gluetun_healthy(&mut self) -> Vec<Action> {
        info!("Gluetun healthy — starting dependent containers");
        self.vpn = VpnState::AwaitingTunnel;
        // ReadPortFiles ensures recovery completes even when gluetun restarts
        // with the same IP/port — the file watcher deduplicates same-value writes
        // so an explicit read is needed to re-trigger Workflow B.
        vec![Action::StartDependentContainers, Action::ReadPortFiles]
    }

    // No-op if content is identical to current state — the debounced
    // watcher sends this event on every write; the Core ignores no-change reads.
    pub(crate) fn on_port_file_read_ok(&mut self, ip: VpnIp, port: VpnPort) -> Vec<Action> {
        if let VpnState::Connected {
            ip: cur_ip,
            port: cur_port,
        } = &self.vpn
        {
            if *cur_ip == ip && *cur_port == port {
                debug!(ip = %ip.0, port = port.into_inner(), "VPN files read: no change");
                return vec![];
            }
            info!(
                ip = %ip.0, port = port.into_inner(),
                old_ip = %cur_ip.0, old_port = cur_port.into_inner(),
                "VPN reconnected with new address"
            );
        } else {
            info!(ip = %ip.0, port = port.into_inner(), "VPN tunnel established");
        }
        self.vpn = VpnState::Connected { ip, port };
        self.qbit = QbitState::Authenticating {
            attempt: RetryCount(0),
        };
        vec![Action::AuthenticateQbit]
    }
}

pub fn on_port_file_read_err(e: &str) -> Vec<Action> {
    debug!(err = %e, "VPN port files not ready — scheduling retry");
    vec![Action::ScheduleWakeup(
        WakeupId::RetryPortRead,
        PORT_READ_RETRY_DELAY.into(),
    )]
}
