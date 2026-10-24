//! Scenario tests for notifications: grace periods, one-off notices, delivery,
//! and retries, with supplied time.

use std::time::{Duration, Instant, SystemTime};

use windlass::core::{
    Action, ClockReading, Condition, Core, Delivery, DeliveryFailure, Event, ExitIpFailure,
    MamApproval, MamPauseEvent, MamRequestMode, Notice, NotificationEvent, NtfyTopic, OUTBOX_LIMIT,
    Phase, QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences, SettingName,
    SettingsEvent, StartupEvent, StartupMode, SwitchBlock, TimerId, TimerName, TunnelEvent,
    VpnServer, VpnSwitch,
};

const TOPIC: &str = "windlass-alerts";

/// A core with a clock that only moves when told to.
struct Harness {
    core: Core,
    start: Instant,
    elapsed: Duration,
}

impl Harness {
    /// A started core whose settings name `topic`, with the startup notice
    /// already delivered.
    fn new(topic: Option<&str>) -> Self {
        let mut harness = Self {
            core: Core::new(),
            start: Instant::now(),
            elapsed: Duration::from_hours(2),
        };
        let _ = harness.send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }));
        let _ = harness.send(Event::Startup(StartupEvent::StorageOpened));
        let ready = harness.send(Event::MamPause(MamPauseEvent::Read {
            mode: Some(MamRequestMode::Paused),
        }));
        assert!(sent(&ready).is_empty(), "settings are not read yet");
        assert_eq!(
            harness.core.notifications().outbox.front(),
            Some(&Notice::Started {
                mode: StartupMode::Resume
            })
        );
        let read = harness.send(Event::Settings(SettingsEvent::Read {
            history_limit: None,
            api_cookie: None,
            ntfy_topic: topic
                .map(|topic| NtfyTopic::parse(topic).unwrap_or_else(|_| unreachable!())),
        }));
        if topic.is_some() {
            assert_eq!(
                sent(&read),
                vec![Notice::Started {
                    mode: StartupMode::Resume
                }]
            );
            assert!(harness.delivered().is_empty());
        } else {
            assert!(sent(&read).is_empty());
            assert!(harness.core.notifications().outbox.is_empty());
        }
        harness
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
        self.core.handle(now, event)
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
            Action::SendNotification { topic, notice } => {
                assert_eq!(topic.expose_secret(), TOPIC);
                Some(notice.clone())
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
fn startup_is_announced_once_settings_name_a_topic() {
    let mut harness = Harness::new(Some(TOPIC));
    assert!(harness.delivered().is_empty(), "nothing else is queued");
    assert!(harness.core.notifications().last_outcome.is_some());
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
        vec![
            Notice::QbittorrentReachable,
            Notice::QbittorrentVersionNotAllowed {
                version: String::from("v5.0.0")
            }
        ],
        "a version MAM does not allow is reported at once"
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
        vec![
            Notice::VpnProtectionLost { failures: 3 },
            Notice::FailoverStarted {
                server: server("two")
            }
        ]
    );
    let _ = harness.send(Event::Tunnel(TunnelEvent::SwitchSaved));
    let switched = harness.send(Event::Tunnel(TunnelEvent::Switched));
    assert_eq!(
        harness.deliver_all(&switched),
        vec![Notice::FailoverSucceeded {
            server: server("two")
        }]
    );
    let observed = harness.send(Event::Tunnel(TunnelEvent::ExitIpObserved {
        ip: std::net::IpAddr::from([192, 0, 2, 9]),
    }));
    assert_eq!(
        harness.deliver_all(&observed),
        vec![Notice::VpnProtectionRestored {
            exit_ip: std::net::IpAddr::from([192, 0, 2, 9]),
            server: Some(server("two"))
        }]
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
        harness.core.notifications().outbox.back(),
        Some(&Notice::SettingNotSaved {
            setting: SettingName::NtfyTopic
        })
    );
}
