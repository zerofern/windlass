use windlass_core::actions::Action;
use windlass_domain_core::WindlassTimer;
use windlass_types::WakeupId;

use super::service::ServiceAction;

impl ServiceAction {
    pub(super) fn debug_action(&self) -> Option<Action> {
        match self {
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
