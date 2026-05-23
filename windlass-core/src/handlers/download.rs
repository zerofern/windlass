use chrono::DateTime;
use chrono::Utc;
use windlass_types::{AlertPriority, MamTorrentId, TorrentHash, TorrentName};

use crate::actions::Action;
use crate::torrent::{TorrentRecord, TorrentState};
use crate::types::{QbitState, SystemState};

const SEEDING_REQUIRED_SECS: u64 = 72 * 3600;

impl SystemState {
    pub(crate) fn on_manual_download_requested(&self, mam_id: MamTorrentId) -> Vec<Action> {
        // Blacklist check: silently log and block.
        if self.blacklisted_mam_ids.contains(&mam_id) {
            return vec![Action::WriteActivity {
                source: "download".into(),
                action: "download_blocked".into(),
                book_id: None,
                detail: Some(format!(
                    "{{\"mam_id\":{},\"reason\":\"blacklisted\"}}",
                    mam_id.0
                )),
            }];
        }

        // Quota check: block if all slots are occupied with unsatisfied torrents.
        let unsatisfied = u32::try_from(
            self.torrents
                .values()
                .filter(|t| t.downloaded_bytes > 0 && t.seeding_time_secs < SEEDING_REQUIRED_SECS)
                .count(),
        )
        .unwrap_or(u32::MAX);
        if unsatisfied >= self.unsatisfied_quota_limit {
            return vec![Action::SendAlert {
                priority: AlertPriority::Warning,
                title: "Download blocked — quota full".into(),
                body: format!(
                    "{unsatisfied} unsatisfied torrents at class limit of {}.",
                    self.unsatisfied_quota_limit
                ),
            }];
        }

        // qBit ready check.
        let cookie = match &self.qbit {
            QbitState::Ready { cookie, .. } => cookie.clone(),
            _ => {
                return vec![Action::SendAlert {
                    priority: AlertPriority::Warning,
                    title: "Download blocked — qBit not ready".into(),
                    body: "qBittorrent is not connected. Try again shortly.".into(),
                }];
            }
        };

        vec![Action::FetchAndAddTorrent { mam_id, cookie }]
    }
}

/// Builds actions when a torrent is successfully added to qBittorrent.
/// Free function (does not require state).
pub fn on_torrent_added_to_qbit(
    mam_id: MamTorrentId,
    hash: &TorrentHash,
    at: DateTime<Utc>,
) -> Vec<Action> {
    // Write a minimal placeholder record so the torrent appears in the DB
    // immediately; the compliance monitor overwrites it on the next poll.
    let record = TorrentRecord {
        hash: hash.clone(),
        name: TorrentName(format!("mam-{}", mam_id.0)),
        state: TorrentState::Downloading,
        seeding_time_secs: 0,
        downloaded_bytes: 0,
        mam_id: Some(mam_id),
        seen_at: at,
    };
    vec![
        Action::SendAlert {
            priority: AlertPriority::Info,
            title: "Download started".into(),
            body: format!("MAM torrent {} added to qBittorrent.", mam_id.0),
        },
        Action::WriteActivity {
            source: "download".into(),
            action: "torrent_added".into(),
            book_id: None,
            detail: Some(format!(
                "{{\"mam_id\":{},\"hash\":\"{}\"}}",
                mam_id.0, hash.0
            )),
        },
        Action::UpsertTorrentRecords(vec![record]),
    ]
}

/// Builds actions when fetching or adding a torrent fails.
/// Free function (does not require state).
pub fn on_torrent_add_failed(mam_id: MamTorrentId, reason: &str) -> Vec<Action> {
    vec![
        Action::SendAlert {
            priority: AlertPriority::Warning,
            title: "Download failed".into(),
            body: format!("Failed to add MAM torrent {}: {reason}", mam_id.0),
        },
        Action::WriteActivity {
            source: "download".into(),
            action: "torrent_add_failed".into(),
            book_id: None,
            detail: Some(format!(
                "{{\"mam_id\":{},\"reason\":\"{reason}\"}}",
                mam_id.0
            )),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::net::Ipv4Addr;
    use windlass_types::{AuthCookie, MamTorrentId, TorrentHash, VpnIp, VpnPort};

    fn port() -> VpnPort {
        VpnPort::try_new(51820).unwrap()
    }

    fn ready_state() -> SystemState {
        use crate::types::{MamState, VpnState};
        SystemState {
            vpn: VpnState::Connected {
                ip: VpnIp(Ipv4Addr::new(10, 8, 0, 1)),
                port: port(),
            },
            qbit: QbitState::Ready {
                port: port(),
                cookie: AuthCookie("sid".into()),
            },
            mam: MamState::Synced {
                port: port(),
                ip: VpnIp(Ipv4Addr::new(10, 8, 0, 1)),
            },
            ..SystemState::initial()
        }
    }

    fn mam_id() -> MamTorrentId {
        MamTorrentId(12345)
    }

    fn hash(s: &str) -> TorrentHash {
        TorrentHash(s.into())
    }

    #[test]
    fn blacklisted_mam_id_emits_write_event_not_fetch() {
        let mut state = ready_state();
        state.blacklisted_mam_ids.insert(mam_id());
        let actions = state.on_manual_download_requested(mam_id());
        assert!(actions.iter().any(
            |a| matches!(a, Action::WriteActivity { action, .. } if action == "download_blocked")
        ));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::FetchAndAddTorrent { .. }))
        );
    }

    #[test]
    fn quota_full_emits_alert_not_fetch() {
        let mut state = ready_state();
        state.unsatisfied_quota_limit = 0;
        let actions = state.on_manual_download_requested(mam_id());
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SendAlert { title, .. } if title.contains("quota")))
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::FetchAndAddTorrent { .. }))
        );
    }

    #[test]
    fn qbit_offline_emits_alert_not_fetch() {
        let mut state = ready_state();
        state.qbit = QbitState::Offline;
        let actions = state.on_manual_download_requested(mam_id());
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SendAlert { title, .. } if title.contains("qBit")))
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::FetchAndAddTorrent { .. }))
        );
    }

    #[test]
    fn happy_path_emits_fetch_and_add_torrent() {
        let state = ready_state();
        let actions = state.on_manual_download_requested(mam_id());
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::FetchAndAddTorrent { mam_id: m, .. } if m.0 == 12345))
        );
    }

    #[test]
    fn torrent_added_emits_alert_write_event_upsert() {
        let actions = on_torrent_added_to_qbit(mam_id(), &hash("abc123"), Utc::now());
        assert!(actions.iter().any(
            |a| matches!(a, Action::SendAlert { priority, .. } if *priority == AlertPriority::Info)
        ));
        assert!(actions.iter().any(
            |a| matches!(a, Action::WriteActivity { action, .. } if action == "torrent_added")
        ));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::UpsertTorrentRecords(r) if !r.is_empty()))
        );
    }

    #[test]
    fn torrent_add_failed_emits_alert_and_write_event() {
        let actions = on_torrent_add_failed(mam_id(), "network error");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SendAlert { priority, .. } if *priority == AlertPriority::Warning))
        );
        assert!(actions.iter().any(
            |a| matches!(a, Action::WriteActivity { action, .. } if action == "torrent_add_failed")
        ));
    }
}
