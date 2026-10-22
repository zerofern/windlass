#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use serde::Deserialize;
use windlass_types::{AuthCookie, MamTorrentId, TorrentName, VpnPort};

/// §36 step 9a: typed result for `QbitClient::authenticate`.
///
/// Replaces the legacy auth event shape so the shell can own protocol mapping
/// without depending on legacy core types.
#[derive(Debug, Clone)]
pub enum QbitAuthResult {
    Success(AuthCookie),
    /// Credentials rejected — the operator must fix `QBITTORRENT_USER`
    /// / `QBITTORRENT_PASS`.
    Rejected,
    /// Connection refused (qBit container starting up).  Silent retry.
    ConnectionRefused,
    /// Unexpected HTTP status from the auth endpoint.
    ApiError(u16),
}

/// §36 step 9a: typed result for `QbitClient::sync_port`.  Replaces the
/// legacy `windlass_core::Event::QbitPortSync{Success,Failed}` return.
#[derive(Debug, Clone, Copy)]
pub enum QbitPortSyncResult {
    Success,
    /// Failed with HTTP status code (or `0` for network errors).
    Failed(u16),
}

/// Full torrent record as returned by `/api/v2/torrents/info`.
#[derive(Debug, Clone)]
pub struct QbitTorrentDetails {
    pub hash: windlass_types::TorrentHash,
    pub name: TorrentName,
    pub state: QbitTorrentState,
    pub seeding_time_secs: u64,
    pub downloaded_bytes: u64,
    /// Parsed from the comment field (MAM torrent page URL).
    pub mam_id: Option<MamTorrentId>,
}

/// qBittorrent torrent state string → typed variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QbitTorrentState {
    Downloading,
    StalledDownloading,
    Uploading,
    StalledUploading,
    ForcedUpload,
    PausedDownloading,
    PausedUploading,
    Error,
    Other(String),
}

impl From<&str> for QbitTorrentState {
    fn from(s: &str) -> Self {
        match s {
            "downloading" => Self::Downloading,
            "stalledDL" => Self::StalledDownloading,
            "uploading" => Self::Uploading,
            "stalledUP" => Self::StalledUploading,
            "forcedUP" => Self::ForcedUpload,
            "pausedDL" | "stoppedDL" => Self::PausedDownloading,
            "pausedUP" | "stoppedUP" => Self::PausedUploading,
            "error" => Self::Error,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// qBittorrent application preferences relevant to compliance.
#[derive(Debug, Clone)]
pub struct QbitPreferences {
    pub torrents: u32,
    pub downloads: u32,
    pub uploads: u32,
    pub listen_port: Option<VpnPort>,
    /// Whether DHT is enabled (MAM Rule 6.1: must be false on private trackers).
    pub dht: bool,
    /// Whether Peer Exchange (`PeX`) is enabled (MAM Rule 6.1: must be false).
    pub pex: bool,
    /// Whether Local Service Discovery (LSD/LPD) is enabled (MAM Rule 6.1: must be false).
    pub lsd: bool,
    /// Maximum number of simultaneously active torrents (`max_active_torrents`).
    ///
    /// A value of `u32::MAX` means "no limit" (used when the preference is
    /// negative, which qBittorrent uses to indicate unlimited).
    pub max_active_torrents: u32,
}

// ── Wire deserialization types (private to this module) ───────────────────────

#[derive(Deserialize)]
pub(super) struct TorrentInfoWire {
    pub hash: String,
    pub name: String,
    pub state: String,
    pub seeding_time: u64,
    pub downloaded: u64,
    #[serde(default)]
    pub comment: String,
}

// Field names are dictated by qBittorrent's API response JSON keys and must not be renamed.
#[allow(clippy::struct_field_names)]
#[derive(Deserialize)]
pub(super) struct PreferencesWire {
    pub max_active_torrents: i64,
    pub max_active_downloads: i64,
    pub max_active_uploads: i64,
    pub listen_port: Option<i64>,
    /// DHT (Distributed Hash Table) — banned on private trackers (MAM Rule 6.1).
    #[serde(default)]
    pub dht: bool,
    /// Peer Exchange — banned on private trackers (MAM Rule 6.1).
    #[serde(default)]
    pub pex: bool,
    /// Local Service Discovery — banned on private trackers (MAM Rule 6.1).
    #[serde(default)]
    pub lsd: bool,
}

// ── parse_mam_id ──────────────────────────────────────────────────────────────

/// Parses a MAM torrent ID from a comment field.
///
/// Accepts both `/t/12345` and `/tor/12345` URL formats.
/// Returns `None` if the comment is not a recognisable MAM URL.
#[must_use]
pub fn parse_mam_id(comment: &str) -> Option<MamTorrentId> {
    let path = comment
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.myanonamouse.net");
    if let Some(rest) = path
        .strip_prefix("/t/")
        .or_else(|| path.strip_prefix("/tor/"))
    {
        rest.split('/')
            .next()?
            .parse::<u64>()
            .ok()
            .and_then(|id| MamTorrentId::try_new(id).ok())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mam_id_t_format() {
        assert_eq!(
            parse_mam_id("https://www.myanonamouse.net/t/12345"),
            Some(MamTorrentId::try_new(12345).unwrap())
        );
    }

    #[test]
    fn parse_mam_id_tor_format() {
        assert_eq!(
            parse_mam_id("https://www.myanonamouse.net/tor/99999"),
            Some(MamTorrentId::try_new(99999).unwrap())
        );
    }

    #[test]
    fn parse_mam_id_http() {
        assert_eq!(
            parse_mam_id("http://www.myanonamouse.net/t/1"),
            Some(MamTorrentId::try_new(1).unwrap())
        );
    }

    #[test]
    fn parse_mam_id_empty_returns_none() {
        assert_eq!(parse_mam_id(""), None);
    }

    #[test]
    fn parse_mam_id_unrelated_comment_returns_none() {
        assert_eq!(parse_mam_id("Some random comment"), None);
    }

    #[test]
    fn parse_mam_id_numeric_only_returns_none() {
        assert_eq!(parse_mam_id("12345"), None);
    }

    #[test]
    fn state_from_str_all_variants() {
        assert_eq!(
            QbitTorrentState::from("downloading"),
            QbitTorrentState::Downloading
        );
        assert_eq!(
            QbitTorrentState::from("stalledDL"),
            QbitTorrentState::StalledDownloading
        );
        assert_eq!(
            QbitTorrentState::from("uploading"),
            QbitTorrentState::Uploading
        );
        assert_eq!(
            QbitTorrentState::from("stalledUP"),
            QbitTorrentState::StalledUploading
        );
        assert_eq!(
            QbitTorrentState::from("forcedUP"),
            QbitTorrentState::ForcedUpload
        );
        assert_eq!(
            QbitTorrentState::from("pausedDL"),
            QbitTorrentState::PausedDownloading
        );
        assert_eq!(
            QbitTorrentState::from("stoppedDL"),
            QbitTorrentState::PausedDownloading
        );
        assert_eq!(
            QbitTorrentState::from("pausedUP"),
            QbitTorrentState::PausedUploading
        );
        assert_eq!(
            QbitTorrentState::from("stoppedUP"),
            QbitTorrentState::PausedUploading
        );
        assert_eq!(QbitTorrentState::from("error"), QbitTorrentState::Error);
        assert_eq!(
            QbitTorrentState::from("unknown_state"),
            QbitTorrentState::Other("unknown_state".to_owned())
        );
    }
}
