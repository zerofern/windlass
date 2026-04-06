use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use std::time::Duration;
use windlass_types::{AlertPriority, HttpStatusCode, RetryCount, WakeupId};

#[test]
fn connection_refused_is_ignored_when_not_authenticating() {
    let (_, actions) = connected_state().process_event(Event::QbitConnectionRefused);
    assert!(
        actions.is_empty(),
        "stale QbitConnectionRefused must produce no actions"
    );
}

#[test]
fn qbit_auth_retry_wakeup_is_ignored_when_not_authenticating() {
    let (_, actions) = connected_state().process_event(Event::Wakeup(WakeupId::QbitAuthRetry));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit)),
        "QbitAuthRetry wakeup must be ignored when qBit is already Ready"
    );
}

#[test]
fn qbit_auth_success_starts_port_sync_when_connected() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(0),
    };
    let (new_state, actions) = state.process_event(Event::QbitAuthSuccess(cookie()));
    assert!(matches!(
        new_state.qbit,
        QbitState::SyncingPort {
            attempt: RetryCount(0),
            ..
        }
    ));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SyncQbitPort(_, _)))
    );
}

#[test]
fn qbit_auth_success_stores_cookie_when_vpn_not_yet_connected() {
    // Auth completes before the port file is read (race condition edge case).
    let mut state = SystemState::initial();
    state.vpn = VpnState::AwaitingTunnel;
    let (new_state, actions) = state.process_event(Event::QbitAuthSuccess(cookie()));
    assert!(matches!(new_state.qbit, QbitState::Authenticated { .. }));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::SyncQbitPort(_, _)))
    );
}

#[test]
fn qbit_auth_failed_emits_critical_alert_and_schedules_retry() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(0),
    };
    let (new_state, actions) = state.process_event(Event::QbitAuthFailed);
    // Credentials rejected: alert immediately, reset to attempt 0.
    assert!(matches!(
        new_state.qbit,
        QbitState::Authenticating {
            attempt: RetryCount(0)
        }
    ));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Critical, _)))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _)))
    );
}

#[test]
fn qbit_connection_refused_schedules_silent_retry() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(0),
    };
    let (new_state, actions) = state.process_event(Event::QbitConnectionRefused);
    // Connection refused is normal startup — no alert, no attempt increment.
    assert!(matches!(
        new_state.qbit,
        QbitState::Authenticating {
            attempt: RetryCount(0)
        }
    ));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(_, _)))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _)))
    );
    // Fixed short delay, not exponential.
    let delay = actions.iter().find_map(|a| match a {
        Action::ScheduleWakeup(WakeupId::QbitAuthRetry, d) => Some(*d),
        _ => None,
    });
    assert_eq!(delay, Some(Duration::from_secs(5)));
}

#[test]
fn qbit_api_error_schedules_exponential_backoff_retry() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(0),
    };
    let (new_state, actions) = state.process_event(Event::QbitApiError(HttpStatusCode(403)));
    assert!(matches!(
        new_state.qbit,
        QbitState::Authenticating {
            attempt: RetryCount(1)
        }
    ));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _)))
    );
}

#[test]
fn qbit_auth_failed_when_not_authenticating_stays_at_attempt_zero() {
    // Stale response arrives after state machine moved on — doesn't increment.
    let mut state = SystemState::initial();
    state.qbit = QbitState::Offline;
    let (new_state, actions) = state.process_event(Event::QbitAuthFailed);
    assert!(matches!(
        new_state.qbit,
        QbitState::Authenticating {
            attempt: RetryCount(0)
        }
    ));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _)))
    );
}

#[test]
fn qbit_api_error_backoff_is_exponential() {
    // attempt 0 → base * 2^0 = 2s; attempt 1 → 4s; attempt 2 → 8s
    for (attempt, expected_secs) in [(0u8, 2u64), (1, 4), (2, 8), (3, 16)] {
        let mut state = SystemState::initial();
        state.qbit = QbitState::Authenticating {
            attempt: RetryCount(attempt),
        };
        let (_, actions) = state.process_event(Event::QbitApiError(HttpStatusCode(500)));
        let backoff = actions.iter().find_map(|a| match a {
            Action::ScheduleWakeup(WakeupId::QbitAuthRetry, d) => Some(*d),
            _ => None,
        });
        assert_eq!(
            backoff,
            Some(Duration::from_secs(expected_secs)),
            "attempt {attempt} should have backoff {expected_secs}s"
        );
    }
}

#[test]
fn qbit_port_sync_success_updates_mam() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::SyncingPort {
        attempt: RetryCount(0),
        cookie: cookie(),
        target: port(),
    };
    let (new_state, actions) = state.process_event(Event::QbitPortSyncSuccess);
    assert!(matches!(new_state.qbit, QbitState::Ready { .. }));
    assert!(
        matches!(new_state.mam, MamState::SyncPending { .. }),
        "mam should be SyncPending while UpdateMam is in flight"
    );
    assert!(actions.iter().any(|a| matches!(a, Action::UpdateMam(_))));
}

#[test]
fn qbit_port_sync_success_is_noop_when_vpn_not_connected() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Stopped;
    state.qbit = QbitState::SyncingPort {
        attempt: RetryCount(0),
        cookie: cookie(),
        target: port(),
    };
    let (new_state, actions) = state.process_event(Event::QbitPortSyncSuccess);
    assert!(!matches!(new_state.qbit, QbitState::Ready { .. }));
    assert!(!actions.iter().any(|a| matches!(a, Action::UpdateMam(_))));
}

#[test]
fn qbit_port_sync_success_is_noop_when_not_syncing() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Authenticated { cookie: cookie() };
    let (_, actions) = state.process_event(Event::QbitPortSyncSuccess);
    assert!(!actions.iter().any(|a| matches!(a, Action::UpdateMam(_))));
}

#[test]
fn qbit_port_sync_failed_retries_under_limit() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort {
        attempt: RetryCount(0),
        cookie: cookie(),
        target: port(),
    };
    let (new_state, actions) = state.process_event(Event::QbitPortSyncFailed(HttpStatusCode(500)));
    assert!(matches!(
        new_state.qbit,
        QbitState::SyncingPort {
            attempt: RetryCount(1),
            ..
        }
    ));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitSyncRetry, _)))
    );
}

#[test]
fn qbit_port_sync_failed_falls_back_at_limit() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort {
        attempt: RetryCount(3),
        cookie: cookie(),
        target: port(),
    };
    let (new_state, actions) = state.process_event(Event::QbitPortSyncFailed(HttpStatusCode(500)));
    assert!(matches!(
        new_state.qbit,
        QbitState::Authenticating {
            attempt: RetryCount(0)
        }
    ));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Warning, _)))
    );
}

#[test]
fn qbit_port_sync_failed_is_noop_when_not_syncing() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Offline;
    let (new_state, actions) = state.process_event(Event::QbitPortSyncFailed(HttpStatusCode(503)));
    assert_eq!(new_state.qbit, QbitState::Offline);
    assert!(actions.is_empty());
}
