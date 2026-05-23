use super::helpers::*;
use crate::actions::Action;
use crate::torrent::{TorrentRecord, TorrentState};
use crate::types::QbitState;
use chrono::Utc;
use windlass_types::{AlertPriority, MamTorrentId, TorrentHash, TorrentName, WakeupId};

fn hash(s: &str) -> TorrentHash {
    TorrentHash(s.into())
}

fn make_torrent(
    id: &str,
    state: TorrentState,
    seeding_secs: u64,
    downloaded: u64,
    mam_id: Option<u64>,
) -> TorrentRecord {
    TorrentRecord {
        hash: hash(id),
        name: TorrentName(format!("Torrent {id}")),
        state,
        seeding_time_secs: seeding_secs,
        downloaded_bytes: downloaded,
        mam_id: mam_id.map(MamTorrentId),
        seen_at: Utc::now(),
    }
}

#[test]
fn new_torrent_emits_set_all_files_priority() {
    let mut state = connected_state();
    let torrents = vec![make_torrent(
        "abc",
        TorrentState::Uploading,
        100,
        1024,
        None,
    )];
    let actions = state.on_qbit_torrent_details_received(torrents);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SetAllFilesPriority(h, _) if h.0 == "abc"))
    );
}

#[test]
fn known_torrent_does_not_emit_set_all_files_priority() {
    let mut state = connected_state();
    let t = make_torrent("abc", TorrentState::Uploading, 100, 1024, None);
    state.torrents.insert(hash("abc"), t.clone());
    let actions = state.on_qbit_torrent_details_received(vec![t]);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::SetAllFilesPriority(h, _) if h.0 == "abc"))
    );
}

#[test]
fn stalled_zero_bytes_emits_delete_and_blacklist() {
    let mut state = connected_state();
    let t = make_torrent("dead", TorrentState::StalledDownloading, 0, 0, Some(99));
    let actions = state.on_qbit_torrent_details_received(vec![t]);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "dead"))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::BlacklistMamId(m) if m.0 == 99))
    );
    assert!(
        actions.iter().any(
            |a| matches!(a, Action::WriteActivity { action, .. } if action == "torrent_deleted")
        )
    );
}

#[test]
fn stalled_with_bytes_and_under_72h_emits_hnr_alert_not_delete() {
    let mut state = connected_state();
    let t = make_torrent("risk", TorrentState::StalledUploading, 3600, 1024, None);
    let actions = state.on_qbit_torrent_details_received(vec![t]);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendAlert { title, .. } if title == "HnR at risk"))
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "risk"))
    );
}

#[test]
fn unsatisfied_at_quota_minus_5_emits_warning() {
    let mut state = connected_state();
    state.unsatisfied_quota_limit = 10;
    let torrents: Vec<_> = (0..5)
        .map(|i| make_torrent(&i.to_string(), TorrentState::Uploading, 3600, 1024, None))
        .collect();
    let actions = state.on_qbit_torrent_details_received(torrents);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendAlert { title, priority, .. }
        if title == "Approaching quota limit" && priority == &AlertPriority::Warning))
    );
}

#[test]
fn unsatisfied_at_quota_limit_emits_critical() {
    let mut state = connected_state();
    state.unsatisfied_quota_limit = 5;
    let torrents: Vec<_> = (0..5)
        .map(|i| make_torrent(&i.to_string(), TorrentState::Uploading, 3600, 1024, None))
        .collect();
    let actions = state.on_qbit_torrent_details_received(torrents);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendAlert { title, priority, .. }
        if title == "Quota limit reached" && priority == &AlertPriority::Critical))
    );
}

#[test]
fn active_limit_full_pauses_oldest_satisfied_resumes_unsatisfied() {
    let mut state = connected_state();
    state.max_active_torrents = 2;
    let torrents = vec![
        make_torrent("active1", TorrentState::Uploading, 80 * 3600, 1024, None),
        make_torrent("active2", TorrentState::Uploading, 100 * 3600, 1024, None),
        make_torrent("parked", TorrentState::PausedUploading, 3600, 1024, None),
    ];
    let actions = state.on_qbit_torrent_details_received(torrents);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::PauseTorrent(h, _) if h.0 == "active2"))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ForceResumeTorrent(h, _) if h.0 == "parked"))
    );
}

#[test]
fn active_limit_emits_torrent_paused_activity() {
    let mut state = connected_state();
    state.max_active_torrents = 2;
    let torrents = vec![
        make_torrent("active1", TorrentState::Uploading, 80 * 3600, 1024, None),
        make_torrent("active2", TorrentState::Uploading, 100 * 3600, 1024, None),
        make_torrent("parked", TorrentState::PausedUploading, 3600, 1024, None),
    ];
    let actions = state.on_qbit_torrent_details_received(torrents);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::WriteActivity { action, detail, .. }
            if action == "torrent_paused"
            && detail.as_deref().map_or(false, |d| d.contains("active2"))))
    );
}

#[test]
fn active_limit_emits_torrent_resumed_activity() {
    let mut state = connected_state();
    state.max_active_torrents = 2;
    let torrents = vec![
        make_torrent("active1", TorrentState::Uploading, 80 * 3600, 1024, None),
        make_torrent("active2", TorrentState::Uploading, 100 * 3600, 1024, None),
        make_torrent("parked", TorrentState::PausedUploading, 3600, 1024, None),
    ];
    let actions = state.on_qbit_torrent_details_received(torrents);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::WriteActivity { action, detail, .. }
            if action == "torrent_resumed"
            && detail.as_deref().map_or(false, |d| d.contains("parked"))))
    );
}

#[test]
fn delete_requested_hnr_locked_blocks_deletion() {
    let mut state = connected_state();
    state.torrents.insert(
        hash("locked"),
        make_torrent("locked", TorrentState::Uploading, 3600, 1024, None),
    );
    let actions = state.on_delete_torrent_requested(hash("locked"));
    assert!(actions.iter().any(
        |a| matches!(a, Action::SendAlert { title, .. } if title == "HnR lock — cannot delete")
    ));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::DeleteTorrent(..)))
    );
}

#[test]
fn delete_requested_satisfied_allows_deletion() {
    let mut state = connected_state();
    state.torrents.insert(
        hash("done"),
        make_torrent("done", TorrentState::Uploading, 80 * 3600, 1024, None),
    );
    let actions = state.on_delete_torrent_requested(hash("done"));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "done"))
    );
}

#[test]
fn delete_requested_zero_bytes_allows_deletion() {
    let mut state = connected_state();
    state.torrents.insert(
        hash("empty"),
        make_torrent("empty", TorrentState::StalledDownloading, 0, 0, None),
    );
    let actions = state.on_delete_torrent_requested(hash("empty"));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "empty"))
    );
}

#[test]
fn delete_requested_unknown_hash_allows_deletion() {
    let state = connected_state();
    let actions = state.on_delete_torrent_requested(hash("unknown"));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::DeleteTorrent(h, _) if h.0 == "unknown"))
    );
}

#[test]
fn compliance_poll_wakeup_schedules_next_and_fetches_when_ready() {
    let state = connected_state();
    let actions = state.on_wakeup_compliance_poll();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::CompliancePoll, _)))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchTorrentDetails(_)))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::FetchQbitPreferences(_)))
    );
}

#[test]
fn compliance_poll_wakeup_no_fetch_when_qbit_offline() {
    let mut state = connected_state();
    state.qbit = QbitState::Offline;
    let actions = state.on_wakeup_compliance_poll();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::CompliancePoll, _)))
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::FetchTorrentDetails(_)))
    );
}
