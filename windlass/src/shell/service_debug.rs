use windlass_core::actions::Action;

use super::service::ServiceAction;

impl ServiceAction {
    pub(super) const fn debug_action(&self) -> Option<Action> {
        match self {
            Self::Db(_) => None,
        }
    }
}

pub(super) fn service_debug_actions(actions: &[ServiceAction]) -> Vec<Action> {
    actions
        .iter()
        .filter_map(ServiceAction::debug_action)
        .collect()
}
