use windlass_types::{AuthCookie, RetryCount, TorrentName, VpnIp, VpnPort};
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
            Self::Stopped => f.write_str("stopped"),
            Self::DumpingLogs => f.write_str("dumping-logs"),
            Self::Starting => f.write_str("starting"),
            Self::AwaitingTunnel => f.write_str("awaiting-tunnel"),
            Self::Connected { ip, port } => write!(f, "connected({}:{})", ip.0, port.into_inner()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QbitState {
    Offline,
    Authenticating {
        attempt: RetryCount,
    },
    Authenticated {
        cookie: AuthCookie,
    },
    SyncingPort {
        attempt: RetryCount,
        cookie: AuthCookie,
        target: VpnPort,
    },
    Ready {
        port: VpnPort,
        cookie: AuthCookie,
    },
}

impl fmt::Display for QbitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => f.write_str("offline"),
            Self::Authenticating { attempt } => write!(f, "authenticating(#{})", attempt.0),
            Self::Authenticated { .. } => f.write_str("authenticated"),
            Self::SyncingPort {
                attempt, target, ..
            } => write!(f, "syncing-port({}:#{})", target.into_inner(), attempt.0),
            Self::Ready { port, .. } => write!(f, "ready({})", port.into_inner()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MamState {
    Unknown,
    SyncPending {
        target_ip: VpnIp,
        target_port: VpnPort,
    },
    Synced {
        port: VpnPort,
        ip: VpnIp,
    },
    AsnBlocked {
        ip: VpnIp,
    },
}

impl fmt::Display for MamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("unknown"),
            Self::SyncPending { target_ip, .. } => write!(f, "sync-pending({})", target_ip.0),
            Self::Synced { ip, port } => write!(f, "synced({}:{})", ip.0, port.into_inner()),
            Self::AsnBlocked { ip } => write!(f, "asn-blocked({})", ip.0),
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
    pub known_torrents: HashSet<TorrentName>,
}

impl SystemState {
    #[must_use]
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
