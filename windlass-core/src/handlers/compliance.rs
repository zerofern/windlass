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
            actions.push(Action::WriteActivity {
                source: "compliance".into(),
                action: "torrent_paused".into(),
                book_id: None,
                detail: Some(format!("hash={}", oldest.hash.0)),
            });
        }
        info!(hash = %parked[0].hash.0, "force-resuming unsatisfied torrent");
        actions.push(Action::ForceResumeTorrent(
            parked[0].hash.clone(),
            cookie.clone(),
        ));
        actions.push(Action::WriteActivity {
            source: "compliance".into(),
            action: "torrent_resumed".into(),
            book_id: None,
            detail: Some(format!("hash={}", parked[0].hash.0)),
        });
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
            Action::WriteActivity {
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
            actions.push(Action::WriteActivity {
                source: "compliance".into(),
                action: "torrent_deleted".into(),
                book_id: None,
                detail: Some(format!("hash={}", t.hash.0)),
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
