use chrono::{DateTime, Utc};
use serde::Serialize;
use windlass_types::{
    AuthCookie, HttpStatusCode, Information, MamStatus, TorrentName, VpnIp, VpnPort, WakeupId,
};

mod serde_information {
    use uom::si::information::byte;

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(super) fn serialize<S: serde::Serializer>(
        v: &uom::si::f64::Information,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_f64(v.get::<byte>())
    }
}

/// Everything the outside world (via the Shell) can tell the Core.
#[derive(Debug, Clone, Serialize)]
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
        #[serde(serialize_with = "serde_information::serialize")]
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
