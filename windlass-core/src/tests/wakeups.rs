use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use chrono::Utc;
use windlass_types::{RetryCount, WakeupId};

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

#[test]
fn wakeup_heartbeat_is_service_orchestration_noop() {
    let mut state = SystemState::initial();
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::Heartbeat,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.is_empty());
}

#[test]
fn wakeup_disk_check_checks_disk_space() {
    let mut state = SystemState::initial();
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::DiskCheck,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.iter().any(|a| matches!(a, Action::CheckDiskSpace)));
}

#[test]
fn wakeup_torrent_check_is_service_orchestration_noop() {
    let mut state = connected_state();
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::TorrentCheck,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.is_empty());
}

#[test]
fn wakeup_torrent_check_is_noop_when_qbit_not_ready() {
    // Core must not emit CheckNewTorrents if we have no valid cookie.
    let mut state = SystemState::initial();
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::TorrentCheck,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::CheckNewTorrents(_)))
    );
}

#[test]
fn wakeup_qbit_auth_retry_is_service_orchestration_noop() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(0),
    };
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::QbitAuthRetry,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.is_empty());
}

#[test]
fn wakeup_qbit_sync_retry_is_service_orchestration_noop() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort {
        attempt: RetryCount(1),
        cookie: cookie(),
        target: port(),
    };
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::QbitSyncRetry,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.is_empty());
}

#[test]
fn wakeup_qbit_sync_retry_is_noop_when_not_syncing() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Offline;
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::QbitSyncRetry,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.is_empty());
}

#[test]
fn wakeup_retry_port_read_reads_port_files_for_legacy_state_bridge() {
    let mut state = SystemState::initial();
    let outcome = state.process_event(
        Event::Wakeup {
            at: now(),
            id: WakeupId::RetryPortRead,
        },
        now(),
    );
    let actions = outcome.actions;
    assert!(!outcome.state_changed);
    assert!(actions.iter().any(|a| matches!(a, Action::ReadPortFiles)));
}
