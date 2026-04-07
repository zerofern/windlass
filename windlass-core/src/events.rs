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
        is_gluetun_healthy: bool,
        /// Contents of the VPN ip+port files as of boot.
        /// `Err` means the files were absent or unparseable (Gluetun not yet up).
        port_files: Result<(VpnIp, VpnPort), String>,
    },

    DockerGluetunDied,
    DockerGluetunHealthy,

    /// Fired by the debounced file watcher after the inotify storm settles.
    /// The Shell reads both VPN files and embeds the result directly —
    /// no separate `ReadPortFiles` round-trip required.
    PortFileReadResult(Result<(VpnIp, VpnPort), String>),

    QbitAuthSuccess(AuthCookie),
    /// Credentials rejected by qBittorrent (`"Fails."` response).
    /// The Core treats this as a configuration error and alerts immediately.
    QbitAuthFailed,
    /// Network-level failure (connection refused, timeout) while reaching qBittorrent.
    /// Normal during container startup — the Core retries silently.
    QbitConnectionRefused,
    QbitApiError(HttpStatusCode),

    QbitPortSyncSuccess,
    QbitPortSyncFailed(HttpStatusCode),

    MamUpdateSuccess,
    MamAsnMismatch(VpnIp),
    MamStatusObserved(MamStatus),

    DiskSpaceObserved(#[serde(serialize_with = "serde_information::serialize")] Information),
    NewTorrentsObserved(Vec<TorrentName>),

    LogsDumped,

    Wakeup(WakeupId),

    /// The MAM rate limit safety guard triggered — a request was attempted less
    /// than 400ms after the previous one. The event loop handles this directly
    /// (freeze + alert) before it reaches the core.
    MamRateLimitViolation,
}
