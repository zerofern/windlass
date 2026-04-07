use crate::actions::Action;
use crate::types::{MamState, QbitState, SystemState, VpnState};
use tracing::{debug, info, warn};
use windlass_types::{AlertPriority, RetryCount, VpnIp, WakeupId};

use super::HEARTBEAT_INTERVAL;

impl SystemState {
    pub(crate) fn on_mam_update_success(&mut self) -> Vec<Action> {
        if let VpnState::Connected { ip, port } = &self.vpn {
            let (ip, port) = (*ip, *port);
            info!(ip = %ip.0, port = port.into_inner(), "MAM seedbox registered — VPN recovery complete");
            self.mam = MamState::Synced { port, ip };
            return vec![Action::SendGotifyAlert(
                AlertPriority::Info,
                "✅ VPN Recovered. Port synced.".into(),
            )];
        }
        vec![]
    }

    pub(crate) fn on_mam_asn_mismatch(&mut self, ip: VpnIp) -> Vec<Action> {
        warn!(ip = %ip.0, "MAM ASN mismatch — manual IP whitelist required");
        self.mam = MamState::AsnBlocked { ip };
        vec![Action::SendGotifyAlert(
            AlertPriority::Critical,
            format!(
                "🚨 MAM ASN mismatch for {}. Log into MAM and whitelist the new IP manually.",
                ip.0
            ),
        )]
    }

    pub(crate) fn on_mam_connectable(&mut self) -> Vec<Action> {
        debug!(mam = %self.mam, "MAM reports connectable — heartbeat OK");
        vec![Action::ScheduleWakeup(
            WakeupId::Heartbeat,
            HEARTBEAT_INTERVAL.into(),
        )]
    }

    pub(crate) fn on_mam_not_connectable(&mut self) -> Vec<Action> {
        warn!(mam = %self.mam, qbit = %self.qbit, "MAM reports NOT connectable");
        // If ASN is blocked, a human must intervene. Don't attempt recovery.
        if let MamState::AsnBlocked { .. } = &self.mam {
            debug!("ASN blocked — suppressing recovery");
            return vec![];
        }

        match &self.qbit {
            // Soft recovery: assume qBit dropped the port, re-trigger Workflow B.
            QbitState::Ready { .. } | QbitState::Authenticated { .. } => {
                info!("soft recovery: re-triggering qBit auth");
                self.qbit = QbitState::Authenticating {
                    attempt: RetryCount(0),
                };
                vec![
                    Action::AuthenticateQbit,
                    Action::ScheduleWakeup(WakeupId::Heartbeat, HEARTBEAT_INTERVAL.into()),
                ]
            }
            // Hard recovery: NAT is frozen, restart the stack.
            // Death-loop prevention is the MAM rate-limit guardrail in the shell.
            _ => {
                warn!("hard recovery: NAT frozen — restarting stack");
                self.vpn = VpnState::DumpingLogs;
                vec![
                    Action::FetchAndDumpAllLogs,
                    Action::SendGotifyAlert(
                        AlertPriority::Critical,
                        "⚠️ NAT Frozen. Initiating Hard Recovery.".into(),
                    ),
                ]
            }
        }
    }
}
