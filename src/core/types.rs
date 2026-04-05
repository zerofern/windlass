use crate::types::{AuthCookie, RetryCount, VpnIp, VpnPort};
use std::collections::HashSet;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    Active,
    Fatal { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VpnState {
    Stopped,
    DumpingLogs,
    Starting,
    AwaitingTunnel,
    Connected { ip: VpnIp, port: VpnPort },
}

impl fmt::Display for VpnState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VpnState::Stopped        => f.write_str("stopped"),
            VpnState::DumpingLogs    => f.write_str("dumping-logs"),
            VpnState::Starting       => f.write_str("starting"),
            VpnState::AwaitingTunnel => f.write_str("awaiting-tunnel"),
            VpnState::Connected { ip, port } =>
                write!(f, "connected({}:{})", ip.0, port.into_inner()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QbitState {
    Offline,
    Authenticating { attempt: RetryCount },
    Authenticated { cookie: AuthCookie },
    SyncingPort { attempt: RetryCount, cookie: AuthCookie, target: VpnPort },
    Ready { port: VpnPort },
}

impl fmt::Display for QbitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QbitState::Offline                          => f.write_str("offline"),
            QbitState::Authenticating { attempt }       => write!(f, "authenticating(#{})", attempt.0),
            QbitState::Authenticated { .. }             => f.write_str("authenticated"),
            QbitState::SyncingPort { attempt, target, .. } =>
                write!(f, "syncing-port({}:#{})", target.into_inner(), attempt.0),
            QbitState::Ready { port }                   => write!(f, "ready({})", port.into_inner()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MamState {
    Unknown,
    SyncPending { target_ip: VpnIp, target_port: VpnPort },
    Synced { port: VpnPort, ip: VpnIp },
    AsnBlocked { ip: VpnIp },
}

impl fmt::Display for MamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MamState::Unknown                          => f.write_str("unknown"),
            MamState::SyncPending { target_ip, .. }    => write!(f, "sync-pending({})", target_ip.0),
            MamState::Synced { ip, port }              => write!(f, "synced({}:{})", ip.0, port.into_inner()),
            MamState::AsnBlocked { ip }                => write!(f, "asn-blocked({})", ip.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemState {
    pub run_mode: RunMode,
    pub hard_recoveries: RetryCount,
    pub vpn: VpnState,
    pub qbit: QbitState,
    pub mam: MamState,
    /// Full list of torrent names seen so far. Used by the Core to diff
    /// against the raw list from `NewTorrentsObserved` and only alert on new ones.
    pub known_torrents: HashSet<String>,
}

impl SystemState {
    pub fn initial() -> Self {
        Self {
            run_mode: RunMode::Active,
            hard_recoveries: RetryCount(0),
            vpn: VpnState::Stopped,
            qbit: QbitState::Offline,
            mam: MamState::Unknown,
            known_torrents: HashSet::new(),
        }
    }
}
