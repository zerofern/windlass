use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use windlass_types::{AlertPriority, MamStatus, RetryCount, WakeupId};

#[test]
fn mam_update_success_sends_ok_alert() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    let (new_state, actions) = state.process_event(Event::MamUpdateSuccess);
    assert!(matches!(new_state.mam, MamState::Synced { .. }));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Info, _)))
    );
}

#[test]
fn mam_update_success_is_noop_when_vpn_not_connected() {
    let (new_state, actions) = SystemState::initial().process_event(Event::MamUpdateSuccess);
    assert_eq!(new_state.mam, MamState::Unknown);
    assert!(actions.is_empty());
}

#[test]
fn mam_asn_mismatch_blocks_and_alerts_with_ip() {
    let (new_state, actions) = connected_state().process_event(Event::MamAsnMismatch(ip()));
    assert!(matches!(new_state.mam, MamState::AsnBlocked { .. }));
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
fn connectable_resets_hard_recoveries() {
    let mut state = connected_state();
    state.hard_recoveries = RetryCount(2);
    let (new_state, _) = state.process_event(Event::MamStatusObserved(MamStatus::Connectable));
    assert_eq!(new_state.hard_recoveries, RetryCount(0));
}

#[test]
fn connectable_rearms_heartbeat() {
    let (_, actions) =
        connected_state().process_event(Event::MamStatusObserved(MamStatus::Connectable));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::Heartbeat, _)))
    );
}

#[test]
fn soft_recovery_from_ready_re_auths_qbit() {
    let state = connected_state(); // qbit is Ready
    let (new_state, actions) =
        state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
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
}

#[test]
fn soft_recovery_from_authenticated_re_auths_qbit() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Authenticated { cookie: cookie() };
    let (new_state, actions) =
        state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
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
}

#[test]
fn soft_recovery_rearms_heartbeat() {
    let state = connected_state();
    let (_, actions) = state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::Heartbeat, _)))
    );
}

#[test]
fn hard_recovery_increments_counter_and_dumps_logs() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.qbit = QbitState::Offline;
    let (new_state, actions) =
        state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
    assert_eq!(new_state.hard_recoveries, RetryCount(1));
    assert_eq!(new_state.vpn, VpnState::DumpingLogs);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}

#[test]
fn hard_recovery_escalates_from_authenticating_in_flight() {
    // If auth is already in flight, soft recovery is considered attempted.
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(1),
    };
    let (new_state, actions) =
        state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
    assert_eq!(new_state.hard_recoveries, RetryCount(1));
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
    let (new_state, _) = state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
    assert_eq!(new_state.hard_recoveries, RetryCount(1));
}

#[test]
fn death_loop_prevention_transitions_to_fatal() {
    let mut state = SystemState::initial();
    state.hard_recoveries = RetryCount(2);
    state.qbit = QbitState::Offline;
    let (new_state, actions) =
        state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
    assert!(matches!(new_state.run_mode, RunMode::Fatal { .. }));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Critical, _)))
    );
}

#[test]
fn death_loop_does_not_dump_logs_on_fatal_transition() {
    // When we hit the limit, we halt — no further recovery actions.
    let mut state = SystemState::initial();
    state.hard_recoveries = RetryCount(2);
    state.qbit = QbitState::Offline;
    let (_, actions) = state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
    assert!(!actions.iter().any(|a| matches!(a, Action::RestartGluetun)));
}

#[test]
fn asn_blocked_suppresses_recovery() {
    let mut state = connected_state();
    state.mam = MamState::AsnBlocked { ip: ip() };
    let (new_state, actions) = state
        .clone()
        .process_event(Event::MamStatusObserved(MamStatus::Unreachable));
    assert_eq!(new_state.qbit, state.qbit);
    assert!(actions.is_empty());
}

#[test]
fn fatal_mode_ignores_all_events_except_reset() {
    let mut state = SystemState::initial();
    state.run_mode = RunMode::Fatal {
        reason: "test".into(),
    };
    let (new_state, actions) = state.clone().process_event(Event::DockerGluetunDied);
    assert!(matches!(new_state.run_mode, RunMode::Fatal { .. }));
    assert!(actions.is_empty());
}

#[test]
fn manual_reset_clears_fatal_mode_and_restarts_gluetun() {
    let mut state = SystemState::initial();
    state.run_mode = RunMode::Fatal {
        reason: "test".into(),
    };
    state.hard_recoveries = RetryCount(3);
    let (new_state, actions) = state.process_event(Event::ManualReset);
    assert_eq!(new_state.run_mode, RunMode::Active);
    assert_eq!(new_state.hard_recoveries, RetryCount(0));
    assert_eq!(new_state.vpn, VpnState::Starting);
    assert!(actions.iter().any(|a| matches!(a, Action::RestartGluetun)));
}
