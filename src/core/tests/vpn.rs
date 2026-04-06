use super::helpers::*;
use crate::core::{actions::Action, events::Event, types::*};
use crate::types::RetryCount;

#[test]
fn unexpected_vpn_death_dumps_logs() {
    let state = connected_state();
    let (new_state, actions) = state.process_event(Event::DockerGluetunDied);
    assert_eq!(new_state.vpn, VpnState::DumpingLogs);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}

#[test]
fn death_from_awaiting_tunnel_dumps_logs() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::AwaitingTunnel;
    let (new_state, actions) = state.process_event(Event::DockerGluetunDied);
    assert_eq!(new_state.vpn, VpnState::DumpingLogs);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}

#[test]
fn death_from_stopped_is_noop_for_vpn() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Stopped;
    let (new_state, actions) = state.process_event(Event::DockerGluetunDied);
    assert_eq!(new_state.vpn, VpnState::Stopped);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::StopDependentContainers))
    );
}

#[test]
fn unexpected_death_resets_qbit_and_mam() {
    let state = connected_state();
    let (new_state, _) = state.process_event(Event::DockerGluetunDied);
    assert_eq!(new_state.qbit, QbitState::Offline);
    assert_eq!(new_state.mam, MamState::Unknown);
}

#[test]
fn logs_dumped_stops_containers_and_restarts_gluetun() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::DumpingLogs;
    let (new_state, actions) = state.process_event(Event::LogsDumped);
    assert_eq!(new_state.vpn, VpnState::Starting);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::StopDependentContainers))
    );
    assert!(actions.iter().any(|a| matches!(a, Action::RestartGluetun)));
}

#[test]
fn double_dump_guard_skips_dump_when_starting() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Starting;
    let (_, actions) = state.process_event(Event::DockerGluetunDied);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::StopDependentContainers))
    );
}

#[test]
fn double_dump_guard_skips_dump_when_dumping_logs() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::DumpingLogs;
    let (_, actions) = state.process_event(Event::DockerGluetunDied);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}

#[test]
fn gluetun_healthy_starts_containers() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Starting;
    let (new_state, actions) = state.process_event(Event::DockerGluetunHealthy);
    assert_eq!(new_state.vpn, VpnState::AwaitingTunnel);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::StartDependentContainers))
    );
}

#[test]
fn port_file_read_ok_authenticates_qbit() {
    let (new_state, actions) =
        SystemState::initial().process_event(Event::PortFileReadResult(Ok((ip(), port()))));
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
fn port_file_read_same_values_is_noop() {
    // Core ignores no-change reads — debouncer may fire even when content is unchanged.
    let (_, actions) =
        connected_state().process_event(Event::PortFileReadResult(Ok((ip(), port()))));
    assert!(
        actions.is_empty(),
        "identical ip+port must produce no actions"
    );
}

#[test]
fn port_file_read_new_port_triggers_reauth() {
    use crate::types::VpnPort;
    let new_port = VpnPort::try_new(51821).unwrap();
    let (new_state, actions) =
        connected_state().process_event(Event::PortFileReadResult(Ok((ip(), new_port))));
    assert!(matches!(new_state.qbit, QbitState::Authenticating { .. }));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
}

#[test]
fn port_file_read_err_schedules_retry() {
    use crate::types::WakeupId;
    let (_, actions) =
        SystemState::initial().process_event(Event::PortFileReadResult(Err("partial".into())));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::RetryPortRead, _)))
    );
}
