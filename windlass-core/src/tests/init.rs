use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use windlass_types::{RetryCount, WakeupId};

#[test]
fn init_healthy_with_files_fast_forwards_to_connected_and_auths() {
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::Init {
        is_gluetun_healthy: true,
        port_files: Ok((ip(), port())),
    });
    assert!(matches!(state.vpn, VpnState::Connected { .. }));
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
    let wakeup_ids: Vec<_> = actions
        .iter()
        .filter_map(|a| match a {
            Action::ScheduleWakeup(id, _) => Some(id),
            _ => None,
        })
        .collect();
    assert!(wakeup_ids.contains(&&WakeupId::Heartbeat));
    assert!(wakeup_ids.contains(&&WakeupId::DiskCheck));
    assert!(wakeup_ids.contains(&&WakeupId::TorrentCheck));
}

#[test]
fn init_healthy_without_files_waits_for_watcher() {
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::Init {
        is_gluetun_healthy: true,
        port_files: Err("not ready".into()),
    });
    assert_eq!(state.vpn, VpnState::AwaitingTunnel);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
}

#[test]
fn init_unhealthy_triggers_workflow_a() {
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::Init {
        is_gluetun_healthy: false,
        port_files: Err("n/a".into()),
    });
    assert_eq!(state.vpn, VpnState::DumpingLogs);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}

#[test]
fn manual_reset_in_active_mode_clears_recovery_counter() {
    // vpn = Stopped — no re-auth triggered
    let mut state = SystemState::initial();
    state.hard_recoveries = RetryCount(2);
    let actions = state.process_event(Event::ManualReset);
    assert_eq!(state.hard_recoveries, RetryCount(0));
    assert!(actions.is_empty());
}

#[test]
fn manual_reset_when_connected_re_authenticates_qbit() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected {
        ip: ip(),
        port: port(),
    };
    state.hard_recoveries = RetryCount(2);
    let actions = state.process_event(Event::ManualReset);
    assert_eq!(state.hard_recoveries, RetryCount(0));
    assert!(
        matches!(
            state.qbit,
            QbitState::Authenticating {
                attempt: RetryCount(0)
            }
        ),
        "qBit should be in Authenticating state after ManualReset"
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit)),
        "ManualReset on connected VPN must schedule AuthenticateQbit"
    );
}
