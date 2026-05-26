use windlass_core::actions::Action;
use windlass_domain_core::WindlassTimer;
use windlass_mam_core::MamAction;
use windlass_types::{VpnIp, WakeupId};

use super::service::ServiceAction;

impl ServiceAction {
    pub(super) fn debug_action(&self) -> Option<Action> {
        match self {
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
