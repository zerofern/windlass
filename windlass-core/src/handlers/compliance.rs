use crate::actions::Action;
use crate::torrent::{TorrentRecord, TorrentState};
use crate::types::{QbitState, SystemState};
use tracing::{debug, info, warn};
use windlass_types::{AlertPriority, AuthCookie, TorrentHash, WakeupId};

const SEEDING_REQUIRED_SECS: u64 = 72 * 3600;

impl SystemState {
    pub(crate) fn on_wakeup_compliance_poll(&self) -> Vec<Action> {
        let mut actions = vec![Action::ScheduleWakeup(
            WakeupId::CompliancePoll,
            std::time::Duration::from_secs(self.compliance_poll_interval_secs),
        )];
        if let QbitState::Ready { cookie, .. } = &self.qbit {
            debug!("compliance poll: fetching torrent details and preferences");
            actions.push(Action::FetchTorrentDetails(cookie.clone()));
            actions.push(Action::FetchQbitPreferences(cookie.clone()));
        }
        actions
    }

    pub(crate) fn on_qbit_torrent_details_received(
        &mut self,
        torrents: Vec<TorrentRecord>,
    ) -> Vec<Action> {
        let cookie = match &self.qbit {
            QbitState::Ready { cookie, .. } => Some(cookie.clone()),
            _ => None,
        };
        let mut actions = Vec::new();
        if let Some(c) = &cookie {
            self.check_new_torrents(&torrents, c, &mut actions);
            check_dead_torrents(&torrents, c, &mut actions);
        }
        check_hnr_at_risk(&torrents, &mut actions);
        self.check_quota(&torrents, &mut actions);
        if let Some(c) = &cookie {
            self.check_active_limit(&torrents, c, &mut actions);
        }
        actions.push(Action::UpsertTorrentRecords(torrents.clone()));
        self.torrents = torrents.into_iter().map(|t| (t.hash.clone(), t)).collect();
        self.mark_changed();
        actions
    }

    /// Emits `SetAllFilesPriority` for every torrent not previously seen.
    fn check_new_torrents(
        &self,
        torrents: &[TorrentRecord],
        cookie: &AuthCookie,
        actions: &mut Vec<Action>,
    ) {
        for t in torrents {
            if !self.torrents.contains_key(&t.hash) {
                debug!(hash = %t.hash.0, "new torrent — enforcing no-partials");
                actions.push(Action::SetAllFilesPriority(t.hash.clone(), cookie.clone()));
            }
        }
    }

    /// Alerts when the unsatisfied torrent count approaches or reaches the quota.
    fn check_quota(&self, torrents: &[TorrentRecord], actions: &mut Vec<Action>) {
        let unsatisfied = u32::try_from(
            torrents
                .iter()
                .filter(|t| t.downloaded_bytes > 0 && t.seeding_time_secs < SEEDING_REQUIRED_SECS)
                .count(),
        )
        .unwrap_or(u32::MAX);

        if unsatisfied >= self.unsatisfied_quota_limit {
            warn!(
                unsatisfied,
                limit = self.unsatisfied_quota_limit,
                "quota limit reached"
            );
            actions.push(Action::SendAlert {
                priority: AlertPriority::Critical,
                title: "Quota limit reached".into(),
                body: format!(
                    "⚠️ {unsatisfied}/{} unsatisfied torrents — download disabled.",
                    self.unsatisfied_quota_limit
                ),
            });
        } else if unsatisfied >= self.unsatisfied_quota_limit.saturating_sub(5) {
            actions.push(Action::SendAlert {
                priority: AlertPriority::Warning,
                title: "Approaching quota limit".into(),
                body: format!(
                    "⚠️ {unsatisfied}/{} unsatisfied torrents.",
                    self.unsatisfied_quota_limit
                ),
            });
        }
    }

    /// If over the active-torrent limit, pause the oldest satisfied seeder
    /// and force-resume the most urgent unsatisfied torrent.
    fn check_active_limit(
        &self,
        torrents: &[TorrentRecord],
        cookie: &AuthCookie,
        actions: &mut Vec<Action>,
    ) {
        let active_count = u32::try_from(torrents.iter().filter(|t| t.state.is_active()).count())
            .unwrap_or(u32::MAX);
        if active_count < self.max_active_torrents {
            return;
        }
        let parked: Vec<_> = torrents
            .iter()
            .filter(|t| {
                t.downloaded_bytes > 0
                    && t.seeding_time_secs < SEEDING_REQUIRED_SECS
                    && matches!(
                        t.state,
                        TorrentState::PausedUploading | TorrentState::StalledUploading
                    )
            })
            .collect();
        if parked.is_empty() {
            return;
        }
        if let Some(oldest) = torrents
            .iter()
            .filter(|t| {
                t.seeding_time_secs >= SEEDING_REQUIRED_SECS
                    && matches!(t.state, TorrentState::Uploading)
            })
            .max_by_key(|t| t.seeding_time_secs)
        {
            info!(hash = %oldest.hash.0, "pausing satisfied seeder to free slot");
            actions.push(Action::PauseTorrent(oldest.hash.clone(), cookie.clone()));
        }
        info!(hash = %parked[0].hash.0, "force-resuming unsatisfied torrent");
        actions.push(Action::ForceResumeTorrent(
            parked[0].hash.clone(),
            cookie.clone(),
        ));
    }

    pub(crate) fn on_delete_torrent_requested(&self, hash: TorrentHash) -> Vec<Action> {
        let cookie = match &self.qbit {
            QbitState::Ready { cookie, .. } => cookie.clone(),
            _ => {
                return vec![Action::SendAlert {
                    priority: AlertPriority::Warning,
                    title: "Delete failed".into(),
                    body: "qBittorrent not connected — cannot delete torrent.".into(),
                }];
            }
        };
        if let Some(t) = self.torrents.get(&hash)
            && t.downloaded_bytes > 0
            && t.seeding_time_secs < SEEDING_REQUIRED_SECS
        {
            let hours_done = t.seeding_time_secs / 3600;
            let hours_left = (SEEDING_REQUIRED_SECS / 3600).saturating_sub(hours_done);
            return vec![Action::SendAlert {
                priority: AlertPriority::Warning,
                title: "HnR lock — cannot delete".into(),
                body: format!(
                    "{}: {hours_done}h seeded, {hours_left}h remaining. \
                     Manual deletion blocked to protect your HnR.",
                    t.name.0
                ),
            }];
        }
        let detail = format!("{{\"hash\":\"{}\"}}", hash.0);
        vec![
            Action::DeleteTorrent(hash, cookie),
            Action::WriteEvent {
                source: "user".into(),
                action: "torrent_deleted".into(),
                book_id: None,
                detail: Some(detail),
            },
        ]
    }
}

/// Deletes and blacklists stalled zero-byte torrents.
fn check_dead_torrents(torrents: &[TorrentRecord], cookie: &AuthCookie, actions: &mut Vec<Action>) {
    for t in torrents {
        if t.downloaded_bytes == 0
            && matches!(
                t.state,
                TorrentState::StalledDownloading | TorrentState::Error
            )
        {
            warn!(hash = %t.hash.0, name = %t.name.0, "dead torrent detected — removing");
            actions.push(Action::DeleteTorrent(t.hash.clone(), cookie.clone()));
            if let Some(mam_id) = t.mam_id {
                actions.push(Action::BlacklistMamId(mam_id));
            }
            actions.push(Action::WriteEvent {
                source: "compliance".into(),
                action: "dead_torrent_removed".into(),
                book_id: None,
                detail: Some(format!("{{\"hash\":\"{}\"}}", t.hash.0)),
            });
        }
    }
}

/// Sends a critical alert for torrents at risk of a Hit-and-Run violation.
fn check_hnr_at_risk(torrents: &[TorrentRecord], actions: &mut Vec<Action>) {
    for t in torrents {
        if t.downloaded_bytes > 0
            && t.seeding_time_secs < SEEDING_REQUIRED_SECS
            && matches!(
                t.state,
                TorrentState::StalledUploading | TorrentState::Error
            )
        {
            let hours_done = t.seeding_time_secs / 3600;
            let hours_left = (SEEDING_REQUIRED_SECS / 3600).saturating_sub(hours_done);
            warn!(name = %t.name.0, hours_done, hours_left, "HnR at risk");
            actions.push(Action::SendAlert {
                priority: AlertPriority::Critical,
                title: "HnR at risk".into(),
                body: format!(
                    "{}: stalled with {hours_done}h seeding, {hours_left}h required",
                    t.name.0
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::net::Ipv4Addr;
    use windlass_types::{AuthCookie, MamTorrentId, TorrentHash, TorrentName, VpnIp, VpnPort};

    fn ip() -> VpnIp {
        VpnIp(Ipv4Addr::new(10, 8, 0, 1))
    }

    fn port() -> VpnPort {
        VpnPort::try_new(51820).unwrap()
    }

    fn connected_state() -> SystemState {
        use crate::types::{MamState, VpnState};
        SystemState {
            vpn: VpnState::Connected {
                ip: ip(),
                port: port(),
            },
            qbit: QbitState::Ready {
                port: port(),
                cookie: AuthCookie("sid=abc".into()),
            },
            mam: MamState::Synced {
                port: port(),
                ip: ip(),
            },
            ..SystemState::initial()
        }
    }

    fn hash(s: &str) -> TorrentHash {
        TorrentHash(s.into())
    }

    fn make_torrent(
        id: &str,
        state: TorrentState,
        seeding_secs: u64,
        downloaded: u64,
        mam_id: Option<u64>,
    ) -> TorrentRecord {
        TorrentRecord {
            hash: hash(id),
            name: TorrentName(format!("Torrent {id}")),
            state,
            seeding_time_secs: seeding_secs,
            downloaded_bytes: downloaded,
            mam_id: mam_id.map(MamTorrentId),
            seen_at: Utc::now(),
        }
    }

    #[test]
    fn new_torrent_emits_set_all_files_priority() {
        let mut state = connected_state();
        let torrents = vec![make_torrent(
            "abc",
            TorrentState::Uploading,
            100,
            1024,
            None,
        )];
        let actions = state.on_qbit_torrent_details_received(torrents);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SetAllFilesPriority(h, _) if h.0 == "abc"))
        );
    }

    #[test]
    fn known_torrent_does_not_emit_set_all_files_priority() {
        let mut state = connected_state();
        let t = make_torrent("abc", TorrentState::Uploading, 100, 1024, None);
        state.torrents.insert(hash("abc"), t.clone());
        let actions = state.on_qbit_torrent_details_received(vec![t]);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::SetAllFilesPriority(h, _) if h.0 == "abc"))
        );
    }

    #[test]
    fn stalled_zero_bytes_emits_delete_and_blacklist() {
        let mut state = connected_state();
        let t = make_torrent("dead", TorrentState::StalledDownloading, 0, 0, Some(99));
        let actions = state.on_qbit_torrent_details_received(vec![t]);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "dead"))
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::BlacklistMamId(m) if m.0 == 99))
        );
        assert!(actions.iter().any(
            |a| matches!(a, Action::WriteEvent { action, .. } if action == "dead_torrent_removed")
        ));
    }

    #[test]
    fn stalled_with_bytes_and_under_72h_emits_hnr_alert_not_delete() {
        let mut state = connected_state();
        let t = make_torrent("risk", TorrentState::StalledUploading, 3600, 1024, None);
        let actions = state.on_qbit_torrent_details_received(vec![t]);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SendAlert { title, .. } if title == "HnR at risk"))
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "risk"))
        );
    }

    #[test]
    fn unsatisfied_at_quota_minus_5_emits_warning() {
        let mut state = connected_state();
        state.unsatisfied_quota_limit = 10;
        // 5 unsatisfied torrents = limit - 5 = 5
        let torrents: Vec<_> = (0..5)
            .map(|i| make_torrent(&i.to_string(), TorrentState::Uploading, 3600, 1024, None))
            .collect();
        let actions = state.on_qbit_torrent_details_received(torrents);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SendAlert { title, priority, .. }
            if title == "Approaching quota limit" && priority == &AlertPriority::Warning))
        );
    }

    #[test]
    fn unsatisfied_at_quota_limit_emits_critical() {
        let mut state = connected_state();
        state.unsatisfied_quota_limit = 5;
        let torrents: Vec<_> = (0..5)
            .map(|i| make_torrent(&i.to_string(), TorrentState::Uploading, 3600, 1024, None))
            .collect();
        let actions = state.on_qbit_torrent_details_received(torrents);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SendAlert { title, priority, .. }
            if title == "Quota limit reached" && priority == &AlertPriority::Critical))
        );
    }

    #[test]
    fn active_limit_full_pauses_oldest_satisfied_resumes_unsatisfied() {
        let mut state = connected_state();
        state.max_active_torrents = 2;
        let torrents = vec![
            make_torrent("active1", TorrentState::Uploading, 80 * 3600, 1024, None),
            make_torrent("active2", TorrentState::Uploading, 100 * 3600, 1024, None),
            make_torrent("parked", TorrentState::PausedUploading, 3600, 1024, None),
        ];
        let actions = state.on_qbit_torrent_details_received(torrents);
        // Should pause "active2" (oldest satisfied = highest seeding_time)
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::PauseTorrent(h, _) if h.0 == "active2"))
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ForceResumeTorrent(h, _) if h.0 == "parked"))
        );
    }

    #[test]
    fn delete_requested_hnr_locked_blocks_deletion() {
        let mut state = connected_state();
        state.torrents.insert(
            hash("locked"),
            make_torrent("locked", TorrentState::Uploading, 3600, 1024, None),
        );
        let actions = state.on_delete_torrent_requested(hash("locked"));
        assert!(actions.iter().any(
            |a| matches!(a, Action::SendAlert { title, .. } if title == "HnR lock — cannot delete")
        ));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::DeleteTorrent(..)))
        );
    }

    #[test]
    fn delete_requested_satisfied_allows_deletion() {
        let mut state = connected_state();
        state.torrents.insert(
            hash("done"),
            make_torrent("done", TorrentState::Uploading, 80 * 3600, 1024, None),
        );
        let actions = state.on_delete_torrent_requested(hash("done"));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "done"))
        );
    }

    #[test]
    fn delete_requested_zero_bytes_allows_deletion() {
        let mut state = connected_state();
        state.torrents.insert(
            hash("empty"),
            make_torrent("empty", TorrentState::StalledDownloading, 0, 0, None),
        );
        let actions = state.on_delete_torrent_requested(hash("empty"));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "empty"))
        );
    }

    #[test]
    fn delete_requested_unknown_hash_allows_deletion() {
        let state = connected_state();
        let actions = state.on_delete_torrent_requested(hash("unknown"));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "unknown"))
        );
    }

    #[test]
    fn compliance_poll_wakeup_schedules_next_and_fetches_when_ready() {
        let state = connected_state();
        let actions = state.on_wakeup_compliance_poll();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::CompliancePoll, _)))
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::FetchTorrentDetails(_)))
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::FetchQbitPreferences(_)))
        );
    }

    #[test]
    fn compliance_poll_wakeup_no_fetch_when_qbit_offline() {
        let mut state = connected_state();
        state.qbit = crate::types::QbitState::Offline;
        let actions = state.on_wakeup_compliance_poll();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::CompliancePoll, _)))
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::FetchTorrentDetails(_)))
        );
    }
}
