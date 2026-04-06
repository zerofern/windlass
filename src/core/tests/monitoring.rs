use crate::core::{actions::Action, events::Event, types::*};
use crate::types::{AlertPriority, TorrentName, WakeupId};
use uom::si::f64::Information;
use uom::si::information::gigabyte;

#[test]
fn low_disk_space_sends_alert() {
    let space = Information::new::<gigabyte>(20.0);
    let (_, actions) = SystemState::initial().process_event(Event::DiskSpaceObserved(space));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Warning, _)))
    );
}

#[test]
fn sufficient_disk_space_sends_no_alert() {
    let space = Information::new::<gigabyte>(200.0);
    let (_, actions) = SystemState::initial().process_event(Event::DiskSpaceObserved(space));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(_, _)))
    );
}

#[test]
fn disk_check_always_reschedules() {
    for gb in [20.0_f64, 200.0] {
        let space = Information::new::<gigabyte>(gb);
        let (_, actions) = SystemState::initial().process_event(Event::DiskSpaceObserved(space));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::DiskCheck, _))),
            "DiskCheck wakeup not rescheduled for {gb} GB"
        );
    }
}

#[test]
fn new_torrents_sends_alert_for_unseen_names() {
    let names = vec![
        TorrentName("Ubuntu.iso".into()),
        TorrentName("Fedora.iso".into()),
    ];
    let (new_state, actions) =
        SystemState::initial().process_event(Event::NewTorrentsObserved(names));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Info, _)))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::TorrentCheck, _)))
    );
    // Core remembers them for next time.
    assert!(
        new_state
            .known_torrents
            .contains(&TorrentName("Ubuntu.iso".into()))
    );
    assert!(
        new_state
            .known_torrents
            .contains(&TorrentName("Fedora.iso".into()))
    );
}

#[test]
fn already_known_torrents_send_no_alert() {
    let mut state = SystemState::initial();
    state
        .known_torrents
        .insert(TorrentName("Ubuntu.iso".into()));
    state
        .known_torrents
        .insert(TorrentName("Fedora.iso".into()));
    let names = vec![
        TorrentName("Ubuntu.iso".into()),
        TorrentName("Fedora.iso".into()),
    ];
    let (_, actions) = state.process_event(Event::NewTorrentsObserved(names));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(_, _)))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::TorrentCheck, _)))
    );
}

#[test]
fn mixed_known_and_new_torrents_alerts_only_for_new() {
    let mut state = SystemState::initial();
    state
        .known_torrents
        .insert(TorrentName("Ubuntu.iso".into()));
    let names = vec![
        TorrentName("Ubuntu.iso".into()),
        TorrentName("Debian.iso".into()),
    ];
    let (new_state, actions) = state.process_event(Event::NewTorrentsObserved(names));
    let alert = actions.iter().find_map(|a| match a {
        Action::SendGotifyAlert(AlertPriority::Info, msg) => Some(msg.clone()),
        _ => None,
    });
    assert!(alert.is_some(), "Expected an alert for the new torrent");
    assert!(alert.unwrap().contains("Debian.iso"));
    assert!(
        new_state
            .known_torrents
            .contains(&TorrentName("Ubuntu.iso".into()))
    );
    assert!(
        new_state
            .known_torrents
            .contains(&TorrentName("Debian.iso".into()))
    );
}

#[test]
fn empty_torrent_list_sends_no_alert_but_reschedules() {
    let (_, actions) = SystemState::initial().process_event(Event::NewTorrentsObserved(vec![]));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::SendGotifyAlert(_, _)))
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::TorrentCheck, _)))
    );
}
