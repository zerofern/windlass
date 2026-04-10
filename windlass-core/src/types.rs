use serde::Serialize;
use std::collections::HashSet;
use std::fmt;
use windlass_types::{AuthCookie, RetryCount, TorrentName, VpnIp, VpnPort};

/// Serializes any value as the string `"[redacted]"`.
mod redact {
    use serde::Serializer;
    pub fn serialize<T, S: Serializer>(_: &T, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("[redacted]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum QbitState {
    Offline,
    Authenticating {
        attempt: RetryCount,
    },
    Authenticated {
        #[serde(serialize_with = "redact::serialize")]
        cookie: AuthCookie,
    },
    SyncingPort {
        attempt: RetryCount,
        #[serde(serialize_with = "redact::serialize")]
        cookie: AuthCookie,
        target: VpnPort,
    },
    Ready {
        port: VpnPort,
        #[serde(serialize_with = "redact::serialize")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct SystemState {
    pub vpn: VpnState,
    pub qbit: QbitState,
    pub mam: MamState,
    pub known_torrents: HashSet<TorrentName>,
    #[serde(skip)]
    pub(crate) version: u64,
}

impl PartialEq for SystemState {
    fn eq(&self, other: &Self) -> bool {
        self.vpn == other.vpn
            && self.qbit == other.qbit
            && self.mam == other.mam
            && self.known_torrents == other.known_torrents
    }
}

impl Eq for SystemState {}

impl SystemState {
    pub(crate) const fn mark_changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    #[must_use]
    pub fn initial() -> Self {
        Self {
            vpn: VpnState::Stopped,
            qbit: QbitState::Offline,
            mam: MamState::Unknown,
            known_torrents: HashSet::new(),
            version: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use windlass_types::{AuthCookie, RetryCount};

    fn ip() -> VpnIp {
        VpnIp(Ipv4Addr::new(10, 8, 0, 1))
    }

    fn port() -> VpnPort {
        VpnPort::try_new(51820).unwrap()
    }

    fn cookie() -> AuthCookie {
        AuthCookie("sid".to_string())
    }

    #[test]
    fn vpn_state_display_stopped() {
        assert_eq!(VpnState::Stopped.to_string(), "stopped");
    }

    #[test]
    fn vpn_state_display_dumping_logs() {
        assert_eq!(VpnState::DumpingLogs.to_string(), "dumping-logs");
    }

    #[test]
    fn vpn_state_display_starting() {
        assert_eq!(VpnState::Starting.to_string(), "starting");
    }

    #[test]
    fn vpn_state_display_awaiting_tunnel() {
        assert_eq!(VpnState::AwaitingTunnel.to_string(), "awaiting-tunnel");
    }

    #[test]
    fn vpn_state_display_connected() {
        let s = VpnState::Connected {
            ip: ip(),
            port: port(),
        };
        assert_eq!(s.to_string(), "connected(10.8.0.1:51820)");
    }

    #[test]
    fn qbit_state_display_offline() {
        assert_eq!(QbitState::Offline.to_string(), "offline");
    }

    #[test]
    fn qbit_state_display_authenticating() {
        let s = QbitState::Authenticating {
            attempt: RetryCount(2),
        };
        assert_eq!(s.to_string(), "authenticating(#2)");
    }

    #[test]
    fn qbit_state_display_authenticated() {
        let s = QbitState::Authenticated { cookie: cookie() };
        assert_eq!(s.to_string(), "authenticated");
    }

    #[test]
    fn qbit_state_display_syncing_port() {
        let s = QbitState::SyncingPort {
            attempt: RetryCount(1),
            cookie: cookie(),
            target: port(),
        };
        assert_eq!(s.to_string(), "syncing-port(51820:#1)");
    }

    #[test]
    fn qbit_state_display_ready() {
        let s = QbitState::Ready {
            port: port(),
            cookie: cookie(),
        };
        assert_eq!(s.to_string(), "ready(51820)");
    }

    #[test]
    fn qbit_state_authenticated_serializes_cookie_as_redacted() {
        let s = QbitState::Authenticated { cookie: cookie() };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("[redacted]"));
        assert!(!json.contains("sid"));
    }

    #[test]
    fn qbit_state_ready_serializes_cookie_as_redacted() {
        let s = QbitState::Ready {
            port: port(),
            cookie: cookie(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("[redacted]"));
    }

    #[test]
    fn mam_state_display_unknown() {
        assert_eq!(MamState::Unknown.to_string(), "unknown");
    }

    #[test]
    fn mam_state_display_sync_pending() {
        let s = MamState::SyncPending {
            target_ip: ip(),
            target_port: port(),
        };
        assert_eq!(s.to_string(), "sync-pending(10.8.0.1)");
    }

    #[test]
    fn mam_state_display_synced() {
        let s = MamState::Synced {
            ip: ip(),
            port: port(),
        };
        assert_eq!(s.to_string(), "synced(10.8.0.1:51820)");
    }

    #[test]
    fn mam_state_display_asn_blocked() {
        let s = MamState::AsnBlocked { ip: ip() };
        assert_eq!(s.to_string(), "asn-blocked(10.8.0.1)");
    }
}
