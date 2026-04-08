use crate::actions::Action;
use crate::types::{QbitState, SystemState};
use tracing::{debug, info, warn};
use uom::si::information::gigabyte;
use windlass_types::{AlertPriority, Information, TorrentName, WakeupId};

use super::{DISK_CHECK_INTERVAL, TORRENT_CHECK_INTERVAL};

impl SystemState {
    // Shell sends the raw full list; Core diffs against known_torrents and
    // only alerts on names that haven't been seen before.
    pub(crate) fn on_new_torrents_observed(&mut self, current: Vec<TorrentName>) -> Vec<Action> {
        let new_names: Vec<_> = current
            .iter()
            .filter(|name| !self.known_torrents.contains(*name))
            .cloned()
            .collect();
        self.known_torrents.extend(current);
        let mut actions = vec![];
        if new_names.is_empty() {
            debug!("torrent check: no new torrents");
        } else {
            info!(names = ?new_names, "new torrent(s) detected");
            self.mark_changed();
            let list = new_names
                .iter()
                .map(|n| n.0.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            actions.push(Action::SendGotifyAlert(
                AlertPriority::Info,
                format!("🧲 New torrent(s) added: {list}"),
            ));
        }
        actions.push(Action::ScheduleWakeup(
            WakeupId::TorrentCheck,
            TORRENT_CHECK_INTERVAL.into(),
        ));
        actions
    }

    pub(crate) fn on_wakeup(&self, id: WakeupId) -> Vec<Action> {
        match id {
            WakeupId::Heartbeat => vec![Action::CheckMamConnectability],
            WakeupId::DiskCheck => vec![Action::CheckDiskSpace],
            WakeupId::TorrentCheck => {
                if let QbitState::Ready { cookie, .. } = &self.qbit {
                    vec![Action::CheckNewTorrents(cookie.clone())]
                } else {
                    vec![]
                }
            }
            WakeupId::QbitAuthRetry => {
                if matches!(self.qbit, QbitState::Authenticating { .. }) {
                    vec![Action::AuthenticateQbit]
                } else {
                    debug!(qbit = %self.qbit, "QbitAuthRetry wakeup: no longer authenticating — ignoring");
                    vec![]
                }
            }
            WakeupId::QbitSyncRetry => {
                if let QbitState::SyncingPort { cookie, target, .. } = &self.qbit {
                    vec![Action::SyncQbitPort(cookie.clone(), *target)]
                } else {
                    vec![]
                }
            }
            WakeupId::RetryPortRead => vec![Action::ReadPortFiles],
        }
    }
}

pub fn on_disk_space_observed(space: Information) -> Vec<Action> {
    let gib = space.get::<gigabyte>();
    let mut actions = vec![];
    if gib < 50.0 {
        warn!(space_gib = format_args!("{gib:.1}"), "disk space low");
        actions.push(Action::SendGotifyAlert(
            AlertPriority::Warning,
            format!("💾 Low disk space: {gib:.1} GB remaining on /mnt/Data."),
        ));
    } else {
        debug!(space_gib = format_args!("{gib:.1}"), "disk space OK");
    }
    actions.push(Action::ScheduleWakeup(
        WakeupId::DiskCheck,
        DISK_CHECK_INTERVAL.into(),
    ));
    actions
}

pub fn on_mam_rate_limit_violation() -> Vec<Action> {
    warn!("MAM rate-limit violation — system entering debug mode");
    vec![Action::SendGotifyAlert(
        AlertPriority::Critical,
        "🛑 MAM rate limit guard triggered — requests were too fast. System paused in debug mode."
            .into(),
    )]
}
