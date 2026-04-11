#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use windlass_types::{MamTorrentId, TorrentHash, TorrentName};

/// Torrent state as tracked by the compliance core.
///
/// Mirrors `QbitTorrentState` from `windlass-clients` but is a pure core type.
/// The shell converts between them at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TorrentState {
    Downloading,
    StalledDownloading,
    Uploading,
    StalledUploading,
    ForcedUpload,
    PausedDownloading,
    PausedUploading,
    Error,
    Other,
}

impl TorrentState {
    /// Returns `true` for states that count towards qBit's active-torrent limit.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Downloading | Self::Uploading | Self::ForcedUpload
        )
    }

    /// Returns the string stored in the `torrents` DB table.
    #[must_use]
    pub const fn as_db_str(&self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::StalledDownloading => "stalledDL",
            Self::Uploading => "uploading",
            Self::StalledUploading => "stalledUP",
            Self::ForcedUpload => "forcedUP",
            Self::PausedDownloading => "pausedDL",
            Self::PausedUploading => "pausedUP",
            Self::Error => "error",
            Self::Other => "other",
        }
    }
}

/// In-memory representation of a torrent tracked by the compliance monitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentRecord {
    pub hash: TorrentHash,
    pub name: TorrentName,
    pub state: TorrentState,
    pub seeding_time_secs: u64,
    pub downloaded_bytes: u64,
    pub mam_id: Option<MamTorrentId>,
    pub seen_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_states_are_downloading_uploading_forced() {
        assert!(TorrentState::Downloading.is_active());
        assert!(TorrentState::Uploading.is_active());
        assert!(TorrentState::ForcedUpload.is_active());
        assert!(!TorrentState::StalledUploading.is_active());
        assert!(!TorrentState::PausedUploading.is_active());
        assert!(!TorrentState::Error.is_active());
    }

    #[test]
    fn as_db_str_roundtrips_all_variants() {
        let variants = [
            (TorrentState::Downloading, "downloading"),
            (TorrentState::StalledDownloading, "stalledDL"),
            (TorrentState::Uploading, "uploading"),
            (TorrentState::StalledUploading, "stalledUP"),
            (TorrentState::ForcedUpload, "forcedUP"),
            (TorrentState::PausedDownloading, "pausedDL"),
            (TorrentState::PausedUploading, "pausedUP"),
            (TorrentState::Error, "error"),
            (TorrentState::Other, "other"),
        ];
        for (state, expected) in variants {
            assert_eq!(state.as_db_str(), expected, "state mismatch for {state:?}");
        }
    }
}
