use serde::Serialize;
use windlass_types::{AlertPriority, AuthCookie, VpnIp, VpnPort, WakeupId};
use std::time::Duration;

mod serde_duration_secs {
    pub(super) fn serialize<S: serde::Serializer>(
        d: &std::time::Duration,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }
}

/// Everything the Core can ask the Shell to do.
///
/// Actions are intentionally semantic — the Shell owns all targeting logic.
/// The Core never passes container lists or hardcoded IDs.
#[derive(Debug, Clone, Serialize)]
pub enum Action {
    /// Sleep for `duration`, then emit `Wakeup(id)` into the event channel.
    /// The Shell MUST cancel any existing timer for the same `WakeupId` before
    /// spawning a new one to prevent timer leaks.
    ScheduleWakeup(WakeupId, #[serde(serialize_with = "serde_duration_secs::serialize")] Duration),

    /// Read both `/tmp/gluetun/ip` and `/tmp/gluetun/forwarded_port`.
    /// Shell returns `PortFileReadResult(Ok/Err)`.
    ReadPortFiles,

    /// Dump logs for all discovered dependent containers + Gluetun itself.
    FetchAndDumpAllLogs,

    /// Stop all discovered dependent containers (not Gluetun).
    StopDependentContainers,

    /// Start all discovered dependent containers (not Gluetun).
    StartDependentContainers,

    /// Restart the Gluetun container via the Docker API.
    RestartGluetun,

    AuthenticateQbit,
    SyncQbitPort(AuthCookie, VpnPort),

    /// Uses `vpn_client` (proxied through Gluetun) to protect real IP.
    UpdateMam(VpnIp),
    /// Uses `vpn_client`.
    CheckMamConnectability,

    CheckDiskSpace,
    CheckNewTorrents(AuthCookie),

    SendGotifyAlert(AlertPriority, String),
}
