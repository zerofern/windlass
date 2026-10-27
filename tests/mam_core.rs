//! Scenario tests for Windlass's work with MAM: the request gate, seedbox
//! registration, the account and spending, bookmark acquisition, and the
//! weekly reminder. Time only moves when a test says so.

use std::{
    net::IpAddr,
    time::{Duration, Instant, SystemTime},
};

use windlass::core::{
    Action, ApiCookie, Asn, Bookmark, BookmarkPage, ByteSize, ClientStanding, ClientVersion,
    ClockReading, Connectability, Connectable, Core, Desired, DownloadWatch, Event, FetchedTorrent,
    Findings, Hold, IpFinding, IpObservation, LEASE_LIFETIME, ListedClient, MamEvent, MamFailure,
    MamNotices, MamPauseEvent, MamReply, MamRequest, MamRequestMode, MamSession, MamTorrentId,
    Notice, NotificationEvent, PersonalFreeleech, Points, Port, PortFinding, PortForwardingEvent,
    Purchase, QbittorrentEvent, QbittorrentPreferences, Registration, Registry, RegistryEvent,
    SeedboxStatus, SessionRejection, SettingsEvent, SnatchSummary, StartupEvent, StartupMode,
    SyncPosition, SyncUpdate, TimerId, TimerName, TorrentCommand, TorrentHash, TorrentRecord,
    TunnelEvent, UserData, WedgeSpent,
};
use windlass::core::{
    BookTitle, BookmarkCount, ClientRefusal, Freeleech, Partials, Snatched, TorrentCount,
    UnsatisfiedCount, UnsatisfiedLimit, UserClass, VipTorrent, WedgeCount,
};

const GIB: u64 = 1 << 30;
const DAY: Duration = Duration::from_hours(24);
/// A wall clock in late 2026, so VIP expiries look real.
const EPOCH: Duration = Duration::from_secs(1_790_000_000);

fn ip(last: u8) -> IpAddr {
    IpAddr::from([192, 0, 2, last])
}

fn torrent_id(id: u64) -> MamTorrentId {
    MamTorrentId::new(id).unwrap_or_else(|| unreachable!("non-zero"))
}

fn hash(fill: char) -> TorrentHash {
    TorrentHash::parse(&fill.to_string().repeat(40)).unwrap_or_else(|| unreachable!("valid hash"))
}

fn cookie(value: &str) -> ApiCookie {
    ApiCookie::parse(value).unwrap_or_else(|_| unreachable!("valid cookie"))
}

/// A core with a clock that only moves when told to.
struct Harness {
    core: Core,
    start: Instant,
    elapsed: Duration,
    position: u64,
    /// Every notice the core saved to its log, oldest first.
    logged: Vec<Notice>,
}

/// What the harness starts from.
struct Setup {
    cookie: Option<&'static str>,
    last_registration: Option<Duration>,
    /// The registry's rows; `None` when it cannot be read.
    rows: Option<Vec<TorrentRecord>>,
    client: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            cookie: Some("first-cookie"),
            last_registration: None,
            rows: Some(Vec::new()),
            client: true,
        }
    }
}

impl Harness {
    /// A started core with MAM requests running, VPN protection, a connected
    /// qBittorrent, and MAM enabled. Returns the actions of the last step.
    fn start(setup: Setup) -> (Self, Vec<Action>) {
        let mut harness = Self {
            core: Core::new(),
            start: Instant::now(),
            elapsed: Duration::ZERO,
            position: 0,
            logged: Vec::new(),
        };
        let now = harness.wall();
        for event in [
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Resume,
            }),
            Event::Startup(StartupEvent::StorageOpened),
            Event::Notification(NotificationEvent::LogRead { last: None }),
            Event::MamPause(MamPauseEvent::Read {
                mode: Some(MamRequestMode::Enabled),
            }),
            Event::Settings(SettingsEvent::Read {
                history_limit: None,
                api_cookie: setup.cookie.map(cookie),
                ntfy_topic: windlass::core::NtfyTopic::parse("test-topic").ok(),
                points_reserve: None,
                torrent_limits: None,
            }),
            Event::Registry(
                setup
                    .rows
                    .map_or(RegistryEvent::ReadFailed, RegistryEvent::Read),
            ),
            Event::Tunnel(TunnelEvent::LastSwitchRead { last_switch: None }),
            Event::Mam(MamEvent::StateRead {
                last_registration: setup.last_registration.and_then(|ago| now.checked_sub(ago)),
                last_reminder: Some(now),
            }),
            Event::Tunnel(TunnelEvent::Enable {
                servers: Vec::new(),
                current: None,
            }),
            Event::Tunnel(TunnelEvent::ExitIpObserved { ip: ip(10) }),
        ] {
            let _ = harness.send(event);
        }
        if setup.client {
            for event in [
                Event::Qbittorrent(QbittorrentEvent::Enable),
                Event::Qbittorrent(QbittorrentEvent::Connected {
                    version: windlass::core::ReportedVersion::Release(
                        windlass::core::ClientVersion::new(5, 2, 2),
                    ),
                    preferences: QbittorrentPreferences::default(),
                }),
            ] {
                let _ = harness.send(event);
            }
            let _ = harness.sync(Vec::new());
        }
        let actions = harness.send(Event::Mam(MamEvent::Enable));
        // The first IP lookup waits a minute after a start, since a previous
        // run may have just made one; let that minute pass.
        let actions = if timer(&actions, TimerName::Mam)
            .is_some_and(|(_, after)| after <= windlass::core::LOOKUP_SPACING)
        {
            harness.wake(&actions)
        } else {
            actions
        };
        (harness, actions)
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + EPOCH + self.elapsed
    }

    fn advance(&mut self, by: Duration) {
        self.elapsed += by;
    }

    fn send(&mut self, event: Event) -> Vec<Action> {
        let now = ClockReading {
            monotonic: self.start + self.elapsed,
            wall: self.wall(),
        };
        let mut actions = self.core.handle(now, event);
        self.logged
            .extend(actions.iter().filter_map(|action| match action {
                Action::SaveNotice { entry, .. } => Some(entry.notice.clone()),
                _ => None,
            }));
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

    fn answer(&mut self, result: Result<MamReply, MamFailure>) -> Vec<Action> {
        self.send(Event::Mam(MamEvent::Answered {
            result,
            rotated: None,
        }))
    }

    /// Deliver the next qBittorrent update; the first is the full list.
    fn sync(&mut self, torrents: Vec<TorrentHash>) -> Vec<Action> {
        self.position += 1;
        self.send(Event::Qbittorrent(QbittorrentEvent::Synced(SyncUpdate {
            position: SyncPosition(self.position),
            scope: if self.position == 1 {
                windlass::core::SyncScope::Full
            } else {
                windlass::core::SyncScope::Changes
            },
            torrents: torrents
                .into_iter()
                .map(|hash| (hash, windlass::core::TorrentUpdate::default()))
                .collect(),
            removed: Vec::new(),
        })))
    }

    /// Wait out the MAM timer started by these actions, and let it expire.
    fn wake(&mut self, actions: &[Action]) -> Vec<Action> {
        let Some((id, after)) = timer(actions, TimerName::Mam) else {
            unreachable!("no MAM timer in {actions:?}");
        };
        self.advance(after);
        self.send(Event::TimerExpired {
            name: TimerName::Mam,
            id,
        })
    }

    /// Observe the IP, then register: the attempt is saved before it is sent.
    fn register(&mut self, actions: &[Action]) -> Vec<Action> {
        assert_eq!(requests(actions), [MamRequest::LookUpIp]);
        let actions = self.answer(Ok(MamReply::Ip(observation(10, 209_103))));
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. })),
            "{actions:?}"
        );
        let actions = self.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
        assert_eq!(requests(&actions), [MamRequest::RegisterSeedbox]);
        self.answer(Ok(MamReply::Registered {
            registration: Registration::Completed,
            ip: ip(10),
        }))
    }

    /// Answer routine IP lookups with the same address until something else
    /// is asked.
    fn past_lookups(&mut self, mut actions: Vec<Action>) -> Vec<Action> {
        while requests(&actions) == [MamRequest::LookUpIp] {
            actions = self.answer(Ok(MamReply::Ip(observation(10, 209_103))));
        }
        actions
    }

    /// Let time pass and let the core decide again.
    fn later(&mut self, by: Duration) -> Vec<Action> {
        self.advance(by);
        let actions = self.send(Event::Mam(MamEvent::ReminderSaved));
        self.past_lookups(actions)
    }

    /// Expire qBittorrent's poll timer and deliver the update it asks for.
    fn poll(&mut self, torrents: Vec<TorrentHash>) -> Vec<Action> {
        if let windlass::core::Qbittorrent::Connected(session) = self.core.qbittorrent()
            && let windlass::core::Activity::Waiting(id) = session.activity
        {
            let _ = self.send(Event::TimerExpired {
                name: TimerName::QbittorrentPoll,
                id,
            });
        }
        self.sync(torrents)
    }

    /// Answer bookmark listings and lookups with these listings, and the bonus
    /// history with no wedges, until something else is asked.
    fn checked(&mut self, actions: Vec<Action>, listings: &[Bookmark]) -> Vec<Action> {
        self.checked_with(actions, listings, &[])
    }

    /// Answer bookmark listings and lookups with these listings, and the bonus
    /// history with these wedges, until something else is asked.
    fn checked_with(
        &mut self,
        mut actions: Vec<Action>,
        listings: &[Bookmark],
        spent: &[WedgeSpent],
    ) -> Vec<Action> {
        loop {
            actions = match requests(&actions).as_slice() {
                [MamRequest::LookUpTorrent { id }] => {
                    let listing = listings.iter().find(|listing| listing.id == *id).cloned();
                    self.answer(Ok(MamReply::Torrent(listing.map(Box::new))))
                }
                [MamRequest::WedgeHistory] => {
                    self.answer(Ok(MamReply::WedgeHistory(spent.to_vec())))
                }
                // A due listing comes before single lookups; it lists these.
                [MamRequest::ListBookmarks { start: 0 }] => self.answer(listed(listings.to_vec())),
                // The bucket ran out; the next token comes soon.
                [] if timer(&actions, TimerName::Mam)
                    .is_some_and(|(_, after)| after <= windlass::core::REQUEST_REFILL) =>
                {
                    self.wake(&actions)
                }
                _ => return actions,
            };
        }
    }

    /// Answer readings with a healthy account, bookmark listings with none,
    /// lookups with this listing, and the bonus history with no wedges, until
    /// something else is asked.
    fn respond(&mut self, mut actions: Vec<Action>, listing: &Bookmark) -> Vec<Action> {
        loop {
            actions = match requests(&actions).as_slice() {
                [MamRequest::LoadUserData { .. }] => self.answer(read(account(self.wall()))),
                [MamRequest::ListBookmarks { .. }] => self.answer(listed(Vec::new())),
                [MamRequest::LookUpTorrent { .. }] => {
                    self.answer(Ok(MamReply::Torrent(Some(Box::new(listing.clone())))))
                }
                [MamRequest::WedgeHistory] => self.answer(Ok(MamReply::WedgeHistory(Vec::new()))),
                _ => return actions,
            };
        }
    }

    const fn mam(&self) -> &windlass::core::Mam {
        self.core.mam()
    }

    /// Every notice given so far.
    fn notices(&self) -> Vec<Notice> {
        self.logged.clone()
    }

    fn record(&self, id: u64) -> Option<TorrentRecord> {
        match self.core.registry() {
            Registry::Loaded(records) => records.find_mam(torrent_id(id)).cloned(),
            _ => None,
        }
    }
}

fn observation(last: u8, asn: u32) -> IpObservation {
    IpObservation {
        ip: ip(last),
        asn: Asn(asn),
        provider: windlass::core::NetworkName::try_new("ProtonVPN Proton AG").ok(),
    }
}

/// The requests in an action list.
fn requests(actions: &[Action]) -> Vec<MamRequest> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::MamRequest { request, .. } => Some(request.clone()),
            _ => None,
        })
        .collect()
}

fn cookie_sent(actions: &[Action]) -> Option<ApiCookie> {
    actions.iter().find_map(|action| match action {
        Action::MamRequest { cookie, .. } => cookie.clone(),
        _ => None,
    })
}

fn timer(actions: &[Action], name: TimerName) -> Option<(TimerId, Duration)> {
    actions.iter().find_map(|action| match action {
        Action::StartTimer {
            name: started,
            id,
            deadline,
        } if *started == name => Some((*id, deadline.after)),
        _ => None,
    })
}

/// A healthy VIP account.
fn account(now: SystemTime) -> UserData {
    UserData {
        uploaded: ByteSize(80 * GIB),
        downloaded: ByteSize(0),
        points: Points(60_000),
        wedges: WedgeCount::new(200),
        class: UserClass::Vip,
        vip_until: now.checked_add(60 * DAY),
        summary: Some(summary()),
        notices: Some(MamNotices::default()),
        clients: Some(vec![listed_client("5.2.2")]),
    }
}

/// A snatch summary with nothing flagged, for a VIP account's limit of 150.
const fn summary() -> SnatchSummary {
    SnatchSummary {
        unsatisfied: UnsatisfiedCount::new(0),
        unsatisfied_limit: UnsatisfiedLimit::new(150),
        unsatisfied_not_seeding: TorrentCount::new(0),
        hit_and_run_not_seeding: TorrentCount::new(0),
        partial: Partials::NotSeen,
        duplicates: TorrentCount::new(0),
        tracker_errors: TorrentCount::new(0),
        connectable: Connectable::Yes,
        red: Vec::new(),
    }
}

/// An entry in MAM's client list naming this qBittorrent version.
fn listed_client(version: &str) -> ListedClient {
    ListedClient {
        qbittorrent: ClientVersion::parse(version),
        connectable: Some(Connectable::Yes),
        refusal: ClientRefusal::NotRefused,
    }
}

/// MAM's answer with this account; answers are results, as the shell sends them.
#[expect(clippy::unnecessary_wraps, reason = "the harness answers with results")]
fn read(data: UserData) -> Result<MamReply, MamFailure> {
    Ok(MamReply::UserData(Box::new(data)))
}

fn bookmark(id: u64) -> Bookmark {
    Bookmark {
        id: torrent_id(id),
        title: BookTitle::try_new(format!("Synthetic {id}"))
            .unwrap_or_else(|_| unreachable!("a title")),
        vip: VipTorrent::No,
        freeleech: Freeleech::No,
        personal: PersonalFreeleech::No,
        snatched: Snatched::Never,
        bookmarked_at: None,
    }
}

/// MAM's answer listing these bookmarks on one page.
#[expect(clippy::unnecessary_wraps, reason = "the harness answers with results")]
fn listed(bookmarks: Vec<Bookmark>) -> Result<MamReply, MamFailure> {
    Ok(MamReply::Bookmarks(BookmarkPage {
        found: BookmarkCount::new(u32::try_from(bookmarks.len()).unwrap_or(u32::MAX)),
        bookmarks,
    }))
}

#[test]
fn nothing_is_sent_until_mam_is_enabled_unpaused_and_protected() {
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    assert_eq!(cookie_sent(&actions), None, "the IP lookup sends no cookie");
    assert!(harness.mam().available);

    // Pausing stops requests and unconfirms what was known.
    let actions = harness.send(Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    }));
    assert!(requests(&actions).is_empty());
    assert!(!harness.mam().available);
    // The lookup sent before the pause answers late and is not applied.
    let _ = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert!(harness.mam().network.observed.is_none());

    // Lost protection keeps requests stopped after resuming.
    let _ = harness.send(Event::MamPause(MamPauseEvent::Written));
    let _ = harness.send(Event::Tunnel(TunnelEvent::Enable {
        servers: Vec::new(),
        current: None,
    }));
    let actions = harness.send(Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Enabled,
    }));
    assert!(requests(&actions).is_empty());
    // The lookup sent before the pause still counts against MAM's minute.
    let actions = harness.send(Event::MamPause(MamPauseEvent::Written));
    assert!(requests(&actions).is_empty(), "{actions:?}");
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
}

#[test]
fn without_a_cookie_only_the_ip_is_observed() {
    let (mut harness, actions) = Harness::start(Setup {
        cookie: None,
        ..Setup::default()
    });
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert!(requests(&actions).is_empty());
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. }))
    );
    // The next lookup is five minutes later.
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
}

#[test]
fn registration_is_saved_before_it_is_sent_and_spaced_an_hour_apart() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(
        harness.mam().seedbox.status,
        SeedboxStatus::Confirmed {
            ip: ip(10),
            at: harness.wall()
        }
    );
    assert!(matches!(harness.mam().session, MamSession::Verified { .. }));
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    assert_eq!(cookie_sent(&actions), Some(cookie("first-cookie")));
    let _ = harness.answer(read(account(harness.wall())));

    let _ = harness.answer(listed(Vec::new()));

    // Five minutes later MAM sees another IP; it is registered only once the
    // hour has passed.
    let actions = harness.later(Duration::from_mins(5));
    assert!(requests(&actions).is_empty());
    harness.advance(Duration::from_mins(5));
    let actions = harness.send(Event::Mam(MamEvent::ReminderSaved));
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(11, 209_103))));
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. }))
    );
    assert!(
        !harness
            .mam()
            .seedbox
            .confirms(harness.mam().network.current())
    );
    harness.advance(Duration::from_mins(50));
    let mut actions = harness.send(Event::Mam(MamEvent::ReminderSaved));
    while requests(&actions) == [MamRequest::LookUpIp] {
        actions = harness.answer(Ok(MamReply::Ip(observation(11, 209_103))));
    }
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. })),
        "{actions:?}"
    );
}

#[test]
fn a_saved_attempt_from_before_a_restart_still_spaces_the_next() {
    let (mut harness, actions) = Harness::start(Setup {
        last_registration: Some(Duration::from_mins(20)),
        ..Setup::default()
    });
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. }))
    );
    // The account is read meanwhile; registration waits 40 more minutes.
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let actions = harness.answer(read(account(harness.wall())));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(Vec::new()));
    let actions = harness.later(Duration::from_mins(40));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. })),
        "{actions:?}"
    );
    // A failed save sends nothing and is reported.
    let _ = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaveFailed));
    assert!(matches!(
        harness.mam().seedbox.status,
        SeedboxStatus::Failed { .. }
    ));
    assert!(harness.notices().contains(&Notice::SeedboxNotRegistered));
}

#[test]
fn unreadable_state_blocks_registration() {
    let mut harness = Harness {
        core: Core::new(),
        start: Instant::now(),
        elapsed: Duration::ZERO,
        position: 0,
        logged: Vec::new(),
    };
    for event in [
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
        Event::Startup(StartupEvent::StorageOpened),
        Event::Notification(NotificationEvent::LogRead { last: None }),
        Event::Mam(MamEvent::StateReadFailed),
    ] {
        let _ = harness.send(event);
    }
    assert_eq!(
        harness.mam().seedbox.last_attempt,
        windlass::core::LastAttempt::ReadFailed
    );
    assert!(harness.notices().contains(&Notice::MamStateUnreadable));
    assert_eq!(
        harness.mam().seedbox.allowance(harness.wall()),
        windlass::core::RegistrationAllowance::Unknown
    );
}

#[test]
fn a_refused_session_stops_cookie_requests_until_the_cookie_changes() {
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let _ = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    let actions = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
    assert_eq!(requests(&actions), [MamRequest::RegisterSeedbox]);
    let actions = harness.answer(Err(MamFailure::Session(SessionRejection::InvalidCookie)));
    assert!(requests(&actions).is_empty());
    assert!(matches!(
        harness.mam().session,
        MamSession::Rejected {
            rejection: SessionRejection::InvalidCookie,
            ..
        }
    ));
    assert!(harness.notices().contains(&Notice::MamSessionRejected {
        rejection: SessionRejection::InvalidCookie
    }));

    // A replacement is saved before it is used, and starts a new session.
    let actions = harness.send(Event::Settings(SettingsEvent::SetApiCookie {
        cookie: cookie("second-cookie"),
    }));
    assert!(requests(&actions).is_empty());
    let actions = harness.send(Event::Settings(SettingsEvent::ApiCookieWritten));
    assert_eq!(harness.mam().session, MamSession::Unverified);
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    assert_eq!(cookie_sent(&actions), Some(cookie("second-cookie")));
}

#[test]
fn an_asn_mismatch_lifts_when_mam_sees_another_asn() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let actions = harness.answer(Err(MamFailure::Session(SessionRejection::AsnMismatch)));
    assert!(requests(&actions).is_empty());
    // The same ASN keeps the session refused; another lets it be tried again.
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert!(requests(&actions).is_empty());
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(12, 64_500))));
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
}

#[test]
fn a_session_that_may_not_register_still_reads_the_account() {
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let _ = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    let _ = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
    let actions = harness.answer(Err(MamFailure::Session(SessionRejection::NotAllowed)));
    assert_eq!(harness.mam().session, MamSession::Unverified);
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    assert!(harness.notices().contains(&Notice::SeedboxNotRegistered));
}

#[test]
fn mam_asking_to_wait_delays_the_retry() {
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let start = harness.start + harness.elapsed;
    let actions = harness.answer(Err(MamFailure::TooSoon {
        retry_after: Some(Duration::from_mins(20)),
    }));
    assert_eq!(
        harness.mam().network.next_lookup,
        Some(start + Duration::from_mins(20))
    );
    assert_eq!(harness.mam().network.failures, 1);
    // Nothing that needs the session is sent before MAM shows which IP it sees.
    assert!(requests(&actions).is_empty(), "{actions:?}");
    harness.advance(Duration::from_mins(20));
    let actions = harness.send(Event::Mam(MamEvent::ReminderSaved));
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let _ = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    let actions = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
    assert_eq!(requests(&actions), [MamRequest::RegisterSeedbox]);
    let _ = harness.answer(Err(MamFailure::TooSoon {
        retry_after: Some(Duration::from_hours(3)),
    }));
    assert_eq!(
        harness.mam().seedbox.allowance(harness.wall()),
        windlass::core::RegistrationAllowance::After(harness.wall() + Duration::from_hours(3))
    );
    // Waiting is not a failure the owner needs to hear about.
    assert!(!harness.notices().contains(&Notice::SeedboxNotRegistered));
}

#[test]
fn a_cookie_mam_issues_is_saved_and_continues_the_session() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let actions = harness.send(Event::Mam(MamEvent::Answered {
        result: read(account(harness.wall())),
        rotated: Some(cookie("rotated-cookie")),
    }));
    assert!(actions.contains(&Action::WriteRotatedApiCookie {
        cookie: cookie("rotated-cookie")
    }));
    let actions = harness.send(Event::Settings(SettingsEvent::RotatedApiCookieWritten));
    assert!(matches!(harness.mam().session, MamSession::Verified { .. }));
    assert!(matches!(
        harness.mam().seedbox.status,
        SeedboxStatus::Confirmed { .. }
    ));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    assert_eq!(cookie_sent(&actions), Some(cookie("rotated-cookie")));

    // A rotation that cannot be saved leaves the saved cookie in use.
    let actions = harness.send(Event::Mam(MamEvent::Answered {
        result: listed(Vec::new()),
        rotated: Some(cookie("another-cookie")),
    }));
    assert!(actions.contains(&Action::WriteRotatedApiCookie {
        cookie: cookie("another-cookie")
    }));
    let _ = harness.send(Event::Settings(SettingsEvent::RotatedApiCookieWriteFailed));
    let settings = harness.core.settings().loaded().cloned();
    assert_eq!(
        settings.and_then(|settings| settings.usable_api_cookie().cloned()),
        Some(cookie("rotated-cookie"))
    );
    assert!(matches!(harness.mam().session, MamSession::Verified { .. }));
}

#[test]
fn the_bucket_allows_ten_requests_then_one_every_three_seconds() {
    const FOUND: u32 = 750;
    let page = |start: u32| BookmarkPage {
        found: BookmarkCount::new(FOUND),
        bookmarks: (u64::from(start) + 1..=u64::from((start + 100).min(FOUND)))
            .map(bookmark)
            .collect(),
    };
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    // The IP lookup, registration, and account read; bookmarks are the fourth.
    assert_eq!(requests(&actions).len(), 1);
    let actions = harness.answer(read(account(harness.wall())));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    // Pages two to seven use the rest of the bucket.
    for start in (0..600).step_by(100) {
        let actions = harness.answer(Ok(MamReply::Bookmarks(page(start))));
        assert_eq!(
            requests(&actions),
            [MamRequest::ListBookmarks { start: start + 100 }]
        );
    }
    let actions = harness.answer(Ok(MamReply::Bookmarks(page(600))));
    assert!(requests(&actions).is_empty(), "the bucket is empty");
    assert_eq!(
        timer(&actions, TimerName::Mam).map(|(_, after)| after),
        Some(Duration::from_secs(3))
    );
    let actions = harness.wake(&actions);
    assert_eq!(
        requests(&actions),
        [MamRequest::ListBookmarks { start: 700 }]
    );
    let _ = harness.answer(Ok(MamReply::Bookmarks(page(700))));
    assert_eq!(
        harness
            .mam()
            .acquisition
            .bookmarks
            .latest
            .as_ref()
            .map(|listed| listed.bookmarks.len()),
        Some(750)
    );
}

/// Register, read the account, and list these bookmarks; the actions of the
/// last step.
fn listed_with(harness: &mut Harness, account: UserData, bookmarks: Vec<Bookmark>) -> Vec<Action> {
    let (started, actions) = Harness::start(Setup::default());
    *harness = started;
    let actions = harness.register(&actions);
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let actions = harness.answer(read(account));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let (page, listings) = (listed(bookmarks.clone()), bookmarks);
    let actions = harness.answer(page);
    harness.checked(actions, &listings)
}

/// List these bookmarks, then answer each lookup with what MAM reports now.
fn listed_then_looked_up(
    harness: &mut Harness,
    bookmarks: Vec<Bookmark>,
    now_listed: &[Bookmark],
) -> Vec<Action> {
    let mut actions = listed_with(harness, account(harness.wall()), Vec::new());
    while !requests(&actions).is_empty() {
        actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    }
    let _ = harness.send(Event::Registry(RegistryEvent::Saved));
    let actions = harness.later(Duration::from_mins(15));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let actions = harness.answer(read(account(harness.wall())));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let actions = harness.answer(listed(bookmarks));
    harness.checked(actions, now_listed)
}

/// A download Windlass added, as storage holds it after a restart: added
/// `ago`, and finished or not.
fn download_row(
    id: u64,
    fill: char,
    ago: Duration,
    finished: bool,
    now: SystemTime,
) -> TorrentRecord {
    TorrentRecord {
        id: windlass::core::RecordId::new(id).unwrap_or_else(|| unreachable!("non-zero")),
        identity: windlass::core::Identity::Linked {
            mam: torrent_id(id),
            hash: hash(fill),
        },
        origin: windlass::core::Origin::Bookmark,
        desired: Desired::Active,
        save_path: None,
        deletion_allowed: false,
        observation: Some(windlass::core::Observation {
            torrent: windlass::core::Torrent {
                name: windlass::core::TorrentName::try_new(format!("Synthetic {id}")).ok(),
                added_at: Some(now - ago),
                completed_at: finished.then_some(now),
                ..windlass::core::Torrent::default()
            },
            last_seen: now,
        }),
    }
}

fn empty_harness() -> Harness {
    Harness {
        core: Core::new(),
        start: Instant::now(),
        elapsed: Duration::ZERO,
        position: 0,
        logged: Vec::new(),
    }
}

#[test]
fn a_bookmark_is_queued_wedged_fetched_and_added() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let mut snatched = bookmark(2);
    snatched.snatched = Snatched::Before;
    let actions = listed_with(&mut harness, account(now), vec![bookmark(1), snatched]);
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Queued)
    );
    assert_eq!(
        harness.record(2).map(|record| record.desired),
        Some(Desired::Snatched),
        "snatched before, so known but not queued"
    );
    // The bookmark listing had just given its flags, so no lookup was needed.
    assert!(harness.mam().acquisition.lookups.is_empty());
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: true
        }]
    );
    let actions = harness.answer(Ok(MamReply::Fetched(FetchedTorrent {
        hash: hash('a'),
        size: ByteSize(GIB),
    })));
    assert_eq!(
        harness
            .record(1)
            .map(|record| (record.desired, record.identity.hash().cloned())),
        Some((Desired::Active, Some(hash('a'))))
    );
    assert!(
        requests(&actions).is_empty(),
        "the bonus history waits until MAM recorded the wedge"
    );

    // The next qBittorrent update adds it from the kept file.
    let actions = harness.poll(Vec::new());
    assert!(actions.contains(&Action::QbittorrentCommand {
        command: TorrentCommand::Add {
            mam: torrent_id(1),
            hash: hash('a')
        }
    }));

    // MAM's bonus history must show the wedge spent; it does.
    let actions = harness.later(Duration::from_mins(1));
    assert_eq!(requests(&actions), [MamRequest::WedgeHistory]);
    let _ = harness.answer(Ok(MamReply::WedgeHistory(vec![WedgeSpent {
        torrent: torrent_id(1),
        at: harness.wall(),
    }])));
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Active)
    );
    assert!(matches!(
        harness.mam().acquisition.watched.as_slice(),
        [(_, DownloadWatch::Wedged)]
    ));
    assert!(!harness.notices().contains(&Notice::WedgeNotSpent));
}

#[test]
fn a_snatched_bookmark_waits_until_the_owner_asks_for_it() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let mut snatched = bookmark(2);
    snatched.snatched = Snatched::Before;
    let actions = listed_with(&mut harness, account(now), vec![snatched.clone()]);
    assert!(requests(&actions).is_empty(), "nothing is downloaded");
    let Some(record) = harness.record(2) else {
        unreachable!("the snatched bookmark is recorded");
    };
    assert_eq!(record.desired, Desired::Snatched);
    // A later listing that no longer says snatched does not queue it.
    let _ = harness.send(Event::Registry(RegistryEvent::Saved));
    let actions = harness.later(Duration::from_mins(16));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let actions = harness.answer(read(account(harness.wall())));
    snatched.snatched = Snatched::Never;
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(vec![snatched]));
    assert_eq!(
        harness.record(2).map(|record| record.desired),
        Some(Desired::Snatched)
    );
    // Asking for a row that is not snatched changes nothing.
    let other = windlass::core::RecordId::new(99).unwrap_or_else(|| unreachable!("non-zero"));
    assert!(
        harness
            .send(Event::Registry(RegistryEvent::Redownload(other)))
            .is_empty()
    );
    // The owner asks for it on the dashboard: it is queued and downloaded.
    let actions = harness.send(Event::Registry(RegistryEvent::Redownload(record.id)));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::SaveTorrents { .. })),
        "{actions:?}"
    );
    assert_eq!(
        harness
            .record(2)
            .map(|record| (record.desired, record.origin)),
        Some((Desired::Queued, windlass::core::Origin::Redownload))
    );
    // MAM still says snatched, but the owner asked for it.
    let mut asked = bookmark(2);
    asked.snatched = Snatched::Before;
    let actions = harness.checked(actions, &[asked]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(2),
            wedge: true
        }]
    );
}

#[test]
fn downloads_wait_until_mam_lists_the_running_qbittorrent() {
    let mut refused = listed_client("5.2.2");
    refused.refusal = ClientRefusal::Refused;
    let unreadable = ListedClient {
        qbittorrent: None,
        connectable: None,
        refusal: ClientRefusal::NotRefused,
    };
    for (clients, standing) in [
        (vec![listed_client("5.1.0")], ClientStanding::NotListed),
        (
            vec![listed_client("5.1.0"), refused],
            ClientStanding::Refused,
        ),
        (vec![unreadable], ClientStanding::NotUnderstood),
    ] {
        let mut harness = empty_harness();
        let mut data = account(harness.wall());
        data.clients = Some(clients);
        let actions = listed_with(&mut harness, data, vec![bookmark(1)]);
        assert!(requests(&actions).is_empty(), "{standing:?}");
        assert_eq!(harness.mam().client, standing);
        assert_eq!(
            harness.mam().acquisition.hold,
            Some(Hold::MamClientList(standing))
        );
        assert!(
            harness
                .notices()
                .contains(&Notice::MamClientNotListed { standing }),
            "{:?}",
            harness.notices()
        );
        // The next client list names the running version: the download starts.
        let actions = harness.later(Duration::from_mins(31));
        assert_eq!(
            requests(&actions),
            [MamRequest::LoadUserData { client_stats: true }]
        );
        let actions = harness.answer(read(account(harness.wall())));
        let actions = harness.checked(actions, &[bookmark(1)]);
        assert_eq!(harness.mam().client, ClientStanding::Listed);
        assert!(matches!(
            requests(&actions).as_slice(),
            [MamRequest::FetchTorrent { .. }]
        ));
        assert!(harness.notices().contains(&Notice::MamClientListed));
    }
    // An empty list, as before anything seeds, admits the first download.
    let mut harness = empty_harness();
    let mut data = account(harness.wall());
    data.clients = Some(Vec::new());
    let actions = listed_with(&mut harness, data, vec![bookmark(1)]);
    assert_eq!(harness.mam().client, ClientStanding::NoneListed);
    assert!(matches!(
        requests(&actions).as_slice(),
        [MamRequest::FetchTorrent { .. }]
    ));
}

/// Read the account once more, after this long, with this connectability.
fn read_later(harness: &mut Harness, after: Duration, connectable: Connectable) -> Vec<Action> {
    let actions = harness.later(after);
    assert!(
        matches!(
            requests(&actions).as_slice(),
            [MamRequest::LoadUserData { .. }]
        ),
        "{actions:?}"
    );
    let mut data = account(harness.wall());
    if let Some(summary) = &mut data.summary {
        summary.connectable = connectable;
    }
    let mut actions = harness.answer(read(data));
    // Bookmarks fall due on the same schedule; there are none.
    while requests(&actions) == [MamRequest::ListBookmarks { start: 0 }] {
        actions = harness.answer(listed(Vec::new()));
    }
    actions
}

#[test]
fn an_unreachable_client_is_repaired_and_reported_if_that_did_not_work() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let forwarded = Port::new(41000).unwrap_or_else(|| unreachable!("non-zero"));
    for event in [
        PortForwardingEvent::Enable,
        PortForwardingEvent::Mapped {
            tcp: forwarded,
            udp: forwarded,
            lifetime: LEASE_LIFETIME,
        },
    ] {
        let _ = harness.send(Event::PortForwarding(event));
    }
    let actions = harness.register(&actions);
    assert!(matches!(
        requests(&actions).as_slice(),
        [MamRequest::LoadUserData { .. }]
    ));
    let mut data = account(harness.wall());
    if let Some(summary) = &mut data.summary {
        summary.connectable = Connectable::No;
    }
    let actions = harness.answer(read(data));
    // The repair: a new lease, and the IP observed and registered again.
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::RequestPortMapping { .. })),
        "{actions:?}"
    );
    // The IP is looked up again once MAM's minute has passed.
    assert!(requests(&actions).is_empty(), "{actions:?}");
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let expected = Findings {
        listen_port: PortFinding::Differs {
            listening: None,
            forwarded,
        },
        lease_renewed: true,
        exit_ip: IpFinding::Matches(ip(10)),
        registered: true,
        interface: None,
    };
    assert!(matches!(
        &harness.mam().connectability,
        Connectability::Repaired { findings, .. } if *findings == expected
    ));
    assert_eq!(
        harness.mam().seedbox.status,
        SeedboxStatus::Unconfirmed,
        "registered again when allowed"
    );
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(Vec::new()));
    // A reading too soon after the repair does not judge it.
    let _ = read_later(&mut harness, Duration::from_mins(16), Connectable::No);
    assert!(matches!(
        harness.mam().connectability,
        Connectability::Repaired { .. }
    ));
    // One taken 30 minutes after still cannot reach it: the owner is told.
    let _ = read_later(&mut harness, Duration::from_mins(16), Connectable::No);
    assert!(matches!(
        harness.mam().connectability,
        Connectability::Unresolved { .. }
    ));
    assert!(
        harness
            .notices()
            .contains(&Notice::NotConnectable { findings: expected })
    );
    // MAM reaches it again.
    let _ = read_later(&mut harness, Duration::from_mins(16), Connectable::Yes);
    assert_eq!(harness.mam().connectability, Connectability::Fine);
    assert!(harness.notices().contains(&Notice::ConnectableAgain));
}

#[test]
fn a_repair_records_a_matching_port_and_a_differing_exit_ip() {
    let (mut harness, actions) = Harness::start(Setup {
        client: false,
        ..Setup::default()
    });
    let forwarded = Port::new(41000).unwrap_or_else(|| unreachable!("non-zero"));
    for event in [
        Event::PortForwarding(PortForwardingEvent::Enable),
        Event::PortForwarding(PortForwardingEvent::Mapped {
            tcp: forwarded,
            udp: forwarded,
            lifetime: LEASE_LIFETIME,
        }),
        Event::Qbittorrent(QbittorrentEvent::Enable),
        Event::Qbittorrent(QbittorrentEvent::Connected {
            version: windlass::core::ReportedVersion::Release(windlass::core::ClientVersion::new(
                5, 2, 2,
            )),
            preferences: QbittorrentPreferences {
                listen_port: Some(forwarded),
                interface: windlass::core::NetworkInterface::try_new("wg0").ok(),
                queue: None,
            },
        }),
    ] {
        let _ = harness.send(event);
    }
    let _ = harness.sync(Vec::new());
    // MAM sees another IP than the tunnel's check did, and registers it.
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let _ = harness.answer(Ok(MamReply::Ip(observation(12, 209_103))));
    let _ = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
    let actions = harness.answer(Ok(MamReply::Registered {
        registration: Registration::Completed,
        ip: ip(12),
    }));
    assert_eq!(requests(&actions).len(), 1);
    let mut data = account(harness.wall());
    if let Some(summary) = &mut data.summary {
        summary.connectable = Connectable::No;
    }
    let _ = harness.answer(read(data));
    let expected = Findings {
        listen_port: PortFinding::Matches(forwarded),
        lease_renewed: true,
        exit_ip: IpFinding::Differs {
            mam: ip(12),
            tunnel: ip(10),
        },
        registered: true,
        interface: windlass::core::NetworkInterface::try_new("wg0").ok(),
    };
    assert!(
        matches!(
            &harness.mam().connectability,
            Connectability::Repaired { findings, .. } if *findings == expected
        ),
        "{:?}",
        harness.mam().connectability
    );
}

#[test]
fn a_repair_that_worked_is_not_reported() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let mut data = account(harness.wall());
    // The running client's own entry says MAM cannot reach it.
    data.clients = Some(vec![ListedClient {
        connectable: Some(Connectable::No),
        ..listed_client("5.2.2")
    }]);
    let actions = harness.answer(read(data));
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::RequestPortMapping { .. })),
        "no port is forwarded, so no lease is renewed"
    );
    assert!(matches!(
        &harness.mam().connectability,
        Connectability::Repaired { findings, .. }
            if findings.listen_port == PortFinding::NoForwardedPort && !findings.lease_renewed
    ));
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(Vec::new()));
    let _ = read_later(&mut harness, Duration::from_mins(31), Connectable::Yes);
    assert_eq!(harness.mam().connectability, Connectability::Fine);
    assert!(
        !harness
            .notices()
            .iter()
            .any(|notice| matches!(notice, Notice::NotConnectable { .. }))
    );
}

#[test]
fn a_wedge_the_bonus_history_does_not_show_stops_the_torrent() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let actions = listed_with(&mut harness, account(now), vec![bookmark(1)]);
    assert_eq!(requests(&actions).len(), 1);
    let _ = harness.answer(Ok(MamReply::Fetched(FetchedTorrent {
        hash: hash('b'),
        size: ByteSize(GIB),
    })));
    // The download starts; MAM's bonus history must then show its wedge.
    let actions = harness.later(Duration::from_mins(1));
    assert_eq!(requests(&actions), [MamRequest::WedgeHistory]);
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Active)
    );
    // Only another torrent's wedge is listed.
    let _ = harness.answer(Ok(MamReply::WedgeHistory(vec![WedgeSpent {
        torrent: torrent_id(7),
        at: harness.wall(),
    }])));
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Held)
    );
    assert!(harness.notices().contains(&Notice::WedgeNotSpent));
    // Once qBittorrent lists it, it is stopped, and no second wedge is tried.
    let actions = harness.poll(vec![hash('b')]);
    assert!(actions.contains(&Action::QbittorrentCommand {
        command: TorrentCommand::Stop(vec![hash('b')])
    }));
    let actions = harness.later(Duration::from_mins(1));
    assert!(
        !requests(&actions)
            .iter()
            .any(|request| matches!(request, MamRequest::FetchTorrent { .. })),
        "{actions:?}"
    );
}

/// A wedged download whose answer is lost, then the reading and lookup that
/// come before trying it again; the actions asking for the bonus history.
fn lost_wedged_download(harness: &mut Harness) -> Vec<Action> {
    let now = harness.wall();
    let actions = listed_with(harness, account(now), vec![bookmark(1), bookmark(2)]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: true
        }]
    );
    let _ = harness.answer(Err(MamFailure::Unreachable));
    let actions = harness.later(Duration::from_mins(1));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    harness.answer(read(account(harness.wall())))
}

#[test]
fn a_wedge_the_bonus_history_shows_spent_is_not_spent_again() {
    let mut harness = empty_harness();
    // The bonus history must show whether the lost attempt spent a wedge.
    let actions = lost_wedged_download(&mut harness);
    assert_eq!(requests(&actions), [MamRequest::WedgeHistory]);
    // Reading it fails; it is asked for again after a wait.
    let actions = harness.answer(Err(MamFailure::Unreachable));
    assert!(requests(&actions).is_empty(), "{actions:?}");
    let actions = harness.later(Duration::from_mins(6));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let actions = harness.answer(read(account(harness.wall())));
    let spent = vec![WedgeSpent {
        torrent: torrent_id(1),
        at: harness.wall(),
    }];
    let actions = harness.checked_with(actions, &[bookmark(1)], &spent);
    // It was spent, so the file is fetched again without spending another.
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: false
        }]
    );
    let _ = harness.answer(Ok(MamReply::Fetched(FetchedTorrent {
        hash: hash('c'),
        size: ByteSize(GIB),
    })));
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Active)
    );
    assert!(matches!(
        harness.mam().acquisition.watched.as_slice(),
        [(_, DownloadWatch::Wedged)]
    ));
}

#[test]
fn a_lost_wedged_download_that_spent_nothing_is_wedged_again() {
    let mut harness = empty_harness();
    let actions = lost_wedged_download(&mut harness);
    let actions = harness.checked(actions, &[bookmark(1)]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: true
        }]
    );
}

#[test]
fn each_download_is_judged_by_its_listing_on_mam_now() {
    let mut harness = empty_harness();
    // Freeleech for everyone, as the bookmark listing reports it now: no wedge.
    let mut free = bookmark(1);
    free.freeleech = Freeleech::ForEveryone;
    let actions = listed_then_looked_up(&mut harness, vec![free], &[]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: false
        }]
    );
    // Listed as VIP, but its VIP status ends within minutes: a wedge.
    let mut ending = bookmark(1);
    ending.vip = VipTorrent::Until(harness.wall() + Duration::from_mins(2));
    let actions = listed_then_looked_up(&mut harness, vec![ending], &[]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: true
        }]
    );
    // Free through VIP while the account is VIP; not once VIP lapsed.
    let mut through_vip = bookmark(1);
    through_vip.personal = PersonalFreeleech::ThroughVip;
    let actions = listed_then_looked_up(&mut harness, vec![through_vip], &[]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: false
        }]
    );
}

#[test]
fn torrents_mam_no_longer_lists_or_reports_snatched_are_not_downloaded() {
    let mut harness = empty_harness();
    // No wedge is left, so the bookmarks wait in the queue.
    let mut none_left = account(harness.wall());
    none_left.wedges = WedgeCount::new(0);
    let actions = listed_with(
        &mut harness,
        none_left,
        vec![bookmark(1), bookmark(2), bookmark(3), bookmark(4)],
    );
    assert!(requests(&actions).is_empty(), "{actions:?}");
    assert_eq!(harness.mam().acquisition.hold, Some(Hold::NoWedge));
    let _ = harness.send(Event::Registry(RegistryEvent::Saved));
    // The next listing reports torrent 2 snatched since: it waits for the owner.
    let mut snatched = bookmark(2);
    snatched.snatched = Snatched::Before;
    let actions = harness.later(Duration::from_mins(15));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let mut still_none = account(harness.wall());
    still_none.wedges = WedgeCount::new(0);
    let actions = harness.answer(read(still_none));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let actions = harness.answer(listed(vec![
        bookmark(1),
        snatched,
        bookmark(3),
        bookmark(4),
    ]));
    let _ = harness.checked(actions, &[]);
    assert_eq!(
        harness.record(2).map(|record| record.desired),
        Some(Desired::Snatched)
    );
    // Wedges arrive once the listing is old: each candidate is looked up.
    // Torrent 1 is gone from MAM, and torrent 3 snatched since.
    let mut snatched_since = bookmark(3);
    snatched_since.snatched = Snatched::Before;
    let actions = harness.later(Duration::from_mins(6));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let actions = harness.answer(read(account(harness.wall())));
    let actions = harness.checked(actions, &[snatched_since, bookmark(4)]);
    assert_eq!(
        harness.record(3).map(|record| record.desired),
        Some(Desired::Snatched)
    );
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(4),
            wedge: true
        }]
    );
    assert!(matches!(
        harness.mam().acquisition.lookups.get(&torrent_id(1)),
        Some(windlass::core::Lookup { listing: None, .. })
    ));
}

#[test]
fn a_failed_lookup_is_tried_again_after_a_wait() {
    let now = SystemTime::UNIX_EPOCH + EPOCH;
    let (mut harness, actions) = Harness::start(Setup {
        rows: Some(vec![download_row(
            1,
            'a',
            Duration::from_hours(3),
            false,
            now,
        )]),
        ..Setup::default()
    });
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let actions = harness.answer(read(account(harness.wall())));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let actions = harness.answer(listed(Vec::new()));
    assert_eq!(
        requests(&actions),
        [MamRequest::LookUpTorrent { id: torrent_id(1) }]
    );
    let actions = harness.answer(Err(MamFailure::Unreachable));
    let actions = without_bookmarks(&mut harness, actions);
    assert!(requests(&actions).is_empty(), "{actions:?}");
    let actions = harness.later(Duration::from_mins(5));
    assert_eq!(
        requests(&actions),
        [MamRequest::LookUpTorrent { id: torrent_id(1) }]
    );
}

#[test]
fn downloads_added_in_the_last_hour_count_against_the_limit_after_a_restart() {
    let now = SystemTime::UNIX_EPOCH + EPOCH;
    let rows = vec![
        download_row(4, 'a', Duration::from_mins(10), true, now),
        download_row(5, 'b', Duration::from_mins(20), true, now),
        download_row(6, 'c', Duration::from_mins(30), true, now),
        download_row(7, 'd', Duration::from_hours(2), true, now),
    ];
    let (mut harness, actions) = Harness::start(Setup {
        rows: Some(rows),
        ..Setup::default()
    });
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let mut near = account(harness.wall());
    if let Some(summary) = &mut near.summary {
        summary.unsatisfied = UnsatisfiedCount::new(145);
    }
    let actions = harness.answer(read(near));
    let _ = harness.checked(actions, &[bookmark(1)]);
    // 145 by MAM's count and 3 added in the last hour leave no slot.
    assert_eq!(
        harness.mam().acquisition.hold,
        Some(Hold::UnsatisfiedLimit {
            count: UnsatisfiedCount::new(148),
            limit: UnsatisfiedLimit::new(150),
        })
    );
}

#[test]
fn no_download_starts_while_the_downloading_limit_is_reached() {
    let now = SystemTime::UNIX_EPOCH + EPOCH;
    let fills = ['a', 'b', 'c', 'd', 'e', 'f'];
    let rows = (2..=7)
        .zip(fills)
        .map(|(id, fill)| download_row(id, fill, Duration::from_hours(3), false, now))
        .collect::<Vec<_>>();
    // Five running downloads, the default limit; the sixth is held.
    let mut rows = rows;
    if let Some(held) = rows.last_mut() {
        held.desired = Desired::Held;
    }
    let (mut harness, actions) = Harness::start(Setup {
        rows: Some(rows),
        ..Setup::default()
    });
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let actions = harness.answer(read(account(harness.wall())));
    let free = |id| {
        let mut listing = bookmark(id);
        listing.freeleech = Freeleech::ForEveryone;
        listing
    };
    let listings: Vec<Bookmark> = std::iter::once(bookmark(1))
        .chain((2..=7).map(free))
        .collect();
    let _ = harness.checked(actions, &listings);
    assert_eq!(
        harness.mam().acquisition.hold,
        Some(Hold::DownloadingLimit {
            downloading: 5,
            limit: windlass::core::DownloadingLimit::default()
        })
    );
}

/// Start with this unfinished download in storage, read the account, and
/// answer MAM's lookup of it with this listing.
fn running_download(listing: Bookmark, wedges: u32) -> (Harness, Vec<Action>) {
    let now = SystemTime::UNIX_EPOCH + EPOCH;
    let (mut harness, actions) = Harness::start(Setup {
        rows: Some(vec![download_row(
            1,
            'a',
            Duration::from_hours(3),
            false,
            now,
        )]),
        ..Setup::default()
    });
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let mut data = account(harness.wall());
    data.wedges = WedgeCount::new(wedges);
    // After a restart, the download is checked with MAM before anything else:
    // the bookmark listing first, then a lookup, since it is not bookmarked.
    let actions = harness.answer(read(data));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let actions = harness.answer(listed(Vec::new()));
    assert_eq!(
        requests(&actions),
        [MamRequest::LookUpTorrent { id: torrent_id(1) }]
    );
    let actions = harness.answer(Ok(MamReply::Torrent(Some(Box::new(listing)))));
    (harness, actions)
}

#[test]
fn a_download_whose_freeleech_ends_is_wedged_while_it_runs() {
    let mut free = bookmark(1);
    free.freeleech = Freeleech::ForEveryone;
    let (mut harness, actions) = running_download(free, 200);
    assert!(matches!(
        harness.mam().acquisition.watched.as_slice(),
        [(_, DownloadWatch::FreeForNow)]
    ));
    let actions = harness.checked(actions, &[]);
    let _ = without_bookmarks(&mut harness, actions);
    // Not bookmarked, so it is looked up by itself, and not again soon.
    let actions = harness.later(Duration::from_mins(10));
    let actions = without_bookmarks(&mut harness, actions);
    assert!(
        !requests(&actions)
            .iter()
            .any(|request| matches!(request, MamRequest::LookUpTorrent { .. })),
        "{actions:?}"
    );
    // The routine reading and bookmark listing come and go.
    let actions = harness.later(Duration::from_mins(6));
    let actions = harness.respond(actions, &bookmark(1));
    assert!(requests(&actions).is_empty(), "{actions:?}");
    // Freeleech ends; the next check finds it, and spending a wedge needs a
    // fresh count of wedges.
    let actions = harness.later(Duration::from_mins(6));
    let actions = harness.checked(actions, &[bookmark(1)]);
    assert!(matches!(
        requests(&actions).as_slice(),
        [MamRequest::LoadUserData { .. }]
    ));
    let actions = harness.answer(read(account(harness.wall())));
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(1),
            wedge: true
        }]
    );
    let _ = harness.answer(Ok(MamReply::Fetched(FetchedTorrent {
        hash: hash('a'),
        size: ByteSize(GIB),
    })));
    assert!(harness.notices().contains(&Notice::WedgedMidDownload));
    let actions = harness.later(Duration::from_mins(1));
    assert_eq!(requests(&actions), [MamRequest::WedgeHistory]);
    let _ = harness.answer(Ok(MamReply::WedgeHistory(vec![WedgeSpent {
        torrent: torrent_id(1),
        at: harness.wall(),
    }])));
    assert!(matches!(
        harness.mam().acquisition.watched.as_slice(),
        [(_, DownloadWatch::Wedged)]
    ));
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Active)
    );
}

#[test]
fn a_download_that_is_not_free_and_cannot_be_wedged_stops_until_it_is_free() {
    let (mut harness, actions) = running_download(bookmark(1), 0);
    // The bonus history shows no wedge on it.
    let _ = harness.checked(actions, &[]);
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Held)
    );
    assert!(
        harness
            .notices()
            .contains(&Notice::DownloadNotFree { stopped: true })
    );
    // With no wedge to spend, nothing more is asked.
    let actions = harness.later(Duration::from_mins(1));
    let actions = without_bookmarks(&mut harness, actions);
    assert!(requests(&actions).is_empty(), "{actions:?}");
    // It becomes freeleech for everyone: it runs again.
    let mut free = bookmark(1);
    free.freeleech = Freeleech::ForEveryone;
    let actions = harness.later(Duration::from_mins(21));
    let _ = harness.respond(actions, &free);
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Active)
    );
}

#[test]
fn a_download_mam_no_longer_lists_stops_and_a_finished_one_is_reported() {
    let now = SystemTime::UNIX_EPOCH + EPOCH;
    let (mut harness, actions) = Harness::start(Setup {
        rows: Some(vec![
            download_row(1, 'a', Duration::from_mins(5), false, now),
            download_row(2, 'b', Duration::from_mins(5), true, now),
        ]),
        ..Setup::default()
    });
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let actions = harness.answer(read(account(harness.wall())));
    let _ = harness.checked(actions, &[]);
    assert_eq!(
        harness.record(1).map(|record| record.desired),
        Some(Desired::Held)
    );
    assert_eq!(
        harness.record(2).map(|record| record.desired),
        Some(Desired::Active),
        "a finished download keeps seeding"
    );
    let notices = harness.notices();
    assert!(notices.contains(&Notice::DownloadNotFree { stopped: true }));
    assert!(notices.contains(&Notice::DownloadNotFree { stopped: false }));
}

#[test]
fn vip_torrents_need_no_wedge_while_the_account_is_vip() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let mut vip = bookmark(3);
    vip.vip = VipTorrent::Indefinitely;
    let actions = listed_with(&mut harness, account(now), vec![vip.clone()]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(3),
            wedge: false
        }]
    );
    // A failed fetch without a wedge spends nothing.
    let _ = harness.answer(Err(MamFailure::Unreachable));
    assert!(harness.mam().acquisition.wedge_attempts.is_empty());
    // Not VIP any more: the same torrent needs a wedge.
    let mut lapsed = account(now);
    lapsed.vip_until = None;
    lapsed.class = UserClass::PowerUser;
    lapsed.points = Points(1000);
    let actions = listed_with(&mut harness, lapsed, vec![vip]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(3),
            wedge: true
        }]
    );
    // A personal freeleech needs none.
    let mut personal = bookmark(4);
    personal.personal = PersonalFreeleech::ThroughWedge;
    let actions = listed_with(&mut harness, account(now), vec![personal]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(4),
            wedge: false
        }]
    );
}

#[test]
fn every_admission_rule_can_hold_a_download() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let check = |harness: &mut Harness, data: UserData, hold: Hold| {
        let actions = listed_with(harness, data, vec![bookmark(5)]);
        assert!(
            !requests(&actions)
                .iter()
                .any(|request| matches!(request, MamRequest::FetchTorrent { .. })),
            "{hold:?}"
        );
        assert_eq!(harness.mam().acquisition.hold, Some(hold));
    };
    let mut no_wedges = account(now);
    no_wedges.wedges = WedgeCount::new(0);
    no_wedges.vip_until = None;
    no_wedges.points = Points(1000);
    check(&mut harness, no_wedges, Hold::NoWedge);
    let mut full = account(now);
    if let Some(summary) = &mut full.summary {
        summary.unsatisfied = UnsatisfiedCount::new(148);
    }
    check(
        &mut harness,
        full,
        Hold::UnsatisfiedLimit {
            count: UnsatisfiedCount::new(148),
            limit: UnsatisfiedLimit::new(150),
        },
    );
    let mut low = account(now);
    low.uploaded = ByteSize(GIB);
    low.downloaded = ByteSize(GIB);
    low.points = Points(0);
    check(&mut harness, low, Hold::RatioLow);
    let mut unknown = account(now);
    unknown.summary = None;
    check(&mut harness, unknown, Hold::AccountUnknown);
}

#[test]
fn a_refused_download_holds_every_download() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let actions = listed_with(&mut harness, account(now), vec![bookmark(6), bookmark(7)]);
    assert_eq!(requests(&actions).len(), 1);
    let _ = harness.answer(Err(MamFailure::Refused { message: None }));
    assert!(harness.mam().acquisition.refused.is_some());
    assert!(harness.notices().contains(&Notice::DownloadsRefused));
    let actions = harness.send(Event::Mam(MamEvent::ReminderSaved));
    assert!(requests(&actions).is_empty());
    assert_eq!(harness.mam().acquisition.hold, Some(Hold::DownloadsRefused));
}

#[test]
fn vip_is_kept_within_the_reserve() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let mut ending = account(now);
    ending.vip_until = now.checked_add(10 * DAY);
    let (started, actions) = Harness::start(Setup::default());
    harness = started;
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let actions = harness.answer(read(ending.clone()));
    assert_eq!(requests(&actions), [MamRequest::Buy(Purchase::Vip)]);
    // 80 days at 1,250 points a week cost 14,286 points; MAM charged 14,000.
    let actions = harness.answer(Ok(MamReply::Purchased {
        points: Points(46_000),
    }));
    assert!(harness.notices().contains(&Notice::PurchaseMade {
        purchase: Purchase::Vip,
        points: Points(46_000)
    }));
    assert_eq!(harness.mam().account.vip_price_per_week, 1225);
    // Nothing more is bought until a new reading.
    assert!(
        !requests(&actions)
            .iter()
            .any(|request| matches!(request, MamRequest::Buy(_)))
    );

    // Within the reserve, VIP is not renewed and the owner is told.
    let mut poor = ending;
    poor.points = Points(25_000);
    poor.vip_until = now.checked_add(5 * DAY);
    let actions = listed_with(&mut harness, poor, Vec::new());
    assert!(
        !requests(&actions)
            .iter()
            .any(|request| matches!(request, MamRequest::Buy(_)))
    );
    assert!(
        harness
            .notices()
            .iter()
            .any(|notice| matches!(notice, Notice::VipEnding { .. }))
    );
}

/// Answer bookmark listings with none until something else is asked.
fn without_bookmarks(harness: &mut Harness, mut actions: Vec<Action>) -> Vec<Action> {
    while requests(&actions) == [MamRequest::ListBookmarks { start: 0 }] {
        actions = harness.answer(listed(Vec::new()));
    }
    actions
}

#[test]
fn an_uncertain_vip_purchase_is_settled_from_the_next_reading() {
    let mut harness = empty_harness();
    let mut ending = account(harness.wall());
    ending.vip_until = harness.wall().checked_add(DAY);
    let (started, actions) = Harness::start(Setup::default());
    harness = started;
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let actions = harness.answer(read(ending.clone()));
    assert_eq!(requests(&actions), [MamRequest::Buy(Purchase::Vip)]);
    let actions = harness.answer(Err(MamFailure::Malformed));
    let _ = without_bookmarks(&mut harness, actions);
    let actions = harness.later(Duration::from_mins(16));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    // VIP now ends later: MAM sold it, so nothing is bought again.
    let mut renewed = ending;
    renewed.vip_until = harness.wall().checked_add(90 * DAY);
    renewed.points = Points(52_000);
    let actions = harness.answer(read(renewed));
    assert!(harness.notices().contains(&Notice::PurchaseSettled {
        purchase: Purchase::Vip,
        made: true
    }));
    assert!(
        !requests(&actions)
            .iter()
            .any(|request| matches!(request, MamRequest::Buy(_)))
    );
}

#[test]
fn a_purchase_the_reading_cannot_settle_waits_for_the_owner() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let mut thin = account(harness.wall());
    thin.uploaded = ByteSize(20 * GIB);
    let actions = harness.answer(read(thin.clone()));
    let Some(MamRequest::Buy(purchase)) = requests(&actions).first().cloned() else {
        unreachable!("no purchase in {actions:?}");
    };
    let actions = harness.answer(Err(MamFailure::Unreachable));
    let _ = without_bookmarks(&mut harness, actions);
    // Points were spent, but no upload credit arrived: Windlass cannot tell.
    // The next reading comes early, to settle it.
    let actions = harness.later(Duration::from_mins(1));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let mut spent = thin.clone();
    spent.points = Points(35_000);
    let actions = harness.answer(read(spent.clone()));
    let actions = without_bookmarks(&mut harness, actions);
    assert!(
        !requests(&actions)
            .iter()
            .any(|request| matches!(request, MamRequest::Buy(_)))
    );
    assert_eq!(harness.mam().account.unresolved_purchase(), Some(purchase));
    assert!(
        harness
            .notices()
            .contains(&Notice::PurchaseNeedsChecking { purchase })
    );
    // A check of a purchase that is not waiting changes nothing.
    let _ = harness.send(Event::Mam(MamEvent::PurchaseChecked { made: true }));
    let _ = harness.send(Event::Mam(MamEvent::PurchaseChecked { made: true }));
    assert!(matches!(
        harness
            .mam()
            .account
            .last_purchase
            .as_ref()
            .and_then(|record| record.outcome.clone()),
        Some(windlass::core::PurchaseOutcome::Settled {
            made: true,
            by: windlass::core::SettledBy::Owner
        })
    ));
    assert!(harness.notices().contains(&Notice::PurchaseSettled {
        purchase,
        made: true
    }));
    // Buying resumes with the next fresh reading.
    let actions = harness.later(Duration::from_mins(15));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let mut earned = spent;
    earned.points = Points(60_000);
    let actions = harness.answer(read(earned));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::Buy(Purchase::Upload(_)))
    ));
    // This one's answer is lost too, but the credit arrives with the points
    // spent: MAM sold it.
    let actions = harness.answer(Err(MamFailure::Unreachable));
    let _ = without_bookmarks(&mut harness, actions);
    let actions = harness.later(Duration::from_mins(15));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let mut credited = thin;
    credited.points = Points(35_000);
    credited.uploaded = ByteSize(70 * GIB);
    let _ = harness.answer(read(credited));
    assert!(harness.notices().contains(&Notice::PurchaseSettled {
        purchase,
        made: true
    }));
}

#[test]
fn upload_credit_keeps_the_buffer_and_uncertain_purchases_wait() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let mut thin = account(harness.wall());
    thin.uploaded = ByteSize(20 * GIB);
    let actions = harness.answer(read(thin.clone()));
    let Some(MamRequest::Buy(Purchase::Upload(amount))) = requests(&actions).first().cloned()
    else {
        unreachable!("no upload purchase in {actions:?}");
    };
    assert_eq!(amount.into_inner(), 50);
    let actions = harness.answer(Err(MamFailure::Unreachable));
    assert!(harness.notices().contains(&Notice::PurchaseUncertain {
        purchase: Purchase::Upload(amount)
    }));
    // Nothing more is bought until the next reading decides from MAM's figures.
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(Vec::new()));
    // A refusal is not retried for hours.
    let actions = harness.later(Duration::from_mins(15));
    assert!(
        matches!(
            requests(&actions).first(),
            Some(MamRequest::LoadUserData { .. })
        ),
        "{actions:?}"
    );
    // The points were not spent, so MAM did not sell it, and it is bought again.
    let actions = harness.answer(read(thin.clone()));
    assert!(harness.notices().contains(&Notice::PurchaseSettled {
        purchase: Purchase::Upload(amount),
        made: false
    }));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::Buy(Purchase::Upload(_)))
    ));
    let actions = harness.answer(Err(MamFailure::Refused {
        message: windlass::core::MamMessage::try_new("Not enough points").ok(),
    }));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(Vec::new()));
    assert!(harness.notices().contains(&Notice::PurchaseRefused {
        purchase: Purchase::Upload(amount)
    }));
    let actions = harness.later(Duration::from_mins(15));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let actions = harness.answer(read(thin));
    assert!(
        !requests(&actions)
            .iter()
            .any(|request| matches!(request, MamRequest::Buy(_))),
        "{actions:?}"
    );
}

#[test]
fn the_owner_is_reminded_weekly_to_visit_mam() {
    let mut harness = empty_harness();
    for event in [
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
        Event::Startup(StartupEvent::StorageOpened),
        Event::Notification(NotificationEvent::LogRead { last: None }),
    ] {
        let _ = harness.send(event);
    }
    // The first run starts the count without a reminder.
    let actions = harness.send(Event::Mam(MamEvent::StateRead {
        last_registration: None,
        last_reminder: None,
    }));
    let started = harness.wall();
    assert!(actions.contains(&Action::SaveReminder { at: started }));
    assert!(!harness.notices().contains(&Notice::MamVisitReminder));
    assert_eq!(
        timer(&actions, TimerName::Mam).map(|(_, after)| after),
        Some(7 * DAY)
    );
    // A timer that was replaced expires without effect.
    let _ = harness.send(Event::TimerExpired {
        name: TimerName::Mam,
        id: timer(&actions, TimerName::Mam).map_or_else(|| unreachable!("timer"), |(id, _)| id),
    });
    let actions = harness.wake(&actions);
    assert!(harness.notices().contains(&Notice::MamVisitReminder));
    assert!(actions.contains(&Action::SaveReminder {
        at: started + 7 * DAY
    }));
    let _ = harness.send(Event::Mam(MamEvent::ReminderSaveFailed));
}

#[test]
fn the_account_watches_raise_notices() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let mut troubled = account(now);
    troubled.notices = Some(MamNotices {
        about_to_drop_client: windlass::core::NoticeCount::new(1),
        ..MamNotices::default()
    });
    troubled.points = Points(10_000);
    if let Some(summary) = &mut troubled.summary {
        summary.unsatisfied = UnsatisfiedCount::new(130);
        summary.unsatisfied_not_seeding = TorrentCount::new(2);
        summary.partial = Partials::Seen;
        summary.tracker_errors = TorrentCount::new(1);
        summary.red = windlass::core::CategoryName::try_new("Duplicate peer entries")
            .map(|name| windlass::core::RedCategory {
                name,
                kind: windlass::core::RedKind::Other,
                count: TorrentCount::new(1),
            })
            .into_iter()
            .collect();
    }
    troubled.uploaded = ByteSize(GIB);
    troubled.downloaded = ByteSize(GIB);
    let _ = listed_with(&mut harness, troubled, Vec::new());
    let notices = harness.notices();
    for expected in [
        Notice::RatioLow {
            ratio: 1000,
            urgent: true,
        },
        Notice::UnsatisfiedNearLimit {
            count: UnsatisfiedCount::new(130),
            limit: UnsatisfiedLimit::new(150),
        },
        Notice::MamFlagged {
            partial: Partials::Seen,
            tracker_errors: TorrentCount::new(1),
            flagged: TorrentCount::new(1),
        },
        Notice::MamMessages {
            count: windlass::core::NoticeCount::new(1),
            client_warning: true,
        },
        Notice::PointsBelowReserve {
            points: Points(10_000),
            reserve: Points(20_000),
        },
    ] {
        assert!(notices.contains(&expected), "{expected:?} in {notices:?}");
    }
    assert!(
        harness
            .core
            .notifications()
            .conditions
            .contains_key(&windlass::core::Condition::UnsatisfiedNotSeeding)
    );
}

fn queued_row(id: u64, mam: u64) -> TorrentRecord {
    TorrentRecord {
        id: windlass::core::RecordId::new(id).unwrap_or_else(|| unreachable!("non-zero")),
        identity: windlass::core::Identity::Mam(torrent_id(mam)),
        origin: windlass::core::Origin::Bookmark,
        desired: Desired::Queued,
        save_path: None,
        deletion_allowed: false,
        observation: None,
    }
}

/// Register and read the account from a start with these settings; the
/// actions of the last step.
fn read_from(setup: Setup) -> (Harness, Vec<Action>) {
    let (mut harness, actions) = Harness::start(setup);
    let actions = harness.register(&actions);
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let actions = harness.answer(read(account(harness.wall())));
    (harness, actions)
}

#[test]
fn stray_results_change_nothing() {
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    // Saves that were never asked for leave the outstanding lookup alone.
    for event in [
        MamEvent::RegistrationAttemptSaved,
        MamEvent::RegistrationAttemptSaveFailed,
    ] {
        let actions = harness.send(Event::Mam(event));
        assert!(requests(&actions).is_empty());
    }
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. }))
    );
    // An answer while the attempt is being saved is not the attempt's.
    let actions = harness.answer(Err(MamFailure::Unreachable));
    assert!(requests(&actions).is_empty());
    let actions = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
    assert_eq!(requests(&actions), [MamRequest::RegisterSeedbox]);
}

#[test]
fn a_pause_while_saving_an_attempt_keeps_it_unsent_but_counted() {
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::SaveRegistrationAttempt { .. }))
    );
    let _ = harness.send(Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    }));
    let actions = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
    assert!(requests(&actions).is_empty());
    assert!(matches!(
        harness.mam().seedbox.allowance(harness.wall()),
        windlass::core::RegistrationAllowance::After(_)
    ));
}

#[test]
fn failed_readings_listings_and_lookups_are_retried_later() {
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let started = harness.start + harness.elapsed;
    let actions = harness.answer(Err(MamFailure::Unreachable));
    assert_eq!(
        harness.mam().network.next_lookup,
        Some(started + windlass::core::OBSERVE_EVERY)
    );
    // Nothing that needs the session is sent before MAM shows which IP it sees.
    assert!(requests(&actions).is_empty(), "{actions:?}");
    harness.advance(windlass::core::OBSERVE_EVERY);
    let actions = harness.send(Event::Mam(MamEvent::ReminderSaved));
    let actions = harness.register(&actions);
    let started = harness.start + harness.elapsed;
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let actions = harness.answer(Err(MamFailure::TooSoon {
        retry_after: Some(Duration::from_mins(9)),
    }));
    assert_eq!(
        harness.mam().account.next_read,
        Some(started + Duration::from_mins(9))
    );
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(Err(MamFailure::TooSoon {
        retry_after: Some(Duration::from_mins(30)),
    }));
    let bookmarks = &harness.mam().acquisition.bookmarks;
    assert_eq!(bookmarks.failures, 1);
    assert_eq!(bookmarks.next, Some(started + Duration::from_mins(30)));
    harness.advance(Duration::from_mins(30));
    let actions = harness.send(Event::Mam(MamEvent::ReminderSaved));
    let mut actions = harness.past_lookups(actions);
    // Register and read the account, which come first, until bookmarks are listed.
    while requests(&actions) != [MamRequest::ListBookmarks { start: 0 }] {
        actions = match requests(&actions).first() {
            Some(MamRequest::RegisterSeedbox) => harness.answer(Ok(MamReply::Registered {
                registration: Registration::Completed,
                ip: ip(10),
            })),
            Some(MamRequest::LoadUserData { .. }) => harness.answer(read(account(harness.wall()))),
            _ => harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved)),
        };
    }
    let _ = harness.answer(Err(MamFailure::Unreachable));
    let later = harness.start + harness.elapsed + Duration::from_mins(5);
    assert_eq!(harness.mam().acquisition.bookmarks.next, Some(later));
}

#[test]
fn upload_credit_restores_a_ratio_below_three() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let mut middling = account(harness.wall());
    middling.uploaded = ByteSize(60 * GIB);
    middling.downloaded = ByteSize(30 * GIB);
    let actions = harness.answer(read(middling));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::Buy(Purchase::Upload(_)))
    ));
}

#[test]
fn a_failed_reading_leaves_queued_bookmarks_waiting_for_the_account() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let actions = harness.answer(Err(MamFailure::Unreachable));
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(vec![bookmark(8)]));
    assert_eq!(harness.mam().acquisition.hold, Some(Hold::AccountUnknown));
}

#[test]
fn downloads_wait_for_the_seedbox_the_client_and_the_bookmark() {
    // Registration failed, so the seedbox is not confirmed.
    let (mut harness, actions) = Harness::start(Setup::default());
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let _ = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    let _ = harness.send(Event::Mam(MamEvent::RegistrationAttemptSaved));
    let actions = harness.answer(Err(MamFailure::Unreachable));
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    let _ = harness.answer(read(account(harness.wall())));
    let _ = harness.answer(listed(vec![bookmark(9)]));
    assert_eq!(
        harness.mam().acquisition.hold,
        Some(Hold::SeedboxUnconfirmed)
    );
    assert!(harness.notices().contains(&Notice::SeedboxNotRegistered));
    let _ = harness.send(Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    }));
    assert_eq!(harness.mam().acquisition.hold, Some(Hold::MamUnavailable));

    // qBittorrent is not connected.
    let (mut harness, actions) = read_from(Setup {
        client: false,
        ..Setup::default()
    });
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(vec![bookmark(9)]));
    assert_eq!(
        harness.mam().acquisition.hold,
        Some(Hold::ClientUnavailable)
    );

    // A queued torrent the owner no longer bookmarks waits.
    let (mut harness, actions) = read_from(Setup {
        rows: Some(vec![queued_row(1, 50)]),
        ..Setup::default()
    });
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let _ = harness.answer(listed(Vec::new()));
    assert_eq!(harness.mam().acquisition.hold, Some(Hold::NotBookmarked));
}

#[test]
fn without_the_registry_nothing_is_queued() {
    let (mut harness, actions) = read_from(Setup {
        rows: None,
        ..Setup::default()
    });
    assert_eq!(requests(&actions), [MamRequest::ListBookmarks { start: 0 }]);
    let actions = harness.answer(listed(vec![bookmark(10)]));
    assert!(requests(&actions).is_empty());
    assert!(harness.mam().acquisition.bookmarks.latest.is_some());
}

#[test]
fn a_fetched_torrent_already_in_qbittorrent_keeps_its_row() {
    let adopted = TorrentRecord {
        id: windlass::core::RecordId::new(1).unwrap_or_else(|| unreachable!("non-zero")),
        identity: windlass::core::Identity::Client(hash('e')),
        origin: windlass::core::Origin::Adopted,
        desired: Desired::Active,
        save_path: None,
        deletion_allowed: false,
        observation: None,
    };
    let (mut harness, actions) = read_from(Setup {
        rows: Some(vec![adopted]),
        ..Setup::default()
    });
    assert_eq!(requests(&actions).len(), 1);
    // The same bookmark appears twice across shifting pages.
    let actions = harness.answer(listed(vec![bookmark(11), bookmark(11)]));
    let actions = harness.checked(actions, &[bookmark(11)]);
    assert_eq!(
        requests(&actions),
        [MamRequest::FetchTorrent {
            id: torrent_id(11),
            wedge: true
        }]
    );
    let _ = harness.answer(Ok(MamReply::Fetched(FetchedTorrent {
        hash: hash('e'),
        size: ByteSize(GIB),
    })));
    let Registry::Loaded(records) = harness.core.registry() else {
        unreachable!("loaded")
    };
    let rows: Vec<(windlass::core::Identity, Desired)> = records
        .records()
        .map(|record| (record.identity.clone(), record.desired))
        .collect();
    assert_eq!(
        rows,
        [
            (
                windlass::core::Identity::Linked {
                    mam: torrent_id(11),
                    hash: hash('e')
                },
                Desired::Active
            ),
            (
                windlass::core::Identity::Mam(torrent_id(11)),
                Desired::Removed
            ),
        ]
    );
}

#[test]
fn an_ip_mismatch_lifts_when_mam_sees_another_ip() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    let actions = harness.answer(Err(MamFailure::Session(SessionRejection::IpMismatch)));
    assert!(requests(&actions).is_empty());
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(10, 209_103))));
    assert!(requests(&actions).is_empty());
    let actions = harness.wake(&actions);
    assert_eq!(requests(&actions), [MamRequest::LookUpIp]);
    let actions = harness.answer(Ok(MamReply::Ip(observation(13, 209_103))));
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
}

#[test]
fn unanswered_and_unexpected_results_are_retried() {
    let mut harness = empty_harness();
    let now = harness.wall();
    let actions = listed_with(&mut harness, account(now), vec![bookmark(12)]);
    assert_eq!(requests(&actions).len(), 1);
    // A fetch that failed in transit is tried again with the next reading.
    let _ = harness.answer(Err(MamFailure::Unreachable));
    assert!(harness.mam().acquisition.fetching.is_none());
    assert_eq!(
        harness.record(12).map(|record| record.desired),
        Some(Desired::Queued)
    );
    let mut ending = account(harness.wall());
    ending.vip_until = harness.wall().checked_add(DAY);
    let actions = harness.later(Duration::from_mins(1));
    assert!(matches!(
        requests(&actions).first(),
        Some(MamRequest::LoadUserData { .. })
    ));
    let actions = harness.answer(read(ending));
    assert_eq!(requests(&actions), [MamRequest::Buy(Purchase::Vip)]);
    // MAM asking to wait refuses the purchase without the owner being told.
    let _ = harness.answer(Err(MamFailure::TooSoon { retry_after: None }));
    assert!(matches!(
        harness
            .mam()
            .account
            .last_purchase
            .as_ref()
            .and_then(|purchase| purchase.outcome.clone()),
        Some(windlass::core::PurchaseOutcome::Refused(None))
    ));
}

#[test]
fn the_points_reserve_is_a_saved_setting() {
    let (mut harness, _) = Harness::start(Setup::default());
    let reserve = windlass::core::PointsReserve::try_new(30_000)
        .unwrap_or_else(|_| unreachable!("valid reserve"));
    let actions = harness.send(Event::Settings(SettingsEvent::SetPointsReserve { reserve }));
    assert!(actions.contains(&Action::WritePointsReserve { reserve }));
    let _ = harness.send(Event::Settings(SettingsEvent::PointsReserveWriteFailed));
    assert!(harness.notices().contains(&Notice::SettingNotSaved {
        setting: windlass::core::SettingName::PointsReserve
    }));
    let actions = harness.send(Event::Settings(SettingsEvent::SetPointsReserve { reserve }));
    assert!(actions.contains(&Action::WritePointsReserve { reserve }));
    let _ = harness.send(Event::Settings(SettingsEvent::PointsReserveWritten));
    assert_eq!(
        harness.core.settings().points_reserve(),
        Some(Points(30_000))
    );
}

#[test]
fn a_cookie_change_by_the_owner_wins_over_one_mam_issues() {
    let (mut harness, actions) = Harness::start(Setup::default());
    let actions = harness.register(&actions);
    assert_eq!(requests(&actions).len(), 1);
    // The owner's change is pending, so MAM's new cookie is not saved.
    let _ = harness.send(Event::Settings(SettingsEvent::SetApiCookie {
        cookie: cookie("owner-cookie"),
    }));
    let actions = harness.send(Event::Mam(MamEvent::Answered {
        result: read(account(harness.wall())),
        rotated: Some(cookie("rotated-cookie")),
    }));
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::WriteRotatedApiCookie { .. }))
    );
    // The reading answered meanwhile belonged to the old session, so the
    // account is read again with the owner's cookie.
    let actions = harness.send(Event::Settings(SettingsEvent::ApiCookieWritten));
    assert_eq!(
        requests(&actions),
        [MamRequest::LoadUserData { client_stats: true }]
    );
    assert_eq!(cookie_sent(&actions), Some(cookie("owner-cookie")));

    // A rotation being saved when the owner changes the cookie: the owner's
    // cookie is saved next.
    let actions = harness.send(Event::Mam(MamEvent::Answered {
        result: read(account(harness.wall())),
        rotated: Some(cookie("rotated-again")),
    }));
    assert!(actions.contains(&Action::WriteRotatedApiCookie {
        cookie: cookie("rotated-again")
    }));
    let _ = harness.send(Event::Settings(SettingsEvent::SetApiCookie {
        cookie: cookie("owner-again"),
    }));
    let actions = harness.send(Event::Settings(SettingsEvent::RotatedApiCookieWriteFailed));
    assert!(actions.contains(&Action::WriteApiCookie {
        cookie: Some(cookie("owner-again"))
    }));
}

#[test]
fn torrents_behind_on_seeding_are_reported() {
    let completed = SystemTime::UNIX_EPOCH + EPOCH - 21 * DAY;
    let mut row = queued_row(1, 70);
    row.identity = windlass::core::Identity::Linked {
        mam: torrent_id(70),
        hash: hash('f'),
    };
    row.desired = Desired::Active;
    row.observation = Some(windlass::core::Observation {
        torrent: windlass::core::Torrent {
            completed_at: Some(completed),
            seeding_time: Duration::from_hours(10),
            ..windlass::core::Torrent::default()
        },
        last_seen: completed,
    });
    let (harness, _) = Harness::start(Setup {
        rows: Some(vec![row]),
        ..Setup::default()
    });
    assert!(
        harness
            .notices()
            .contains(&Notice::SeedingBehind { count: 1 })
    );
}
