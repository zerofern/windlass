use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use chrono::Utc;
use windlass_types::{RetryCount, WakeupId};

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

#[test]
fn init_healthy_with_files_fast_forwards_to_connected_and_auths() {
    let mut state = SystemState::initial();
    let actions = state.process_event(
        Event::Init {
            at: now(),
            is_gluetun_healthy: true,
            port_files: Ok((ip(), port())),
        },
        now(),
    );
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
    let actions = state.process_event(
        Event::Init {
            at: now(),
            is_gluetun_healthy: true,
            port_files: Err("not ready".into()),
        },
        now(),
    );
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
    let actions = state.process_event(
        Event::Init {
            at: now(),
            is_gluetun_healthy: false,
            port_files: Err("n/a".into()),
        },
        now(),
    );
    assert_eq!(state.vpn, VpnState::DumpingLogs);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchAndDumpAllLogs))
    );
}
