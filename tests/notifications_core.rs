//! Scenario tests for notifications: grace periods, one-off notices, delivery,
//! and retries, with supplied time.

use std::time::{Duration, Instant, SystemTime};

use windlass::core::{
    Action, ClockReading, Condition, Core, Delivery, DeliveryFailure, Event, ExitIpFailure,
    LOG_KEPT, MamApproval, MamPauseEvent, MamRequestMode, Notice, NoticeId, NoticeLog,
    NotificationEvent, NtfyTopic, OUTBOX_LIMIT, Phase, Push, QbittorrentEvent, QbittorrentFailure,
    QbittorrentPreferences, Reach, SettingName, SettingsEvent, StartupEvent, StartupMode,
    SwitchBlock, TimerId, TimerName, TunnelEvent, VpnServer, VpnSwitch,
};

const TOPIC: &str = "windlass-alerts";

/// How far the notifications log was read at startup.
#[derive(Clone, Copy)]
enum Log {
    Unread,
    /// Read, with the newest stored id.
    Read(Option<u64>),
}

/// A core with a clock that only moves when told to.
struct Harness {
    core: Core,
    start: Instant,
    elapsed: Duration,
    /// Every notice saved to the log, with its id, oldest first.
    logged: Vec<(NoticeId, Notice)>,
}

impl Harness {
    /// A started core whose settings name `topic`, with an empty log.
    fn new(topic: Option<&str>) -> Self {
        let mut harness = Self::opened(Log::Read(None));
        let ready = harness.send(Event::MamPause(MamPauseEvent::Read {
            mode: Some(MamRequestMode::Paused),
        }));
        assert!(sent(&ready).is_empty(), "settings are not read yet");
        assert_eq!(
            harness.notices(),
            [Notice::Started {
                mode: StartupMode::Resume
            }]
        );
        let read = harness.send(Event::Settings(SettingsEvent::Read {
            history_limit: None,
            api_cookie: None,
            ntfy_topic: topic
                .map(|topic| NtfyTopic::parse(topic).unwrap_or_else(|_| unreachable!())),
            points_reserve: None,
        }));
        assert!(sent(&read).is_empty(), "the start is only logged");
        assert!(harness.core.notifications().outbox.is_empty());
        harness
    }

    /// A core whose storage opened, with the log read as given.
    fn opened(log: Log) -> Self {
        let mut harness = Self {
            core: Core::new(),
            start: Instant::now(),
            elapsed: Duration::from_hours(2),
            logged: Vec::new(),
        };
        let _ = harness.send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }));
        let opened = harness.send(Event::Startup(StartupEvent::StorageOpened));
        assert!(opened.contains(&Action::ReadNoticeLog));
        if let Log::Read(last) = log {
            let _ = harness.send(Event::Notification(NotificationEvent::LogRead {
                last: last.map(NoticeId::new),
            }));
        }
        harness
    }

    /// The notices saved to the log so far, oldest first.
    fn notices(&self) -> Vec<Notice> {
        self.logged
            .iter()
            .map(|(_, notice)| notice.clone())
            .collect()
    }

    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + self.elapsed
    }

    fn advance(&mut self, by: Duration) {
        self.elapsed += by;
    }

    fn send(&mut self, event: Event) -> Vec<Action> {
        let now = ClockReading {
            monotonic: self.start + self.elapsed,
            wall: self.now(),
        };
        let actions = self.core.handle(now, event);
        self.logged
            .extend(actions.iter().filter_map(|action| match action {
                Action::SaveNotice { entry, .. } => Some((entry.id, entry.notice.clone())),
                _ => None,
            }));
        actions
    }

    fn delivered(&mut self) -> Vec<Action> {
        self.send(Event::Notification(NotificationEvent::Delivered))
    }

    /// Deliver every queued notice, returning them in order.
    fn deliver_all(&mut self, first: &[Action]) -> Vec<Notice> {
        let mut notices = sent(first);
        let mut pending = !notices.is_empty();
        while pending {
            let next = sent(&self.delivered());
            pending = !next.is_empty();
            notices.extend(next);
        }
        notices
    }

    fn expire(&mut self, name: TimerName, id: TimerId) -> Vec<Action> {
        self.send(Event::TimerExpired { name, id })
    }
}

fn server(name: &str) -> VpnServer {
    VpnServer::try_new(name).unwrap_or_else(|_| unreachable!("valid server name"))
}

/// The notices an action list sends.
fn sent(actions: &[Action]) -> Vec<Notice> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::SendNotification { topic, push } => {
                assert_eq!(topic.expose_secret(), TOPIC);
                Some(push.notice.clone())
            }
            _ => None,
        })
        .collect()
}

/// The timer with this name an action list starts.
fn timer(actions: &[Action], expected: TimerName) -> Option<(TimerId, Duration)> {
    actions.iter().find_map(|action| match action {
        Action::StartTimer { name, id, deadline } if *name == expected => {
            Some((*id, deadline.after))
        }
        _ => None,
    })
}

#[test]
fn every_notice_is_logged_and_only_urgent_ones_are_pushed_with_their_entry() {
    let mut harness = Harness::new(Some(TOPIC));
    let actions = harness.send(Event::Notification(NotificationEvent::SendTest));
    let pushes: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            Action::SendNotification { push, .. } => Some(push.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        pushes,
        [Push {
            entry: Some(NoticeId::new(2)),
            notice: Notice::Test
        }],
        "the start took the first entry"
    );
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::SaveNotice { entry, forget_through: None }
            if entry.id == NoticeId::new(2) && entry.reach == Reach::Push
    )));
    assert_eq!(
        Notice::Started {
            mode: StartupMode::Resume
        }
        .reach(),
        Reach::Dashboard
    );
}

#[test]
fn ids_continue_from_the_log_and_old_notices_are_forgotten() {
    let mut harness = Harness::opened(Log::Read(Some(LOG_KEPT + 4)));
    let ready = harness.send(Event::MamPause(MamPauseEvent::Read {
        mode: Some(MamRequestMode::Paused),
    }));
    assert!(ready.iter().any(|action| matches!(
        action,
        Action::SaveNotice { entry, forget_through: Some(through) }
            if entry.id == NoticeId::new(LOG_KEPT + 5) && *through == NoticeId::new(5)
    )));
}

#[test]
fn notices_wait_for_the_log_to_be_read() {
    let mut harness = Harness::opened(Log::Unread);
    let _ = harness.send(Event::MamPause(MamPauseEvent::Read {
        mode: Some(MamRequestMode::Paused),
    }));
    let _ = harness.send(Event::Notification(NotificationEvent::SendTest));
    assert!(harness.notices().is_empty());
    let _ = harness.send(Event::Notification(NotificationEvent::LogRead {
        last: Some(NoticeId::new(9)),
    }));
    assert_eq!(
        harness.logged,
        [
            (
                NoticeId::new(10),
                Notice::Started {
                    mode: StartupMode::Resume
                }
            ),
            (NoticeId::new(11), Notice::Test)
        ]
    );
    assert_eq!(
        harness
            .core
            .notifications()
            .outbox
            .iter()
            .map(|push| push.entry)
            .collect::<Vec<_>>(),
        [Some(NoticeId::new(11))]
    );
    // A second read changes nothing.
    let again = harness.send(Event::Notification(NotificationEvent::LogRead {
        last: None,
    }));
    assert_eq!(
        harness.core.notifications().log,
        NoticeLog::Ready {
            next: NoticeId::new(12)
        }
    );
    assert!(again.is_empty());
}

#[test]
fn without_a_log_urgent_notices_are_still_pushed() {
    let mut harness = Harness::opened(Log::Unread);
    let _ = harness.send(Event::Notification(NotificationEvent::SendTest));
    let _ = harness.send(Event::Notification(NotificationEvent::LogReadFailed));
    assert_eq!(harness.core.notifications().log, NoticeLog::Unavailable);
    let again = harness.send(Event::Notification(NotificationEvent::LogReadFailed));
    assert_eq!(sent(&again).len(), 0, "nothing more to push");
    let _ = harness.send(Event::Notification(NotificationEvent::SendTest));
    let _ = harness.send(Event::Notification(NotificationEvent::SaveFailed));
    assert!(harness.notices().is_empty());
    assert_eq!(
        harness.core.notifications().outbox,
        [
            Push {
                entry: None,
                notice: Notice::Test
            },
            Push {
                entry: None,
                notice: Notice::Test
            }
        ]
    );

    // Storage that cannot open never reads the log.
    let mut failed = Harness {
        core: Core::new(),
        start: Instant::now(),
        elapsed: Duration::ZERO,
        logged: Vec::new(),
    };
    let _ = failed.send(Event::Startup(StartupEvent::Start {
        mode: StartupMode::Resume,
    }));
    let _ = failed.send(Event::Startup(StartupEvent::StorageOpenFailed));
    assert_eq!(failed.core.notifications().log, NoticeLog::Unavailable);
    assert_eq!(
        failed
            .core
            .notifications()
            .outbox
            .iter()
            .map(|push| &push.notice)
            .collect::<Vec<_>>(),
        [&Notice::StartupFailed]
    );
}

#[test]
fn without_a_topic_notices_are_discarded() {
    let mut harness = Harness::new(None);
    let actions = harness.send(Event::Notification(NotificationEvent::SendTest));
    assert!(sent(&actions).is_empty());
    assert!(harness.core.notifications().outbox.is_empty());
}

#[test]
fn failed_deliveries_are_retried_with_growing_waits() -> Result<(), &'static str> {
    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.delivered();
    let test = harness.send(Event::Notification(NotificationEvent::SendTest));
    assert_eq!(sent(&test), vec![Notice::Test]);
    let failed = Event::Notification(NotificationEvent::DeliveryFailed {
        reason: DeliveryFailure::Unreachable,
    });
    let first = harness.send(failed.clone());
    let (retry, wait) = timer(&first, TimerName::NotificationRetry).ok_or("no retry")?;
    assert_eq!(wait, Duration::from_secs(30));
    // Notices queued meanwhile wait behind the failed one.
    let _ = harness.send(Event::Notification(NotificationEvent::SendTest));
    assert!(matches!(
        harness.core.notifications().delivery,
        Delivery::Retrying { .. }
    ));
    harness.advance(wait);
    assert_eq!(
        sent(&harness.expire(TimerName::NotificationRetry, retry)),
        vec![Notice::Test]
    );
    let second = harness.send(failed);
    let (_, wait) = timer(&second, TimerName::NotificationRetry).ok_or("no retry")?;
    assert_eq!(wait, Duration::from_mins(1));
    assert_eq!(harness.core.notifications().failures, 2);
    assert_eq!(
        harness
            .core
            .notifications()
            .last_outcome
            .map(|outcome| outcome.result),
        Some(Err(DeliveryFailure::Unreachable))
    );
    assert!(
        harness
            .expire(TimerName::NotificationRetry, retry)
            .is_empty(),
        "a used retry timer is obsolete"
    );
    Ok(())
}

#[test]
fn qbittorrent_outages_notify_after_five_minutes_and_on_recovery() -> Result<(), &'static str> {
    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.delivered();
    let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::Enable));
    let unreachable = Event::Qbittorrent(QbittorrentEvent::ConnectFailed {
        reason: QbittorrentFailure::Unreachable,
    });
    let first = harness.send(unreachable);
    assert!(sent(&first).is_empty(), "within the grace period");
    let (grace, wait) = timer(&first, TimerName::NotificationGrace).ok_or("no grace timer")?;
    assert_eq!(wait, Duration::from_mins(5));
    assert!(matches!(
        harness
            .core
            .notifications()
            .conditions
            .get(&Condition::QbittorrentReachable),
        Some(Phase::Pending { .. })
    ));
    harness.advance(wait);
    assert_eq!(
        sent(&harness.expire(TimerName::NotificationGrace, grace)),
        vec![Notice::QbittorrentUnreachable {
            reason: QbittorrentFailure::Unreachable
        }]
    );
    let _ = harness.delivered();
    reconnect(&mut harness, &first)?;
    let connected = harness.send(Event::Qbittorrent(QbittorrentEvent::Connected {
        version: String::from("v5.0.0"),
        preferences: QbittorrentPreferences::default(),
    }));
    assert_eq!(
        harness.deliver_all(&connected),
        vec![Notice::QbittorrentVersionNotAllowed {
            version: String::from("v5.0.0")
        }],
        "a version MAM does not allow is reported at once"
    );
    assert!(
        harness.notices().contains(&Notice::QbittorrentReachable),
        "recovery is only logged"
    );
    Ok(())
}

/// Expire the qBittorrent retry timer an action list started, so it reconnects.
fn reconnect(harness: &mut Harness, actions: &[Action]) -> Result<(), &'static str> {
    let (retry, _) = timer(actions, TimerName::QbittorrentPoll).ok_or("no retry timer")?;
    assert_eq!(
        harness.expire(TimerName::QbittorrentPoll, retry),
        vec![Action::ConnectQbittorrent]
    );
    Ok(())
}

#[test]
fn short_outages_stay_quiet() -> Result<(), &'static str> {
    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.delivered();
    let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::Enable));
    let failed = harness.send(Event::Qbittorrent(QbittorrentEvent::ConnectFailed {
        reason: QbittorrentFailure::Rejected { status: 502 },
    }));
    harness.advance(Duration::from_mins(1));
    reconnect(&mut harness, &failed)?;
    let actions = harness.send(Event::Qbittorrent(QbittorrentEvent::Connected {
        version: String::from("v5.2.2"),
        preferences: QbittorrentPreferences::default(),
    }));
    assert!(sent(&actions).is_empty());
    assert!(harness.core.notifications().conditions.is_empty());
    assert_eq!(
        windlass::core::ClientVersion::parse("v5.2.2")
            .map(windlass::core::ClientVersion::mam_approval),
        Some(MamApproval::Recommended)
    );
    Ok(())
}

/// Fail tunnel checks until a switch is considered, delivering what is sent.
fn fail_checks(harness: &mut Harness) -> Vec<Notice> {
    let mut notices = Vec::new();
    for _ in 0..3 {
        let actions = harness.send(Event::Tunnel(TunnelEvent::ExitIpCheckFailed {
            reason: ExitIpFailure::Unreachable,
        }));
        notices.extend(harness.deliver_all(&actions));
        if let Some((id, _)) = timer(&actions, TimerName::TunnelCheck) {
            let _ = harness.expire(TimerName::TunnelCheck, id);
        }
    }
    notices
}

fn enable_tunnel(harness: &mut Harness, last_switch: Option<VpnSwitch>) {
    let _ = harness.send(Event::Tunnel(TunnelEvent::LastSwitchRead { last_switch }));
    let _ = harness.send(Event::Tunnel(TunnelEvent::Enable {
        servers: vec![server("one"), server("two")],
        current: Some(server("one")),
    }));
}

#[test]
fn failover_is_announced_step_by_step() {
    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.delivered();
    enable_tunnel(&mut harness, None);
    assert_eq!(
        fail_checks(&mut harness),
        vec![Notice::VpnProtectionLost { failures: 3 }]
    );
    let _ = harness.send(Event::Tunnel(TunnelEvent::SwitchSaved));
    let switched = harness.send(Event::Tunnel(TunnelEvent::Switched));
    assert!(harness.deliver_all(&switched).is_empty());
    let observed = harness.send(Event::Tunnel(TunnelEvent::ExitIpObserved {
        ip: std::net::IpAddr::from([192, 0, 2, 9]),
    }));
    assert!(harness.deliver_all(&observed).is_empty());
    // The steps of the switch, and the recovery, are only logged.
    assert_eq!(
        harness.notices().get(1..),
        Some(
            [
                Notice::VpnProtectionLost { failures: 3 },
                Notice::FailoverStarted {
                    server: server("two")
                },
                Notice::FailoverSucceeded {
                    server: server("two")
                },
                Notice::VpnProtectionRestored {
                    exit_ip: std::net::IpAddr::from([192, 0, 2, 9]),
                    server: Some(server("two"))
                }
            ]
            .as_slice()
        )
    );
}

#[test]
fn failed_and_blocked_switches_are_announced() {
    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.delivered();
    enable_tunnel(&mut harness, None);
    let _ = fail_checks(&mut harness);
    let failed = harness.send(Event::Tunnel(TunnelEvent::SwitchSaveFailed));
    assert_eq!(
        harness.deliver_all(&failed),
        vec![Notice::FailoverFailed {
            server: server("two")
        }]
    );

    let mut blocked = Harness::new(Some(TOPIC));
    let _ = blocked.delivered();
    let last = VpnSwitch {
        server: server("one"),
        at: blocked.now() - Duration::from_mins(10),
    };
    let until = last.at + Duration::from_hours(1);
    enable_tunnel(&mut blocked, Some(last));
    assert_eq!(
        fail_checks(&mut blocked),
        vec![
            Notice::VpnProtectionLost { failures: 3 },
            Notice::FailoverBlocked {
                reason: SwitchBlock::Until(until)
            }
        ]
    );
}

#[test]
fn settings_and_history_failures_are_announced() {
    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.delivered();
    let _ = harness.send(Event::Settings(SettingsEvent::SetHistoryLimit {
        limit: windlass::core::HistoryLimit::try_new(500).unwrap_or_default(),
    }));
    let failed = harness.send(Event::Settings(SettingsEvent::HistoryLimitWriteFailed));
    assert_eq!(
        sent(&failed),
        vec![Notice::SettingNotSaved {
            setting: SettingName::HistoryLimit
        }]
    );
}

#[test]
fn a_full_outbox_drops_the_oldest_notices() {
    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.send(Event::Notification(NotificationEvent::SendTest));
    let _ = harness.send(Event::Notification(NotificationEvent::DeliveryFailed {
        reason: DeliveryFailure::Rejected { status: 429 },
    }));
    for _ in 0..=OUTBOX_LIMIT {
        let _ = harness.send(Event::Notification(NotificationEvent::SendTest));
    }
    assert_eq!(harness.core.notifications().outbox.len(), OUTBOX_LIMIT);
    assert_eq!(harness.core.notifications().dropped, 1);
}

#[test]
fn topics_are_validated_and_never_shown() {
    assert!(NtfyTopic::parse("windlass_alerts-42").is_ok());
    for invalid in ["", "two words", "a/b", &"a".repeat(65)] {
        assert!(NtfyTopic::parse(invalid).is_err(), "{invalid}");
    }
    let topic = NtfyTopic::parse(TOPIC).unwrap_or_else(|_| unreachable!());
    assert_eq!(format!("{topic:?}"), "NtfyTopic([redacted])");
}

#[test]
fn torrent_problems_are_reported_after_their_grace_periods() -> Result<(), &'static str> {
    use windlass::core::{
        Registry, RegistryEvent, SyncPosition, SyncUpdate, TorrentHash, TorrentState, TorrentUpdate,
    };

    let mut harness = Harness::new(Some(TOPIC));
    let _ = harness.delivered();
    let _ = harness.send(Event::Registry(RegistryEvent::Read(Vec::new())));
    assert!(matches!(harness.core.registry(), Registry::Loaded(_)));
    let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::Enable));
    let _ = harness.send(Event::Qbittorrent(QbittorrentEvent::Connected {
        version: String::from("v5.2.2"),
        preferences: QbittorrentPreferences::default(),
    }));
    let torrent = |fill: char, state| {
        TorrentHash::parse(&fill.to_string().repeat(40)).map(|hash| {
            (
                hash,
                TorrentUpdate {
                    state: Some(state),
                    ..TorrentUpdate::default()
                },
            )
        })
    };
    let torrents = [
        torrent('a', TorrentState::Error),
        torrent('b', TorrentState::MissingFiles),
    ]
    .into_iter()
    .collect::<Option<Vec<_>>>()
    .ok_or("invalid hash")?;
    let synced = harness.send(Event::Qbittorrent(QbittorrentEvent::Synced(SyncUpdate {
        position: SyncPosition(1),
        full: true,
        torrents,
        removed: Vec::new(),
    })));
    assert!(sent(&synced).is_empty());
    let (grace, wait) = timer(&synced, TimerName::NotificationGrace).ok_or("no grace timer")?;
    assert_eq!(
        wait,
        Duration::from_mins(5),
        "the earliest grace period ends first"
    );
    harness.advance(wait);
    let errors = harness.expire(TimerName::NotificationGrace, grace);
    assert_eq!(sent(&errors), vec![Notice::TorrentErrors { count: 1 }]);
    let (grace, wait) = timer(&errors, TimerName::NotificationGrace).ok_or("no grace timer")?;
    assert_eq!(wait, Duration::from_mins(5));
    let _ = harness.delivered();
    harness.advance(wait);
    let missing = harness.expire(TimerName::NotificationGrace, grace);
    assert_eq!(sent(&missing), vec![Notice::FilesMissing { count: 1 }]);
    Ok(())
}

#[test]
fn notices_wait_while_one_is_being_sent() {
    let mut harness = Harness::new(Some(TOPIC));
    let first = harness.send(Event::Notification(NotificationEvent::SendTest));
    assert_eq!(sent(&first), vec![Notice::Test]);
    let second = harness.send(Event::Notification(NotificationEvent::SendTest));
    assert!(
        sent(&second).is_empty(),
        "the first test is still being sent"
    );
    let _ = harness.send(Event::Settings(SettingsEvent::SetNtfyTopic {
        topic: NtfyTopic::parse("other-topic").unwrap_or_else(|_| unreachable!()),
    }));
    let failed = harness.send(Event::Settings(SettingsEvent::NtfyTopicWriteFailed));
    assert!(sent(&failed).is_empty());
    assert_eq!(
        harness
            .core
            .notifications()
            .outbox
            .back()
            .map(|push| &push.notice),
        Some(&Notice::SettingNotSaved {
            setting: SettingName::NtfyTopic
        })
    );
}
