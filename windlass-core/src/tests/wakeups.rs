use super::helpers::*;
use crate::{actions::Action, events::Event, types::*};
use windlass_types::{RetryCount, WakeupId};

#[test]
fn wakeup_heartbeat_checks_mam_connectability() {
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::Wakeup(WakeupId::Heartbeat));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::CheckMamConnectability))
    );
}

#[test]
fn wakeup_disk_check_checks_disk_space() {
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::Wakeup(WakeupId::DiskCheck));
    assert!(actions.iter().any(|a| matches!(a, Action::CheckDiskSpace)));
}

#[test]
fn wakeup_torrent_check_checks_new_torrents() {
    let mut state = connected_state();
    let actions = state.process_event(Event::Wakeup(WakeupId::TorrentCheck));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::CheckNewTorrents(_)))
    );
}

#[test]
fn wakeup_torrent_check_is_noop_when_qbit_not_ready() {
    // Core must not emit CheckNewTorrents if we have no valid cookie.
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::Wakeup(WakeupId::TorrentCheck));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::CheckNewTorrents(_)))
    );
}

#[test]
fn wakeup_qbit_auth_retry_authenticates() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating {
        attempt: RetryCount(0),
    };
    let actions = state.process_event(Event::Wakeup(WakeupId::QbitAuthRetry));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::AuthenticateQbit))
    );
}

#[test]
fn wakeup_qbit_sync_retry_syncs_when_in_syncing_state() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort {
        attempt: RetryCount(1),
        cookie: cookie(),
        target: port(),
    };
    let actions = state.process_event(Event::Wakeup(WakeupId::QbitSyncRetry));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SyncQbitPort(_, _)))
    );
}

#[test]
fn wakeup_qbit_sync_retry_is_noop_when_not_syncing() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Offline;
    let actions = state.process_event(Event::Wakeup(WakeupId::QbitSyncRetry));
    assert!(actions.is_empty());
}

#[test]
fn wakeup_retry_port_read_reads_port_files() {
    let mut state = SystemState::initial();
    let actions = state.process_event(Event::Wakeup(WakeupId::RetryPortRead));
    assert!(actions.iter().any(|a| matches!(a, Action::ReadPortFiles)));
}
