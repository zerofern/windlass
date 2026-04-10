use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use windlass_types::{
    AuthCookie, HttpStatusCode, Information, MamStatus, TorrentName, VpnIp, VpnPort, WakeupId,
};

mod serde_information {
    use uom::si::f64::Information;
    use uom::si::information::byte;

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(super) fn serialize<S: serde::Serializer>(
        v: &Information,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_f64(v.get::<byte>())
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Information, D::Error> {
        use serde::Deserialize as _;
        let bytes = f64::deserialize(d)?;
        Ok(Information::new::<byte>(bytes))
    }
}

/// Everything the outside world (via the Shell) can tell the Core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Boot reconciliation. The Shell inspects Gluetun's actual health and
    /// reads the VPN files before emitting this, so the Core can fast-forward
    /// to the correct starting state immediately.
    Init {
        at: DateTime<Utc>,
        is_gluetun_healthy: bool,
        /// Contents of the VPN ip+port files as of boot.
        /// `Err` means the files were absent or unparseable (Gluetun not yet up).
        port_files: Result<(VpnIp, VpnPort), String>,
    },

    DockerGluetunDied {
        at: DateTime<Utc>,
    },
    DockerGluetunHealthy {
        at: DateTime<Utc>,
    },

    /// Fired by the debounced file watcher after the inotify storm settles.
    /// The Shell reads both VPN files and embeds the result directly —
    /// no separate `ReadPortFiles` round-trip required.
    PortFileReadResult {
        at: DateTime<Utc>,
        result: Result<(VpnIp, VpnPort), String>,
    },

    QbitAuthSuccess {
        at: DateTime<Utc>,
        cookie: AuthCookie,
    },
    /// Credentials rejected by qBittorrent (`"Fails."` response).
    /// The Core treats this as a configuration error and alerts immediately.
    QbitAuthFailed {
        at: DateTime<Utc>,
    },
    /// Network-level failure (connection refused, timeout) while reaching qBittorrent.
    /// Normal during container startup — the Core retries silently.
    QbitConnectionRefused {
        at: DateTime<Utc>,
    },
    QbitApiError {
        at: DateTime<Utc>,
        code: HttpStatusCode,
    },

    QbitPortSyncSuccess {
        at: DateTime<Utc>,
    },
    QbitPortSyncFailed {
        at: DateTime<Utc>,
        code: HttpStatusCode,
    },

    MamUpdateSuccess {
        at: DateTime<Utc>,
    },
    MamAsnMismatch {
        at: DateTime<Utc>,
        ip: VpnIp,
    },
    MamStatusObserved {
        at: DateTime<Utc>,
        status: MamStatus,
    },

    DiskSpaceObserved {
        at: DateTime<Utc>,
        #[serde(with = "serde_information")]
        space: Information,
    },
    NewTorrentsObserved {
        at: DateTime<Utc>,
        torrents: Vec<TorrentName>,
    },

    LogsDumped {
        at: DateTime<Utc>,
    },

    Wakeup {
        at: DateTime<Utc>,
        id: WakeupId,
    },

    /// The MAM rate limit safety guard triggered — a request was attempted less
    /// than 400ms after the previous one.
    MamRateLimitViolation {
        at: DateTime<Utc>,
    },
}

impl Event {
    #[must_use]
    pub const fn at(&self) -> DateTime<Utc> {
        match self {
            Self::Init { at, .. }
            | Self::DockerGluetunDied { at }
            | Self::DockerGluetunHealthy { at }
            | Self::PortFileReadResult { at, .. }
            | Self::QbitAuthSuccess { at, .. }
            | Self::QbitAuthFailed { at }
            | Self::QbitConnectionRefused { at }
            | Self::QbitApiError { at, .. }
            | Self::QbitPortSyncSuccess { at }
            | Self::QbitPortSyncFailed { at, .. }
            | Self::MamUpdateSuccess { at }
            | Self::MamAsnMismatch { at, .. }
            | Self::MamStatusObserved { at, .. }
            | Self::DiskSpaceObserved { at, .. }
            | Self::NewTorrentsObserved { at, .. }
            | Self::LogsDumped { at }
            | Self::Wakeup { at, .. }
            | Self::MamRateLimitViolation { at } => *at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uom::si::f64::Information;
    use uom::si::information::byte;

    #[test]
    fn disk_space_observed_roundtrips_through_json() {
        let at = Utc::now();
        let space = Information::new::<byte>(1_073_741_824.0); // 1 GiB
        let event = Event::DiskSpaceObserved { at, space };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        match back {
            Event::DiskSpaceObserved { space: s, .. } => {
                assert!((s.get::<byte>() - 1_073_741_824.0).abs() < 1.0);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn event_at_returns_correct_timestamp() {
        let at = Utc::now();
        let event = Event::MamRateLimitViolation { at };
        assert_eq!(event.at(), at);
    }

    #[test]
    fn init_event_at_returns_correct_timestamp() {
        use std::net::Ipv4Addr;
        use windlass_types::{VpnIp, VpnPort};
        let at = Utc::now();
        let event = Event::Init {
            at,
            is_gluetun_healthy: true,
            port_files: Ok((
                VpnIp(Ipv4Addr::new(10, 8, 0, 1)),
                VpnPort::try_new(51820).unwrap(),
            )),
        };
        assert_eq!(event.at(), at);
    }
}
