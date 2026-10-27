//! Scenario tests for the torrent registry: adoption, saved observations, and
//! the commands that make qBittorrent match it.

use std::time::{Duration, Instant, SystemTime};

use windlass::core::{
    Action, ByteSize, COUNTER_SAVE_INTERVAL, ClockReading, Completion, Core, Desired, Event,
    ForceStart, Identity, LEASE_LIFETIME, MamTorrentId, POLL_INTERVAL, Port, PortForwardingEvent,
    Progress, QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences, RecordId, Records,
    Registry, RegistryEvent, StartupEvent, StartupMode, SyncPosition, SyncUpdate, TimerName,
    TorrentCommand, TorrentHash, TorrentRecord, TorrentState, TorrentUpdate,
};

type TestResult = Result<(), &'static str>;

/// A core with a clock that only moves when told to.
struct Harness {
    core: Core,
    start: Instant,
    elapsed: Duration,
    position: u64,
}

impl Harness {
    /// A started core with a connected qBittorrent and a registry read from
    /// these rows.
    fn new(rows: Vec<TorrentRecord>) -> Self {
        let mut harness = Self {
            core: Core::new(),
            start: Instant::now(),
            elapsed: Duration::ZERO,
            position: 0,
        };
        let _ = harness.send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }));
        let _ = harness.send(Event::Startup(StartupEvent::StorageOpened));
        assert_eq!(harness.core.registry(), &Registry::Reading);
        assert!(
            harness
                .send(Event::Registry(RegistryEvent::Read(rows)))
                .is_empty()
        );
        let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::Enable));
        let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::Connected {
            version: windlass::core::ReportedVersion::Release(windlass::core::ClientVersion::new(
                5, 2, 2,
            )),
            preferences: QbittorrentPreferences::default(),
        }));
        harness
    }

    fn advance(&mut self, by: Duration) {
        self.elapsed += by;
    }

    fn send(&mut self, event: Event) -> Vec<Action> {
        let now = ClockReading {
            monotonic: self.start + self.elapsed,
            wall: SystemTime::UNIX_EPOCH + self.elapsed,
        };
        let mut actions = self.core.handle(now, event);
        // Notification timers have their own tests.
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

    /// Deliver the next update; the first is the full list.
    fn sync(&mut self, torrents: Vec<(TorrentHash, TorrentUpdate)>) -> Vec<Action> {
        self.position += 1;
        self.send(Event::Qbittorrent(QbittorrentEvent::Synced(SyncUpdate {
            position: SyncPosition::new(self.position),
            scope: if self.position == 1 {
                windlass::core::SyncScope::Full
            } else {
                windlass::core::SyncScope::Changes
            },
            torrents,
            removed: Vec::new(),
        })))
    }

    /// Wait out the poll timer an action list started, and request the next sync.
    fn poll(&mut self, actions: &[Action]) {
        let id = actions.iter().find_map(|action| match action {
            Action::StartTimer {
                name: TimerName::QbittorrentPoll,
                id,
                ..
            } => Some(*id),
            _ => None,
        });
        let Some(id) = id else {
            unreachable!("no poll timer in {actions:?}");
        };
        self.advance(POLL_INTERVAL);
        let _ = self.send(Event::TimerExpired {
            name: TimerName::QbittorrentPoll,
            id,
        });
    }

    fn records(&self) -> &Records {
        match self.core.registry() {
            Registry::Loaded(records) => records,
            other => unreachable!("registry not loaded: {other:?}"),
        }
    }
}

fn hash(fill: char) -> TorrentHash {
    TorrentHash::parse(&fill.to_string().repeat(40)).unwrap_or_else(|| unreachable!("valid hash"))
}

fn torrent(state: TorrentState, size: u64, total_size: u64) -> TorrentUpdate {
    TorrentUpdate {
        name: Some(windlass::core::TorrentName::try_new("A Synthetic Audiobook").ok()),
        state: Some(state),
        progress: Progress::from_fraction(0.5),
        size: Some(ByteSize::new(size)),
        total_size: Some(ByteSize::new(total_size)),
        save_path: Some(windlass::core::SavePath::try_new("/srv/books").ok()),
        ..TorrentUpdate::default()
    }
}

fn record(id: u64, identity: Identity, desired: Desired) -> Result<TorrentRecord, &'static str> {
    Ok(TorrentRecord {
        id: RecordId::new(id).ok_or("zero id")?,
        identity,
        origin: windlass::core::Origin::Bookmark,
        desired,
        save_path: None,
        deletion_allowed: true,
        observation: None,
    })
}

/// The rows a save action carries.
fn saved(actions: &[Action]) -> Vec<TorrentRecord> {
    actions
        .iter()
        .find_map(|action| match action {
            Action::SaveTorrents { records } => Some(records.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn command(actions: &[Action]) -> Option<&TorrentCommand> {
    actions.iter().find_map(|action| match action {
        Action::QbittorrentCommand { command } => Some(command),
        _ => None,
    })
}

#[test]
fn unknown_torrents_are_adopted_as_active() {
    let mut harness = Harness::new(Vec::new());
    let seeding = torrent(TorrentState::Seeding, 10, 10);
    let actions = harness.sync(vec![
        (hash('a'), seeding),
        (hash('b'), torrent(TorrentState::Downloading, 5, 5)),
    ]);
    let rows = saved(&actions);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.origin, windlass::core::Origin::Adopted);
        assert_eq!(row.desired, Desired::Active);
        assert_eq!(
            row.save_path,
            windlass::core::SavePath::try_new("/srv/books").ok()
        );
        assert!(!row.deletion_allowed);
        assert_eq!(row.identity.mam_id(), None);
    }
    let summary: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.id.get(),
                row.identity.clone(),
                row.observation.as_ref().map(|seen| seen.last_seen),
            )
        })
        .collect();
    let seen = Some(SystemTime::UNIX_EPOCH);
    assert_eq!(
        summary,
        vec![
            (1, Identity::Client(hash('a')), seen),
            (2, Identity::Client(hash('b')), seen)
        ]
    );
    assert_eq!(
        harness
            .records()
            .find(&hash('b'))
            .map(|row| row.identity.hash().cloned()),
        Some(Some(hash('b')))
    );
    assert_eq!(harness.records().records().count(), 2);
}

#[test]
fn significant_changes_save_at_once_and_counters_every_fifteen_minutes() {
    let mut harness = Harness::new(Vec::new());
    let actions = harness.sync(vec![(hash('a'), torrent(TorrentState::Seeding, 10, 10))]);
    assert_eq!(saved(&actions).len(), 1);
    assert!(
        harness
            .send(Event::Registry(RegistryEvent::Saved))
            .is_empty()
    );
    harness.poll(&actions);
    let upload = TorrentUpdate {
        uploaded: Some(ByteSize::new(100)),
        ..TorrentUpdate::default()
    };
    let actions = harness.sync(vec![(hash('a'), upload)]);
    assert!(saved(&actions).is_empty(), "counters wait for the interval");
    harness.poll(&actions);
    let actions = harness.sync(Vec::new());
    assert!(saved(&actions).is_empty());
    harness.advance(COUNTER_SAVE_INTERVAL);
    harness.poll(&actions);
    let actions = harness.sync(Vec::new());
    let rows = saved(&actions);
    assert_eq!(
        rows.first()
            .and_then(|row| row.observation.as_ref())
            .map(|seen| seen.torrent.uploaded),
        Some(ByteSize::new(100)),
        "unsaved counters are saved once due"
    );
    let _ = harness.send(Event::Registry(RegistryEvent::Saved));
    harness.poll(&actions);
    let stalled = TorrentUpdate {
        state: Some(TorrentState::SeedStalled),
        ..TorrentUpdate::default()
    };
    let actions = harness.sync(vec![(hash('a'), stalled)]);
    assert_eq!(saved(&actions).len(), 1, "a state change saves at once");
}

#[test]
fn one_batch_is_saved_at_a_time_and_failures_are_retried() {
    let mut harness = Harness::new(Vec::new());
    let actions = harness.sync(vec![(hash('a'), torrent(TorrentState::Seeding, 10, 10))]);
    assert_eq!(saved(&actions).len(), 1);
    harness.poll(&actions);
    let actions = harness.sync(vec![(hash('b'), torrent(TorrentState::Seeding, 10, 10))]);
    assert!(saved(&actions).is_empty(), "the first batch is outstanding");
    let flushed = harness.send(Event::Registry(RegistryEvent::Saved));
    assert_eq!(
        saved(&flushed)
            .iter()
            .map(|row| row.identity.clone())
            .collect::<Vec<_>>(),
        vec![Identity::Client(hash('b'))]
    );
    assert!(
        harness
            .send(Event::Registry(RegistryEvent::SaveFailed))
            .is_empty()
    );
    assert!(harness.records().save_failed());
    assert!(
        harness
            .send(Event::Registry(RegistryEvent::SaveFailed))
            .is_empty(),
        "a failure without an outstanding batch changes nothing"
    );
    harness.poll(&actions);
    let retried = harness.sync(Vec::new());
    assert_eq!(saved(&retried).len(), 1, "the failed batch is saved again");
    let _ = harness.send(Event::Registry(RegistryEvent::Saved));
    assert!(!harness.records().save_failed());
    assert!(
        harness
            .send(Event::Registry(RegistryEvent::Saved))
            .is_empty(),
        "an unexpected completion changes nothing"
    );
}

#[test]
fn stopped_active_torrents_are_started_without_repeating_too_soon() {
    let mut harness = Harness::new(Vec::new());
    let stopped = torrent(TorrentState::Stopped(Completion::Complete), 10, 10);
    let actions = harness.sync(vec![(hash('a'), stopped)]);
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::Start(vec![hash('a')]))
    );
    assert_eq!(
        harness.send(Event::Qbittorrent(QbittorrentEvent::CommandDone)),
        vec![Action::SyncQbittorrent {
            position: SyncPosition::new(1)
        }],
        "the result is read at once"
    );
    // qBittorrent has not started it yet: the same command waits a minute.
    let actions = harness.sync(Vec::new());
    assert_eq!(command(&actions), None);
    harness.advance(Duration::from_mins(1));
    harness.poll(&actions);
    let actions = harness.sync(Vec::new());
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::Start(vec![hash('a')]))
    );
    assert!(matches!(
        harness
            .send(Event::Qbittorrent(QbittorrentEvent::CommandFailed {
                reason: QbittorrentFailure::Rejected { status: 404 },
            }))
            .as_slice(),
        [Action::StartTimer {
            name: TimerName::QbittorrentPoll,
            ..
        }]
    ));
}

#[test]
fn starting_and_force_starting_never_add_an_unsatisfied_torrent() -> TestResult {
    let mam = MamTorrentId::new(5).ok_or("zero id")?;
    let ours = record(
        1,
        Identity::Linked {
            mam,
            hash: hash('b'),
        },
        Desired::Active,
    )?;
    let mut harness = Harness::new(vec![ours]);
    let untouched = TorrentUpdate {
        state: Some(TorrentState::Stopped(Completion::Incomplete)),
        progress: Some(Progress::default()),
        downloaded: Some(ByteSize::new(0)),
        ..TorrentUpdate::default()
    };
    // A torrent found stopped that has downloaded nothing stays stopped;
    // Windlass's own download, admitted against the limit, is started.
    let actions = harness.sync(vec![(hash('a'), untouched.clone()), (hash('b'), untouched)]);
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::Start(vec![hash('b')]))
    );
    let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::CommandDone));
    let seeded = |hours: u64, forced: ForceStart| TorrentUpdate {
        state: Some(TorrentState::SeedQueued),
        progress: Progress::from_fraction(1.0),
        completed_at: Some(Some(SystemTime::UNIX_EPOCH)),
        seeding_time: Some(Duration::from_hours(hours)),
        force_start: Some(forced),
        ..TorrentUpdate::default()
    };
    // Past MAM's 72 hours but short of the 96-hour target: still forced.
    let actions = harness.sync(vec![
        (
            hash('b'),
            TorrentUpdate {
                state: Some(TorrentState::Downloading),
                ..TorrentUpdate::default()
            },
        ),
        (hash('c'), seeded(80, ForceStart::NotForced)),
        (hash('d'), seeded(97, ForceStart::Forced)),
    ]);
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::ForceStart(vec![hash('c')]))
    );
    let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::CommandDone));
    let actions = harness.sync(vec![(hash('c'), seeded(80, ForceStart::Forced))]);
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::EndForceStart(vec![hash('d')]))
    );
    assert_eq!(
        windlass::core::SEEDING_TARGET,
        Duration::from_hours(96),
        "a day beyond MAM's {:?}",
        windlass::core::SEEDING_REQUIRED
    );
    Ok(())
}

#[test]
fn partial_torrents_get_every_file_once_metadata_is_known() {
    let mut harness = Harness::new(Vec::new());
    let actions = harness.sync(vec![
        (hash('a'), torrent(TorrentState::FetchingMetadata, 0, 10)),
        (hash('b'), torrent(TorrentState::Downloading, 5, 10)),
    ]);
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::SelectAllFiles(hash('b')))
    );
}

#[test]
fn removed_torrents_are_deleted_with_their_files() -> TestResult {
    let mam = MamTorrentId::new(12345).ok_or("zero MAM id")?;
    let rows = vec![
        record(
            4,
            Identity::Linked {
                mam,
                hash: hash('a'),
            },
            Desired::Removed,
        )?,
        record(7, Identity::Mam(mam), Desired::Queued)?,
        record(9, Identity::Client(hash('c')), Desired::Removed)?,
        record(
            8,
            Identity::Mam(MamTorrentId::new(5).ok_or("zero MAM id")?),
            Desired::Removed,
        )?,
    ];
    let mut harness = Harness::new(rows);
    let actions = harness.sync(vec![
        (hash('a'), torrent(TorrentState::Seeding, 10, 10)),
        (hash('b'), torrent(TorrentState::Seeding, 10, 10)),
    ]);
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::Remove(vec![hash('a')])),
        "a removed row absent from qBittorrent needs nothing"
    );
    let adopted = saved(&actions)
        .into_iter()
        .find(|row| row.identity == Identity::Client(hash('b')))
        .map(|row| row.id.get());
    assert_eq!(adopted, Some(10), "new rows follow the highest stored id");
    assert_eq!(
        harness.records().find(&hash('a')).map(|row| row.desired),
        Some(Desired::Removed)
    );
    assert_eq!(harness.records().find(&hash('d')), None);
    assert_eq!(mam.get(), 12345);
    Ok(())
}

#[test]
fn the_listen_port_comes_before_registry_commands() {
    let mut harness = Harness::new(Vec::new());
    let _ = harness.send(Event::PortForwarding(PortForwardingEvent::Enable));
    let forwarded = Port::new(41000).unwrap_or_else(|| unreachable!("non-zero port"));
    let _ = harness.send(Event::PortForwarding(PortForwardingEvent::Mapped {
        tcp: forwarded,
        udp: forwarded,
        lifetime: LEASE_LIFETIME,
    }));
    let stopped = torrent(TorrentState::Stopped(Completion::Incomplete), 10, 10);
    let actions = harness.sync(vec![(hash('a'), stopped)]);
    assert!(actions.contains(&Action::SetQbittorrentListenPort { port: forwarded }));
    assert_eq!(command(&actions), None);
    let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::ListenPortSet));
    let actions = harness.send(Event::Qbittorrent(QbittorrentEvent::PreferencesRead(
        QbittorrentPreferences {
            listen_port: Some(forwarded),
            interface: None,
            queue: None,
        },
    )));
    harness.poll(&actions);
    let actions = harness.sync(Vec::new());
    assert_eq!(
        command(&actions),
        Some(&TorrentCommand::Start(vec![hash('a')])),
        "the command was not used up while the port changed"
    );
}

#[test]
fn nothing_is_managed_before_the_full_list_or_without_the_registry() {
    let mut harness = Harness::new(Vec::new());
    harness.position = 1;
    let actions = harness.sync(vec![(
        hash('a'),
        torrent(TorrentState::Stopped(Completion::Complete), 1, 1),
    )]);
    assert!(saved(&actions).is_empty(), "the list is incomplete");
    assert_eq!(command(&actions), None);

    let mut core = Core::new();
    let now = ClockReading {
        monotonic: Instant::now(),
        wall: SystemTime::UNIX_EPOCH,
    };
    let _ = core.handle(
        now,
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
    );
    assert!(
        core.handle(now, Event::Registry(RegistryEvent::Read(Vec::new())))
            .is_empty(),
        "results nobody asked for are ignored"
    );
    let _ = core.handle(now, Event::Startup(StartupEvent::StorageOpened));
    let _ = core.handle(now, Event::Registry(RegistryEvent::ReadFailed));
    assert_eq!(core.registry(), &Registry::ReadFailed);
    let _ = core.handle(now, Event::Qbittorrent(QbittorrentEvent::Enable));
    let _ = core.handle(
        now,
        Event::Qbittorrent(QbittorrentEvent::Connected {
            version: windlass::core::ReportedVersion::Release(windlass::core::ClientVersion::new(
                5, 2, 2,
            )),
            preferences: QbittorrentPreferences::default(),
        }),
    );
    let actions = core.handle(
        now,
        Event::Qbittorrent(QbittorrentEvent::Synced(SyncUpdate {
            position: SyncPosition::new(1),
            scope: windlass::core::SyncScope::Full,
            torrents: vec![(
                hash('a'),
                torrent(TorrentState::Stopped(Completion::Complete), 1, 1),
            )],
            removed: Vec::new(),
        })),
    );
    assert!(matches!(
        actions.as_slice(),
        [Action::StartTimer {
            name: TimerName::QbittorrentPoll,
            ..
        }]
    ));
}

#[test]
fn record_ids_are_validated() {
    assert_eq!(RecordId::new(0), None);
    assert_eq!(MamTorrentId::new(0), None);
    assert_eq!(Identity::new(None, None), None);
    let mam = MamTorrentId::new(1);
    assert_eq!(
        Identity::new(mam, Some(hash('a'))).and_then(|identity| identity.mam_id()),
        mam
    );
    assert_eq!(
        Identity::new(mam, None).and_then(|identity| identity.hash().cloned()),
        None
    );
}
