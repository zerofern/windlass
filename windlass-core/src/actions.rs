use serde::Serialize;
use std::time::Duration;
use windlass_types::{
    AlertPriority, AuthCookie, MamTorrentId, TorrentHash, VpnIp, VpnPort, WakeupId,
};

use crate::torrent::TorrentRecord;

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
    ScheduleWakeup(
        WakeupId,
        #[serde(serialize_with = "serde_duration_secs::serialize")] Duration,
    ),

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

    SendAlert {
        priority: AlertPriority,
        title: String,
        body: String,
    },

    /// Fetches full torrent details from qBittorrent for compliance checking.
    FetchTorrentDetails(AuthCookie),
    /// Fetches qBittorrent application preferences (e.g. `max_active_torrents`).
    FetchQbitPreferences(AuthCookie),

    /// Pauses a torrent in qBittorrent.
    PauseTorrent(TorrentHash, AuthCookie),
    /// Force-resumes a torrent, bypassing qBit's seeding ratio/time limits.
    ForceResumeTorrent(TorrentHash, AuthCookie),
    /// Removes a torrent from qBittorrent without deleting the data files.
    DeleteTorrent(TorrentHash, AuthCookie),
    /// Sets all files in a torrent to normal priority (enforces MAM no-partials rule).
    SetAllFilesPriority(TorrentHash, AuthCookie),

    /// Persists torrent compliance records to the database.
    UpsertTorrentRecords(Vec<TorrentRecord>),
    /// Marks a MAM torrent ID as blacklisted in the download queue.
    BlacklistMamId(MamTorrentId),
    /// Writes a compliance or user action record to the activity_log table.
    WriteActivity {
        source: String,
        action: String,
        book_id: Option<i64>,
        detail: Option<String>,
    },

    /// Fetches a `.torrent` file from MAM and adds it to qBittorrent.
    /// Shell emits `TorrentAddedToQbit` on success or `TorrentAddFailed` on error.
    FetchAndAddTorrent {
        mam_id: MamTorrentId,
        cookie: AuthCookie,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_types::WakeupId;

    #[test]
    fn schedule_wakeup_serializes_duration_as_seconds() {
        let action = Action::ScheduleWakeup(WakeupId::RetryPortRead, Duration::from_secs(30));
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("30"), "expected seconds in JSON: {json}");
    }
}
