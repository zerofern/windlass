use windlass_core::actions::Action;
use windlass_domain_core::WindlassTimer;
use windlass_mam_core::MamAction;
use windlass_qbit_core::QbitAction;
use windlass_types::{VpnIp, WakeupId};

use super::service::ServiceAction;

impl ServiceAction {
    pub(super) fn debug_action(&self) -> Option<Action> {
        match self {
            Self::Qbit(action) => match action {
                QbitAction::Login => Some(Action::AuthenticateQbit),
                QbitAction::ReadPreferences { cookie } => {
                    Some(Action::FetchQbitPreferences(cookie.clone()))
                }
                QbitAction::SetListenPort { cookie, port } => {
                    Some(Action::SyncQbitPort(cookie.clone(), *port))
                }
                QbitAction::ListTorrents { cookie } => {
                    Some(Action::CheckNewTorrents(cookie.clone()))
                }
                QbitAction::PauseTorrent { cookie, hash } => {
                    Some(Action::PauseTorrent(hash.clone(), cookie.clone()))
                }
                QbitAction::ResumeTorrent { cookie, hash } => {
                    Some(Action::ForceResumeTorrent(hash.clone(), cookie.clone()))
                }
                QbitAction::ScheduleTimer { timer, after } => {
                    let wakeup = match timer {
                        windlass_qbit_core::QbitTimer::AuthRetry => WakeupId::QbitAuthRetry,
                        windlass_qbit_core::QbitTimer::SyncRetry => WakeupId::QbitSyncRetry,
                        windlass_qbit_core::QbitTimer::TorrentRefresh => WakeupId::TorrentCheck,
                    };
                    Some(Action::ScheduleWakeup(wakeup, *after))
                }
            },
            Self::Mam(action) => match action {
                MamAction::FetchStatus => Some(Action::CheckMamConnectability),
                MamAction::UpdateSeedboxPort { .. } => {
                    Some(Action::UpdateMam(VpnIp(std::net::Ipv4Addr::UNSPECIFIED)))
                }
                MamAction::ScheduleTimer { after, .. } => {
                    Some(Action::ScheduleWakeup(WakeupId::Heartbeat, *after))
                }
            },
            Self::Db(_) => None,
            Self::ScheduleTimer { timer, after } => {
                Some(Action::ScheduleWakeup(service_timer_wakeup(*timer), *after))
            }
        }
    }
}

pub(super) const fn service_timer_wakeup(timer: WindlassTimer) -> WakeupId {
    match timer {
        WindlassTimer::Snapshot => WakeupId::DomainSnapshot,
    }
}

pub(super) fn service_debug_actions(actions: &[ServiceAction]) -> Vec<Action> {
    actions
        .iter()
        .filter_map(ServiceAction::debug_action)
        .collect()
}
