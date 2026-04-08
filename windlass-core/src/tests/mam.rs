use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use chrono::Utc;
use windlass_types::{AlertPriority, MamStatus, RetryCount, WakeupId};

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

#[test]
fn mam_update_success_sends_ok_alert() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    let actions = state.process_event(Event::MamUpdateSuccess { at: now() }, now());
    assert!(matches!(state.mam, MamState::Synced { .. }));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Info, _)))
    );
}

#[test]
fn mam_update_success_is_noop_when_vpn_not_connected() {
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::MamUpdateSuccess { at: now() }, now());
    assert_eq!(state.mam, MamState::Unknown);
    assert!(actions.is_empty());
}

#[test]
fn mam_asn_mismatch_blocks_and_alerts_with_ip() {
    let mut state = connected_state();
    let actions = state.process_event(
        Event::MamAsnMismatch {
            at: now(),
            ip: ip(),
        },
        now(),
    );
    assert!(matches!(state.mam, MamState::AsnBlocked { .. }));
    let alert = actions.iter().find_map(|a| match a {
        Action::SendGotifyAlert(AlertPriority::Critical, msg) => Some(msg.clone()),
        _ => None,
    });
    assert!(alert.is_some(), "expected a Critical alert");
    assert!(
        alert.unwrap().contains("10.8.0.1"),
        "alert should include the mismatched IP"
    );
}

#[test]
fn connectable_schedules_heartbeat() {
    let mut state = connected_state();
    let actions = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Connectable,
        },
        now(),
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::Heartbeat, _)))
    );
}

#[test]
fn soft_recovery_re_triggers_qbit_auth() {
    let mut state = connected_state();
    let actions = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::NotConnectable,
        },
        now(),
    );
    assert!(
        matches!(
            state.qbit,
            QbitState::Authenticating {
                attempt: RetryCount(0)
            }
        ),
        "qBit should be Authenticating after soft recovery"
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
}

#[test]
fn soft_recovery_rearms_heartbeat() {
    let state = connected_state();
    let mut state = state;
    let actions = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::Heartbeat, _)))
    );
}

#[test]
fn hard_recovery_dumps_logs_and_restarts_stack() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Offline;
    let actions = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    assert_eq!(state.vpn, VpnState::DumpingLogs);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Critical, _)))
    );
}

#[test]
fn hard_recovery_escalates_from_authenticating_in_flight() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(1),
    };
    let actions = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}

#[test]
fn hard_recovery_escalates_from_syncing_in_flight() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort {
        attempt: RetryCount(0),
        cookie: cookie(),
        target: port(),
    };
    let actions = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}

#[test]
fn asn_blocked_suppresses_recovery() {
    let mut state = connected_state();
    state.mam = MamState::AsnBlocked { ip: ip() };
    let mut new_state = state.clone();
    let actions = new_state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    assert_eq!(new_state.qbit, state.qbit);
    assert!(actions.is_empty());
}

#[test]
fn soft_recovery_from_authenticated_re_auths_qbit() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Authenticated { cookie: cookie() };
    let actions = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    assert!(matches!(
        state.qbit,
        QbitState::Authenticating {
            attempt: RetryCount(0)
        }
    ));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
}
