//! Scenario tests for port forwarding, qBittorrent, and timers, with supplied time.

use std::time::{Duration, Instant, SystemTime};

use windlass::core::{
    Action, Activity, ByteSize, ClientVersion, ClockReading, Core, Event, LEASE_LIFETIME,
    MamApproval, MappingFailure, POLL_INTERVAL, Port, PortForwarding, PortForwardingEvent,
    Progress, Qbittorrent, QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences,
    SyncPosition, SyncUpdate, TimerId, TimerName, TorrentHash, TorrentState, TorrentUpdate,
};

/// A core with a clock that only moves when told to.
struct Harness {
    core: Core,
    start: Instant,
    elapsed: Duration,
}

impl Harness {
    fn new() -> Self {
        Self {
            core: Core::new(),
            start: Instant::now(),
            elapsed: Duration::ZERO,
        }
    }

    fn advance(&mut self, by: Duration) {
        self.elapsed += by;
    }

    /// Handle an event; notification timers are left out, as notifications
    /// have their own tests.
    fn send(&mut self, event: Event) -> Vec<Action> {
        let now = ClockReading {
            monotonic: self.start + self.elapsed,
            wall: SystemTime::UNIX_EPOCH + self.elapsed,
        };
        let mut actions = self.core.handle(now, event);
        actions.retain(|action| {
            !matches!(
                action,
                Action::StartTimer {
                    name: TimerName::NotificationGrace,
                    ..
                }
            )
        });
        actions
    }

    fn forwarding(&mut self, event: PortForwardingEvent) -> Vec<Action> {
        self.send(Event::PortForwarding(event))
    }

    fn qbittorrent(&mut self, event: QbittorrentEvent) -> Vec<Action> {
        self.send(Event::Qbittorrent(event))
    }

    fn expire(&mut self, name: TimerName, id: TimerId) -> Vec<Action> {
        self.send(Event::TimerExpired { name, id })
    }
}

fn port(number: u16) -> Port {
    Port::new(number).unwrap_or_else(|| unreachable!("non-zero test port"))
}

fn hash(fill: char) -> TorrentHash {
    TorrentHash::parse(&fill.to_string().repeat(40)).unwrap_or_else(|| unreachable!("valid hash"))
}

/// The single timer an action list starts.
fn timer(actions: &[Action], expected: TimerName, after: Duration) -> TimerId {
    match actions {
        [Action::StartTimer { name, id, deadline }]
            if *name == expected && deadline.after == after =>
        {
            *id
        }
        other => unreachable!("expected one {expected:?} timer after {after:?}, got {other:?}"),
    }
}

fn mapped(number: u16) -> PortForwardingEvent {
    PortForwardingEvent::Mapped {
        tcp: port(number),
        udp: port(number),
        lifetime: LEASE_LIFETIME,
    }
}

fn preferences(listen: u16) -> QbittorrentPreferences {
    QbittorrentPreferences {
        listen_port: Port::new(listen),
        interface: Some(String::from("wg0")),
    }
}

fn session(harness: &Harness) -> &windlass::core::Session {
    match harness.core.qbittorrent() {
        Qbittorrent::Connected(session) => session,
        other => unreachable!("qBittorrent is not connected: {other:?}"),
    }
}

#[test]
fn leases_are_renewed_before_they_expire() {
    let mut harness = Harness::new();
    assert_eq!(harness.core.port_forwarding(), &PortForwarding::Disabled);
    let request = vec![Action::RequestPortMapping {
        lifetime: LEASE_LIFETIME,
    }];
    assert_eq!(harness.forwarding(PortForwardingEvent::Enable), request);
    assert!(harness.forwarding(PortForwardingEvent::Enable).is_empty());
    let renew = timer(
        &harness.forwarding(mapped(41000)),
        TimerName::PortLease,
        Duration::from_secs(45),
    );
    assert_eq!(
        harness.core.port_forwarding().forwarded_port(),
        Some(port(41000))
    );
    // An unexpected answer while no request is outstanding changes nothing.
    assert!(harness.forwarding(mapped(1)).is_empty());
    harness.advance(Duration::from_secs(45));
    assert_eq!(harness.expire(TimerName::PortLease, renew), request);
    assert!(
        harness.expire(TimerName::PortLease, renew).is_empty(),
        "a duplicate expiry runs nothing"
    );
    let _ = harness.forwarding(mapped(42000));
    assert_eq!(
        harness.core.port_forwarding().forwarded_port(),
        Some(port(42000))
    );
}

#[test]
fn failed_renewals_retry_and_the_lease_lapses() {
    let mut harness = Harness::new();
    let _ = harness.forwarding(PortForwardingEvent::Enable);
    let renew = timer(
        &harness.forwarding(mapped(41000)),
        TimerName::PortLease,
        Duration::from_secs(45),
    );
    harness.advance(Duration::from_secs(45));
    let _ = harness.expire(TimerName::PortLease, renew);
    let retry = timer(
        &harness.forwarding(PortForwardingEvent::MappingFailed {
            reason: MappingFailure::NoResponse,
        }),
        TimerName::PortLease,
        Duration::from_secs(5),
    );
    assert_eq!(
        harness.core.port_forwarding().forwarded_port(),
        Some(port(41000)),
        "the lease is still valid"
    );
    assert!(
        harness.expire(TimerName::PortLease, renew).is_empty(),
        "the replaced timer is obsolete"
    );
    harness.advance(Duration::from_secs(15));
    let _ = harness.expire(TimerName::PortLease, retry);
    let _ = harness.forwarding(PortForwardingEvent::MappingFailed {
        reason: MappingFailure::Refused { result_code: 2 },
    });
    assert_eq!(harness.core.port_forwarding().forwarded_port(), None);
    match harness.core.port_forwarding() {
        PortForwarding::Enabled {
            lease: None,
            failures: 2,
            last_failure: Some(MappingFailure::Refused { result_code: 2 }),
            ..
        } => {}
        other => unreachable!("unexpected state {other:?}"),
    }
}

#[test]
fn zero_lifetimes_count_as_failures() {
    let mut harness = Harness::new();
    let _ = harness.forwarding(PortForwardingEvent::Enable);
    let _ = timer(
        &harness.forwarding(PortForwardingEvent::Mapped {
            tcp: port(1),
            udp: port(1),
            lifetime: Duration::ZERO,
        }),
        TimerName::PortLease,
        Duration::from_secs(5),
    );
    assert_eq!(harness.core.port_forwarding().forwarded_port(), None);
}

#[test]
fn versions_follow_mams_client_list() {
    for (version, approval) in [
        ("v5.2.2", MamApproval::Recommended),
        ("5.2.0", MamApproval::Recommended),
        ("v5.1.4", MamApproval::Allowed),
        ("v5.0.1", MamApproval::Allowed),
        ("v5.0.0", MamApproval::NotAllowed),
        ("v4.6.7", MamApproval::Discouraged),
        ("v4.5.2", MamApproval::Discouraged),
        ("v4.4.5", MamApproval::Discouraged),
        ("v4.3.4", MamApproval::Discouraged),
        ("v4.5.1", MamApproval::NotAllowed),
        ("v4.3.3", MamApproval::NotAllowed),
        ("v5.3.0", MamApproval::NotAllowed),
    ] {
        let parsed = ClientVersion::parse(version).map(ClientVersion::mam_approval);
        assert_eq!(parsed, Some(approval), "{version}");
    }
    for invalid in [
        "v5.2.2beta1",
        "v5.2.2.1",
        "5",
        "latest",
        "x.2",
        "5.x",
        "5.x.1",
    ] {
        assert_eq!(ClientVersion::parse(invalid), None, "{invalid}");
    }
    assert_eq!(
        ClientVersion::parse("v5.2").map(|version| version.to_string()),
        Some(String::from("5.2.0"))
    );
}

#[test]
fn qbittorrent_is_polled_and_its_torrents_are_merged() {
    let mut harness = Harness::new();
    assert_eq!(
        harness.qbittorrent(QbittorrentEvent::Enable),
        vec![Action::ConnectQbittorrent]
    );
    assert_eq!(harness.core.qbittorrent(), &Qbittorrent::Connecting);
    assert_eq!(
        harness.qbittorrent(QbittorrentEvent::Connected {
            version: String::from("v5.2.2"),
            preferences: preferences(6881),
        }),
        vec![Action::SyncQbittorrent {
            position: SyncPosition(0)
        }]
    );
    assert_eq!(session(&harness).approval, MamApproval::Recommended);
    let full = SyncUpdate {
        position: SyncPosition(1),
        full: true,
        torrents: vec![
            (
                hash('a'),
                TorrentUpdate {
                    name: Some(String::from("first")),
                    state: Some(TorrentState::Downloading),
                    progress: Some(Progress::from_fraction(0.25)),
                    size: Some(ByteSize(100)),
                    ..TorrentUpdate::default()
                },
            ),
            (hash('b'), TorrentUpdate::default()),
        ],
        removed: Vec::new(),
    };
    let poll = timer(
        &harness.qbittorrent(QbittorrentEvent::Synced(full)),
        TimerName::QbittorrentPoll,
        POLL_INTERVAL,
    );
    assert_eq!(session(&harness).torrents.len(), 2);
    assert_eq!(session(&harness).activity, Activity::Waiting(poll));
    harness.advance(POLL_INTERVAL);
    assert_eq!(
        harness.expire(TimerName::QbittorrentPoll, poll),
        vec![Action::SyncQbittorrent {
            position: SyncPosition(1)
        }]
    );
    let partial = SyncUpdate {
        position: SyncPosition(2),
        full: false,
        torrents: vec![(
            hash('a'),
            TorrentUpdate {
                state: Some(TorrentState::Seeding),
                progress: Some(Progress::from_fraction(1.0)),
                ..TorrentUpdate::default()
            },
        )],
        removed: vec![hash('b')],
    };
    let _ = harness.qbittorrent(QbittorrentEvent::Synced(partial));
    let torrent = session(&harness).torrents.get(&hash('a')).cloned();
    assert_eq!(
        torrent.map(|torrent| (
            torrent.name,
            torrent.state,
            torrent.progress.permille(),
            torrent.size
        )),
        Some((
            String::from("first"),
            TorrentState::Seeding,
            1000,
            ByteSize(100)
        ))
    );
    assert!(!session(&harness).torrents.contains_key(&hash('b')));
    // An update arriving while none was requested is ignored.
    let _ = harness.qbittorrent(QbittorrentEvent::Synced(SyncUpdate {
        full: true,
        ..SyncUpdate::default()
    }));
    assert_eq!(session(&harness).torrents.len(), 1);
}

#[test]
fn preferences_are_reread_periodically() {
    let mut harness = Harness::new();
    let _ = harness.qbittorrent(QbittorrentEvent::Enable);
    let _ = harness.qbittorrent(QbittorrentEvent::Connected {
        version: String::from("v5.1.0"),
        preferences: preferences(6881),
    });
    let mut actions = Vec::new();
    for poll in 1..=6 {
        actions = harness.qbittorrent(QbittorrentEvent::Synced(SyncUpdate {
            position: SyncPosition(poll),
            ..SyncUpdate::default()
        }));
        if poll < 6 {
            let id = timer(&actions, TimerName::QbittorrentPoll, POLL_INTERVAL);
            let _ = harness.expire(TimerName::QbittorrentPoll, id);
        }
    }
    assert_eq!(actions, vec![Action::ReadQbittorrentPreferences]);
    let _ = timer(
        &harness.qbittorrent(QbittorrentEvent::PreferencesRead(preferences(7000))),
        TimerName::QbittorrentPoll,
        POLL_INTERVAL,
    );
    assert_eq!(session(&harness).preferences.listen_port, Some(port(7000)));
}

#[test]
fn qbittorrent_follows_the_forwarded_port() {
    let mut harness = Harness::new();
    let _ = harness.qbittorrent(QbittorrentEvent::Enable);
    let _ = harness.qbittorrent(QbittorrentEvent::Connected {
        version: String::from("v5.2.2"),
        preferences: preferences(6881),
    });
    let _ = harness.qbittorrent(QbittorrentEvent::Synced(SyncUpdate::default()));
    // The lease arrives while qBittorrent is idle: the port changes right away.
    let _ = harness.forwarding(PortForwardingEvent::Enable);
    let actions = harness.forwarding(mapped(41000));
    assert!(actions.contains(&Action::SetQbittorrentListenPort { port: port(41000) }));
    assert_eq!(
        session(&harness).activity,
        Activity::SettingListenPort(port(41000))
    );
    assert_eq!(
        harness.qbittorrent(QbittorrentEvent::ListenPortSet),
        vec![Action::ReadQbittorrentPreferences],
        "the change is confirmed by reading it back"
    );
    let poll = timer(
        &harness.qbittorrent(QbittorrentEvent::PreferencesRead(preferences(41000))),
        TimerName::QbittorrentPoll,
        POLL_INTERVAL,
    );
    // Matching ports need no change: the next poll just syncs.
    assert_eq!(
        harness.expire(TimerName::QbittorrentPoll, poll),
        vec![Action::SyncQbittorrent {
            position: SyncPosition(0)
        }]
    );
    // A new lease during a sync is applied once the sync completes.
    let _ = harness.forwarding(PortForwardingEvent::Enable);
    harness.advance(Duration::from_secs(45));
    let renew = match harness.core.port_forwarding() {
        PortForwarding::Enabled {
            request: windlass::core::LeaseRequest::Waiting(id),
            ..
        } => *id,
        other => unreachable!("no renewal timer: {other:?}"),
    };
    let _ = harness.expire(TimerName::PortLease, renew);
    let renewed = harness.forwarding(mapped(42000));
    assert!(
        !renewed
            .iter()
            .any(|action| matches!(action, Action::SetQbittorrentListenPort { .. }))
    );
    assert_eq!(
        harness.qbittorrent(QbittorrentEvent::Synced(SyncUpdate::default())),
        vec![Action::SetQbittorrentListenPort { port: port(42000) }]
    );
}

#[test]
fn failures_make_qbittorrent_unreachable_until_it_answers() {
    let mut harness = Harness::new();
    let _ = harness.qbittorrent(QbittorrentEvent::Enable);
    let retry = timer(
        &harness.qbittorrent(QbittorrentEvent::ConnectFailed {
            reason: QbittorrentFailure::Unreachable,
        }),
        TimerName::QbittorrentPoll,
        POLL_INTERVAL,
    );
    assert!(matches!(
        harness.core.qbittorrent(),
        Qbittorrent::Unreachable {
            reason: QbittorrentFailure::Unreachable,
            ..
        }
    ));
    assert_eq!(
        harness.expire(TimerName::QbittorrentPoll, retry),
        vec![Action::ConnectQbittorrent]
    );
    let _ = harness.qbittorrent(QbittorrentEvent::Connected {
        version: String::from("v5.2.2-dev"),
        preferences: QbittorrentPreferences::default(),
    });
    assert_eq!(session(&harness).approval, MamApproval::NotAllowed);
    for failure in [
        QbittorrentEvent::SyncFailed {
            reason: QbittorrentFailure::Rejected { status: 403 },
        },
        QbittorrentEvent::PreferencesReadFailed {
            reason: QbittorrentFailure::Malformed,
        },
        QbittorrentEvent::ListenPortSetFailed {
            reason: QbittorrentFailure::Unreachable,
        },
    ] {
        let retry = timer(
            &harness.qbittorrent(failure),
            TimerName::QbittorrentPoll,
            POLL_INTERVAL,
        );
        assert!(
            harness.expire(TimerName::PortLease, retry).is_empty(),
            "another timer's name does not match"
        );
        let _ = harness.expire(TimerName::QbittorrentPoll, retry);
        let _ = harness.qbittorrent(QbittorrentEvent::Connected {
            version: String::from("v5.2.2"),
            preferences: QbittorrentPreferences::default(),
        });
    }
    // A poll timer that was already used changes nothing.
    let poll = timer(
        &harness.qbittorrent(QbittorrentEvent::Synced(SyncUpdate::default())),
        TimerName::QbittorrentPoll,
        POLL_INTERVAL,
    );
    let _ = harness.expire(TimerName::QbittorrentPoll, poll);
    assert!(harness.expire(TimerName::QbittorrentPoll, poll).is_empty());
    // Results that do not match the outstanding request change nothing.
    assert!(
        harness
            .qbittorrent(QbittorrentEvent::ListenPortSet)
            .is_empty()
    );
    assert!(
        harness
            .qbittorrent(QbittorrentEvent::PreferencesRead(preferences(1)))
            .is_empty()
    );
    assert!(harness.qbittorrent(QbittorrentEvent::Enable).is_empty());
}

#[test]
fn hashes_and_progress_are_validated() {
    assert_eq!(
        TorrentHash::parse(&"AB".repeat(20)).map(|hash| hash.as_str().to_owned()),
        Some("ab".repeat(20))
    );
    assert!(TorrentHash::parse(&"a".repeat(64)).is_some());
    assert!(TorrentHash::parse(&"a".repeat(41)).is_none());
    assert!(TorrentHash::parse(&"g".repeat(40)).is_none());
    assert_eq!(Progress::from_fraction(-1.0).permille(), 0);
    assert_eq!(Progress::from_fraction(2.0).permille(), 1000);
    assert_eq!(Progress::from_fraction(0.1234).permille(), 123);
    assert_eq!(Port::new(0), None);
}
