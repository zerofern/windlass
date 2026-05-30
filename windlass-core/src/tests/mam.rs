use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use chrono::Utc;
use windlass_types::{AlertPriority, MamStatus, RetryCount};

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
    let outcome = state.process_event(
        Event::MamUpdateSuccess {
            at: now(),
            registered_ip: None,
            registered_asn: None,
            registered_as: None,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
    assert!(matches!(state.mam, MamState::Synced { .. }));
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::SendAlert {
            priority: AlertPriority::Info,
            ..
        }
    )));
}

#[test]
fn mam_update_success_is_noop_when_vpn_not_connected() {
    let mut state = SystemState::initial();
    let outcome = state.process_event(
        Event::MamUpdateSuccess {
            at: now(),
            registered_ip: None,
            registered_asn: None,
            registered_as: None,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert_eq!(state.mam, MamState::Unknown);
    assert!(actions.is_empty());
}

#[test]
fn mam_asn_mismatch_blocks_and_alerts_with_ip() {
    let mut state = connected_state();
    let outcome = state.process_event(
        Event::MamAsnMismatch {
            at: now(),
            ip: ip(),
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
    assert!(matches!(state.mam, MamState::AsnBlocked { .. }));
    let alert = actions.iter().find_map(|a| match a {
        Action::SendAlert {
            priority: AlertPriority::Critical,
            body,
            ..
        } => Some(body.clone()),
        _ => None,
    });
    assert!(alert.is_some(), "expected a Critical alert");
    assert!(
        alert.unwrap().contains("10.8.0.1"),
        "alert should include the mismatched IP"
    );
}

#[test]
fn connectable_is_service_orchestration_noop() {
    let mut state = connected_state();
    let outcome = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Connectable,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.is_empty());
}

#[test]
fn soft_recovery_marks_qbit_authenticating() {
    let mut state = connected_state();
    let outcome = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::NotConnectable,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
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
        !actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
}

#[test]
fn soft_recovery_does_not_emit_legacy_heartbeat() {
    let state = connected_state();
    let mut state = state;
    let outcome = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
    assert!(actions.is_empty());
}

#[test]
fn hard_recovery_dumps_logs_and_restarts_stack() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Offline;
    let outcome = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
    assert_eq!(state.vpn, VpnState::DumpingLogs);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::SendAlert {
            priority: AlertPriority::Critical,
            ..
        }
    )));
}

#[test]
fn hard_recovery_escalates_from_authenticating_in_flight() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(1),
    };
    let outcome = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
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
    let outcome = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
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
    let outcome = new_state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    assert!(!outcome.state_changed);
    assert_eq!(new_state.qbit, state.qbit);
    assert!(outcome.actions.is_empty());
}

#[test]
fn soft_recovery_from_authenticated_marks_qbit_authenticating() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Authenticated { cookie: cookie() };
    let outcome = state.process_event(
        Event::MamStatusObserved {
            at: now(),
            status: MamStatus::Unreachable,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(outcome.state_changed);
    assert!(matches!(
        state.qbit,
        QbitState::Authenticating {
            attempt: RetryCount(0)
        }
    ));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
}
