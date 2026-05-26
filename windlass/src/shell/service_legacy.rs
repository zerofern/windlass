use windlass_core::actions::Action;
use windlass_types::WakeupId;

pub(super) fn legacy_actions_for_service_mode(
    execute_service_actions: bool,
    actions: Vec<Action>,
) -> Vec<Action> {
    if !execute_service_actions {
        return actions;
    }
    actions
        .into_iter()
        .filter(|action| !service_replaces_legacy_action(action))
        .collect()
}

const fn service_replaces_legacy_action(action: &Action) -> bool {
    matches!(
        action,
        Action::ReadPortFiles
            | Action::AuthenticateQbit
            | Action::SyncQbitPort(_, _)
            | Action::UpdateMam(_)
            | Action::CheckMamConnectability
            | Action::CheckNewTorrents(_)
            | Action::FetchQbitPreferences(_)
            | Action::ScheduleWakeup(
                WakeupId::QbitAuthRetry
                    | WakeupId::QbitSyncRetry
                    | WakeupId::Heartbeat
                    | WakeupId::RetryPortRead,
                _
            )
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use windlass_core::actions::Action;
    use windlass_types::{AuthCookie, MamTorrentId, WakeupId};

    use super::{legacy_actions_for_service_mode, service_replaces_legacy_action};

    #[test]
    fn service_mode_filters_only_service_orchestration_actions() {
        let actions = vec![
            Action::AuthenticateQbit,
            Action::ScheduleWakeup(WakeupId::QbitAuthRetry, Duration::from_secs(1)),
            Action::ScheduleWakeup(WakeupId::DiskCheck, Duration::from_secs(1)),
            Action::FetchAndAddTorrent {
                mam_id: MamTorrentId(1),
                cookie: AuthCookie("sid".to_string()),
            },
        ];

        let filtered = legacy_actions_for_service_mode(true, actions);

        assert_eq!(filtered.len(), 2);
        assert!(matches!(
            filtered[0],
            Action::ScheduleWakeup(WakeupId::DiskCheck, _)
        ));
        assert!(matches!(filtered[1], Action::FetchAndAddTorrent { .. }));
    }

    #[test]
    fn service_mode_keeps_legacy_actions_when_disabled() {
        let actions = vec![Action::AuthenticateQbit];

        let filtered = legacy_actions_for_service_mode(false, actions);

        assert_eq!(filtered.len(), 1);
        assert!(matches!(filtered[0], Action::AuthenticateQbit));
    }

    #[test]
    fn service_replaces_qbit_preference_fetches() {
        assert!(service_replaces_legacy_action(
            &Action::FetchQbitPreferences(AuthCookie("sid".to_string()))
        ));
    }
}
