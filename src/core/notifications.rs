//! Decide what the user is told through ntfy, and when.
//!
//! Lasting conditions notify once they have held for their grace period and
//! again when they clear. One-off events notify as they happen. Notices wait in
//! a bounded outbox and are delivered one at a time, with retries.

use std::{
    collections::{BTreeMap, VecDeque},
    net::IpAddr,
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use super::{
    Action, ClockReading, Desired, EntryCount, Failover, Hold, Mam, MamSession, Points, Port,
    PortForwarding, Protection, Purchase, Qbittorrent, QbittorrentFailure, Registry, SeedboxStatus,
    SessionRejection, SettingsState, StartupMode, SwitchAllowance, TimerId, TimerName,
    TorrentState, Tunnel, VpnServer, timer::Timers,
};

/// A torrent completed this long ago must have seeded 72 hours by now, or soon
/// will not within MAM's 30 days.
const SEEDING_DEADLINE_WARNING: Duration = Duration::from_hours(20 * 24);
/// MAM requires 72 hours of seeding.
const SEEDING_REQUIRED: Duration = Duration::from_hours(72);

/// How many notices wait for delivery before the oldest are dropped.
pub const OUTBOX_LIMIT: usize = 50;
/// The first retry delay after a failed delivery; it doubles up to the maximum.
const RETRY_FIRST: Duration = Duration::from_secs(30);
/// The longest retry delay.
const RETRY_MAX: Duration = Duration::from_mins(10);

/// A setting the user changed that may fail to save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SettingName {
    /// Whether Windlass's MAM requests are paused.
    MamRequests,
    /// The history size limit.
    HistoryLimit,
    /// The MAM API cookie.
    ApiCookie,
    /// The ntfy topic.
    NtfyTopic,
    /// The bonus points reserve.
    PointsReserve,
}

/// Why an automatic VPN server switch cannot start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SwitchBlock {
    /// MAM allows one IP change an hour; the next is allowed at this time.
    Until(SystemTime),
    /// The last switch could not be read.
    LastSwitchUnknown,
    /// No other server is configured.
    NoOtherServer,
}

/// Something the user is told. Notices name conditions and counts, never book
/// titles or account details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Notice {
    /// Windlass started.
    Started {
        /// Whether storage was created or resumed.
        mode: StartupMode,
    },
    /// Startup failed; Windlass does nothing until restarted.
    StartupFailed,
    /// Tunnel checks keep failing.
    VpnProtectionLost {
        /// Consecutive failed checks.
        failures: u32,
    },
    /// A tunnel check succeeded again.
    VpnProtectionRestored {
        /// The tunnel's public address.
        exit_ip: IpAddr,
        /// The connected server, if known.
        server: Option<VpnServer>,
    },
    /// Windlass started switching to another server.
    FailoverStarted {
        /// The server switched to.
        server: VpnServer,
    },
    /// The switch completed.
    FailoverSucceeded {
        /// The server now connected.
        server: VpnServer,
    },
    /// The switch failed.
    FailoverFailed {
        /// The server Windlass tried to switch to.
        server: VpnServer,
    },
    /// A switch is needed but cannot start.
    FailoverBlocked {
        /// Why not.
        reason: SwitchBlock,
    },
    /// No forwarded port, so peers cannot connect in.
    ForwardedPortLost,
    /// A forwarded port was granted again.
    ForwardedPortRestored {
        /// The port.
        port: Port,
    },
    /// qBittorrent does not answer.
    QbittorrentUnreachable {
        /// Why the last request failed.
        reason: QbittorrentFailure,
    },
    /// qBittorrent answers again.
    QbittorrentReachable,
    /// MAM does not allow the running qBittorrent version.
    QbittorrentVersionNotAllowed {
        /// The version.
        version: String,
    },
    /// The running qBittorrent version is allowed again.
    QbittorrentVersionAllowed {
        /// The version.
        version: String,
    },
    /// qBittorrent reports errors for some torrents.
    TorrentErrors {
        /// How many torrents.
        count: usize,
    },
    /// No torrent reports an error.
    TorrentErrorsCleared,
    /// Some torrents' files are missing on disk.
    FilesMissing {
        /// How many torrents.
        count: usize,
    },
    /// No torrent's files are missing.
    FilesMissingCleared,
    /// Some torrents Windlass wants are missing from qBittorrent.
    TorrentsMissing {
        /// How many torrents.
        count: usize,
    },
    /// Every torrent Windlass wants is in qBittorrent.
    TorrentsMissingCleared,
    /// The torrent registry keeps failing to save.
    RegistryNotSaved,
    /// The torrent registry saved again.
    RegistrySaved,
    /// The torrent registry could not be read, so torrents are not managed.
    RegistryUnreadable,
    /// A setting the user changed could not be saved.
    SettingNotSaved {
        /// Which setting.
        setting: SettingName,
    },
    /// History storage could not be opened.
    HistoryUnavailable,
    /// History entries could not be saved.
    HistoryEntriesLost {
        /// How many.
        count: EntryCount,
    },
    /// The user asked for a test notification.
    Test,
    /// MAM refused the API session.
    MamSessionRejected {
        /// Why.
        rejection: SessionRejection,
    },
    /// MAM accepted the API session again.
    MamSessionAccepted,
    /// MAM has not registered the seedbox IP.
    SeedboxNotRegistered,
    /// MAM registered the seedbox IP.
    SeedboxRegistered {
        /// The registered IP.
        ip: IpAddr,
    },
    /// Windlass cannot learn which IP MAM sees.
    MamIpUnknown {
        /// Failed lookups in a row.
        failures: u32,
    },
    /// Windlass learns which IP MAM sees again.
    MamIpKnown,
    /// Saved MAM state could not be read, so registration waits.
    MamStateUnreadable,
    /// The ratio is below 2.0, so downloads are held.
    RatioLow {
        /// The ratio in thousandths.
        ratio: u64,
        /// Whether it is below 1.1.
        urgent: bool,
    },
    /// The ratio is 2.0 or more again.
    RatioRestored,
    /// Unsatisfied torrents reached 80% of MAM's limit.
    UnsatisfiedNearLimit {
        /// MAM's count.
        count: u32,
        /// MAM's limit.
        limit: u32,
    },
    /// Unsatisfied torrents are below 80% of the limit again.
    UnsatisfiedBelowLimit,
    /// MAM counts unsatisfied torrents that are not seeding.
    UnsatisfiedNotSeeding {
        /// How many.
        count: u32,
    },
    /// Every unsatisfied torrent seeds again.
    UnsatisfiedSeeding,
    /// MAM flags a partial download, tracker errors, or other red categories.
    MamFlagged {
        /// Whether MAM sees a partial download.
        partial: bool,
        /// Important tracker errors.
        tracker_errors: u32,
        /// Torrents in other red categories, such as duplicate peers.
        flagged: u32,
    },
    /// MAM flags nothing any more.
    MamFlagsCleared,
    /// MAM has notifications for the owner; their text is not read.
    MamMessages {
        /// How many.
        count: u32,
        /// Whether one warns that MAM is about to stop accepting a client.
        client_warning: bool,
    },
    /// Bookmarked torrents wait.
    DownloadsHeld {
        /// Why.
        hold: Hold,
    },
    /// Bookmarked torrents download again.
    DownloadsResumed,
    /// Torrents wait for a wedge and none is left.
    NoWedgesLeft,
    /// The points balance is below the reserve.
    PointsBelowReserve {
        /// The balance.
        points: Points,
        /// The reserve.
        reserve: Points,
    },
    /// VIP ends within 7 days and has not been renewed.
    VipEnding {
        /// When it ends.
        until: SystemTime,
    },
    /// MAM refused a download, so downloads are held.
    DownloadsRefused,
    /// Torrents completed 20 or more days ago have not seeded 72 hours.
    SeedingBehind {
        /// How many.
        count: usize,
    },
    /// Every torrent is on track to seed 72 hours.
    SeedingOnTrack,
    /// A bonus store purchase went through.
    PurchaseMade {
        /// What was bought.
        purchase: Purchase,
        /// The points left.
        points: Points,
    },
    /// MAM refused a purchase.
    PurchaseRefused {
        /// What Windlass tried to buy.
        purchase: Purchase,
    },
    /// A purchase's outcome is unknown; nothing more is bought until MAM's
    /// figures are read again.
    PurchaseUncertain {
        /// What Windlass tried to buy.
        purchase: Purchase,
    },
    /// A download's wedge was not spent, so the torrent is stopped.
    WedgeNotSpent,
    /// A weekly reminder to visit MAM.
    MamVisitReminder,
}

/// Why a delivery failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DeliveryFailure {
    /// No connection, or no answer in time.
    Unreachable,
    /// ntfy answered with an unexpected HTTP status.
    Rejected {
        /// The HTTP status code.
        status: u16,
    },
}

/// A notification command or a delivery result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum NotificationEvent {
    /// The user asked for a test notification.
    SendTest,
    /// ntfy accepted the notice being sent.
    Delivered,
    /// Sending the notice failed.
    DeliveryFailed {
        /// Why.
        reason: DeliveryFailure,
    },
}

/// A lasting condition worth telling the user about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Condition {
    /// Tunnel checks keep failing.
    VpnProtection,
    /// A needed server switch cannot start.
    FailoverBlocked,
    /// No forwarded port.
    ForwardedPort,
    /// qBittorrent does not answer.
    QbittorrentReachable,
    /// MAM does not allow the qBittorrent version.
    QbittorrentVersion,
    /// Torrents in an error state.
    TorrentErrors,
    /// Torrents whose files are missing.
    FilesMissing,
    /// Wanted torrents missing from qBittorrent.
    TorrentsMissing,
    /// The registry keeps failing to save.
    RegistrySave,
    /// MAM refuses the API session.
    MamSession,
    /// The seedbox IP is not registered.
    SeedboxRegistration,
    /// The IP MAM sees is unknown.
    MamIp,
    /// The ratio is low.
    Ratio,
    /// Unsatisfied torrents near the limit.
    UnsatisfiedLimit,
    /// Unsatisfied torrents not seeding.
    UnsatisfiedNotSeeding,
    /// MAM flags something.
    MamFlags,
    /// MAM has notifications.
    MamMessages,
    /// Downloads wait.
    DownloadsHeld,
    /// No wedge is left for waiting torrents.
    Wedges,
    /// Points are below the reserve.
    PointsReserve,
    /// VIP ends soon.
    VipEnding,
    /// MAM refused a download.
    DownloadsRefused,
    /// Torrents behind on seeding.
    SeedingBehind,
}

impl Condition {
    /// How long the condition must hold before the user is told.
    #[must_use]
    pub const fn grace(self) -> Duration {
        match self {
            Self::VpnProtection
            | Self::FailoverBlocked
            | Self::QbittorrentVersion
            | Self::MamSession
            | Self::SeedboxRegistration
            | Self::Ratio
            | Self::UnsatisfiedLimit
            | Self::MamFlags
            | Self::MamMessages
            | Self::Wedges
            | Self::PointsReserve
            | Self::VipEnding
            | Self::DownloadsRefused
            | Self::SeedingBehind => Duration::ZERO,
            Self::MamIp => Duration::from_mins(15),
            Self::UnsatisfiedNotSeeding | Self::DownloadsHeld => Duration::from_mins(30),
            Self::RegistrySave => Duration::from_mins(2),
            Self::ForwardedPort | Self::QbittorrentReachable | Self::TorrentErrors => {
                Duration::from_mins(5)
            }
            Self::FilesMissing | Self::TorrentsMissing => Duration::from_mins(10),
        }
    }
}

/// A condition's state in this step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// It holds; this notice describes it.
    Active(Notice),
    /// It does not hold; tell the user with this notice if they were told it did.
    Clear(Option<Notice>),
    /// Nothing new is known.
    Unchanged,
}

/// How far a condition has progressed toward notifying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Holding, but within its grace period.
    Pending {
        /// When it started holding.
        since: Instant,
    },
    /// The user was told.
    Raised,
}

/// The notice being delivered, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Delivery {
    /// Nothing is being sent.
    #[default]
    Idle,
    /// The shell is sending this notice.
    Sending(Notice),
    /// Sending failed; it is sent again when the timer expires.
    Retrying {
        /// The notice.
        notice: Notice,
        /// The retry timer.
        timer: TimerId,
    },
}

/// The result of the latest delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// When it finished.
    pub at: SystemTime,
    /// Whether ntfy accepted it, or why not.
    pub result: Result<(), DeliveryFailure>,
}

/// What the core is telling the user, and how delivery is going.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notifications {
    /// Conditions holding now, by how far they have progressed.
    pub conditions: BTreeMap<Condition, Phase>,
    /// Notices waiting to be sent, oldest first.
    pub outbox: VecDeque<Notice>,
    /// The notice being sent.
    pub delivery: Delivery,
    /// The latest delivery result.
    pub last_outcome: Option<Outcome>,
    /// Notices dropped because the outbox was full.
    pub dropped: u64,
    /// Deliveries that failed since the last success.
    pub failures: u32,
    grace_timer: Option<(TimerId, Instant)>,
    mode: Option<StartupMode>,
}

/// The parts of core state that one-off notices compare across a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Glance {
    pub(super) failover: Option<Failover>,
    pub(super) current: Option<VpnServer>,
    pub(super) failed: bool,
    pub(super) ready: bool,
}

impl Glance {
    pub(super) fn of(tunnel: &Tunnel, failed: bool, ready: bool) -> Self {
        Self {
            failover: tunnel
                .watch
                .as_ref()
                .and_then(|watch| watch.failover.clone()),
            current: tunnel.current().cloned(),
            failed,
            ready,
        }
    }
}

/// The core state that lasting conditions are read from.
#[derive(Debug, Clone, Copy)]
pub(super) struct Facts<'a> {
    pub(super) tunnel: &'a Tunnel,
    pub(super) port_forwarding: &'a PortForwarding,
    pub(super) qbittorrent: &'a Qbittorrent,
    pub(super) registry: &'a Registry,
    pub(super) mam: &'a Mam,
    pub(super) reserve: Option<Points>,
}

impl Notifications {
    pub(super) const fn new() -> Self {
        Self {
            conditions: BTreeMap::new(),
            outbox: VecDeque::new(),
            delivery: Delivery::Idle,
            last_outcome: None,
            dropped: 0,
            failures: 0,
            grace_timer: None,
            mode: None,
        }
    }

    /// Notices for what an event reports, taken before it is handled.
    pub(super) fn reported(&mut self, event: &super::Event) -> Vec<Notice> {
        use super::{
            Event, HistoryEvent, MamEvent, MamPauseEvent, RegistryEvent, SettingsEvent,
            StartupEvent,
        };

        let notice = match event {
            Event::Startup(StartupEvent::Start { mode }) => {
                self.mode = Some(*mode);
                None
            }
            Event::MamPause(MamPauseEvent::WriteFailed | MamPauseEvent::DeleteFailed) => {
                Some(Notice::SettingNotSaved {
                    setting: SettingName::MamRequests,
                })
            }
            Event::Settings(SettingsEvent::HistoryLimitWriteFailed) => {
                Some(Notice::SettingNotSaved {
                    setting: SettingName::HistoryLimit,
                })
            }
            Event::Settings(SettingsEvent::ApiCookieWriteFailed) => Some(Notice::SettingNotSaved {
                setting: SettingName::ApiCookie,
            }),
            Event::Settings(SettingsEvent::NtfyTopicWriteFailed) => Some(Notice::SettingNotSaved {
                setting: SettingName::NtfyTopic,
            }),
            Event::Settings(SettingsEvent::PointsReserveWriteFailed) => {
                Some(Notice::SettingNotSaved {
                    setting: SettingName::PointsReserve,
                })
            }
            Event::Mam(MamEvent::StateReadFailed) => Some(Notice::MamStateUnreadable),
            Event::History(HistoryEvent::OpenFailed) => Some(Notice::HistoryUnavailable),
            Event::History(HistoryEvent::EntriesLost { count, .. }) => {
                Some(Notice::HistoryEntriesLost { count: *count })
            }
            Event::Registry(RegistryEvent::ReadFailed) => Some(Notice::RegistryUnreadable),
            Event::Notification(NotificationEvent::SendTest) => Some(Notice::Test),
            _ => None,
        };
        notice.into_iter().collect()
    }

    /// Notices for what changed in this step.
    pub(super) fn changed(&self, before: &Glance, after: &Glance) -> Vec<Notice> {
        let mut notices = Vec::new();
        if after.ready
            && !before.ready
            && let Some(mode) = self.mode
        {
            notices.push(Notice::Started { mode });
        }
        if after.failed && !before.failed {
            notices.push(Notice::StartupFailed);
        }
        if let (None, Some(Failover::Saving(switch))) = (&before.failover, &after.failover) {
            notices.push(Notice::FailoverStarted {
                server: switch.server.clone(),
            });
        }
        let ended = match (&before.failover, &after.failover) {
            (Some(Failover::Saving(switch)), None) => Some(&switch.server),
            (Some(Failover::Switching(server)), None) => Some(server),
            _ => None,
        };
        if let Some(server) = ended {
            let server = server.clone();
            notices.push(if after.current.as_ref() == Some(&server) {
                Notice::FailoverSucceeded { server }
            } else {
                Notice::FailoverFailed { server }
            });
        }
        notices
    }

    /// Track lasting conditions, queue notices, and start the next delivery.
    pub(super) fn after_step(
        &mut self,
        now: ClockReading,
        happened: Vec<Notice>,
        facts: Facts<'_>,
        settings: &SettingsState,
        timers: &mut Timers,
    ) -> Vec<Action> {
        for (condition, status) in assess(now.wall, facts) {
            self.track(now.monotonic, condition, status);
        }
        for notice in happened {
            self.enqueue(notice);
        }
        let mut actions = self.schedule_grace(now.monotonic, timers);
        actions.extend(self.send(settings));
        actions
    }

    pub(super) fn handle(
        &mut self,
        now: ClockReading,
        event: &NotificationEvent,
        timers: &mut Timers,
    ) -> Vec<Action> {
        let Delivery::Sending(notice) = &self.delivery else {
            return Vec::new();
        };
        match event {
            NotificationEvent::SendTest => Vec::new(),
            NotificationEvent::Delivered => {
                self.delivery = Delivery::Idle;
                self.failures = 0;
                self.last_outcome = Some(Outcome {
                    at: now.wall,
                    result: Ok(()),
                });
                Vec::new()
            }
            NotificationEvent::DeliveryFailed { reason } => {
                let notice = notice.clone();
                self.failures = self.failures.saturating_add(1);
                self.last_outcome = Some(Outcome {
                    at: now.wall,
                    result: Err(*reason),
                });
                // Waits of 30 seconds, then 1, 2, 4, and 8 minutes, then 10.
                let after = RETRY_FIRST
                    .saturating_mul(2_u32.saturating_pow(self.failures - 1))
                    .min(RETRY_MAX);
                let (timer, deadline) = timers.schedule(now.monotonic, after);
                self.delivery = Delivery::Retrying { notice, timer };
                vec![Action::StartTimer {
                    name: TimerName::NotificationRetry,
                    id: timer,
                    deadline,
                }]
            }
        }
    }

    /// A failed delivery whose retry is due is sent again.
    pub(super) fn timer_expired(&mut self, id: TimerId) {
        if self.grace_timer.is_some_and(|(timer, _)| timer == id) {
            self.grace_timer = None;
        }
        if let Delivery::Retrying { notice, timer, .. } = &self.delivery
            && *timer == id
        {
            self.outbox.push_front(notice.clone());
            self.delivery = Delivery::Idle;
        }
    }

    fn enqueue(&mut self, notice: Notice) {
        if self.outbox.len() >= OUTBOX_LIMIT {
            self.outbox.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.outbox.push_back(notice);
    }

    fn track(&mut self, now: Instant, condition: Condition, status: Status) {
        match (self.conditions.get(&condition).copied(), status) {
            (None, Status::Active(notice)) => {
                if condition.grace().is_zero() {
                    self.conditions.insert(condition, Phase::Raised);
                    self.enqueue(notice);
                } else {
                    self.conditions
                        .insert(condition, Phase::Pending { since: now });
                }
            }
            (Some(Phase::Pending { since }), Status::Active(notice))
                if now.duration_since(since) >= condition.grace() =>
            {
                self.conditions.insert(condition, Phase::Raised);
                self.enqueue(notice);
            }
            (Some(Phase::Pending { .. }), Status::Clear(_)) => {
                self.conditions.remove(&condition);
            }
            (Some(Phase::Raised), Status::Clear(notice)) => {
                self.conditions.remove(&condition);
                if let Some(notice) = notice {
                    self.enqueue(notice);
                }
            }
            _ => {}
        }
    }

    /// Wake the core when the earliest pending condition's grace ends.
    fn schedule_grace(&mut self, now: Instant, timers: &mut Timers) -> Vec<Action> {
        let due = self
            .conditions
            .iter()
            .filter_map(|(condition, phase)| match phase {
                Phase::Pending { since } => since.checked_add(condition.grace()),
                Phase::Raised => None,
            })
            .min();
        let Some(due) = due else {
            return Vec::new();
        };
        if self.grace_timer.is_some_and(|(_, at)| at == due) {
            return Vec::new();
        }
        let (id, deadline) = timers.schedule(now, due.saturating_duration_since(now));
        self.grace_timer = Some((id, due));
        vec![Action::StartTimer {
            name: TimerName::NotificationGrace,
            id,
            deadline,
        }]
    }

    /// Send the oldest notice once settings name a topic; without one, notices
    /// are discarded.
    fn send(&mut self, settings: &SettingsState) -> Vec<Action> {
        let topic = match settings {
            SettingsState::Unread | SettingsState::Reading => return Vec::new(),
            SettingsState::ReadFailed => None,
            SettingsState::Loaded(settings) => settings.ntfy_topic.saved.clone(),
        };
        let Some(topic) = topic else {
            self.outbox.clear();
            if matches!(self.delivery, Delivery::Retrying { .. }) {
                self.delivery = Delivery::Idle;
            }
            return Vec::new();
        };
        if self.delivery != Delivery::Idle {
            return Vec::new();
        }
        let Some(notice) = self.outbox.pop_front() else {
            return Vec::new();
        };
        self.delivery = Delivery::Sending(notice.clone());
        vec![Action::SendNotification { topic, notice }]
    }
}

/// Read every lasting condition from core state.
#[expect(
    clippy::too_many_lines,
    reason = "one entry per condition reads best as one table"
)]
fn assess(now: SystemTime, facts: Facts<'_>) -> Vec<(Condition, Status)> {
    let session = match facts.qbittorrent {
        Qbittorrent::Connected(session) => Some(&**session),
        _ => None,
    };
    let listed = session.filter(|session| session.listed());
    let count = |state: TorrentState| {
        listed.map(|session| {
            session
                .torrents
                .values()
                .filter(|torrent| torrent.state == state)
                .count()
        })
    };
    let missing = match facts.registry {
        Registry::Loaded(records) => listed.map(|session| {
            records
                .records()
                .filter(|record| record.desired == Desired::Active)
                .filter_map(|record| record.identity.hash())
                .filter(|hash| !session.torrents.contains_key(*hash))
                .count()
        }),
        _ => None,
    };
    let counted = |count: Option<usize>, raised: fn(usize) -> Notice, cleared: Notice| match count {
        Some(0) => Status::Clear(Some(cleared)),
        Some(count) => Status::Active(raised(count)),
        None => Status::Unchanged,
    };
    let (vpn, blocked) = vpn(now, facts.tunnel);
    let mut conditions = vec![
        (Condition::VpnProtection, vpn),
        (Condition::FailoverBlocked, blocked),
        (
            Condition::ForwardedPort,
            forwarded_port(facts.port_forwarding, facts.tunnel),
        ),
        (
            Condition::QbittorrentReachable,
            match facts.qbittorrent {
                Qbittorrent::Disabled => Status::Clear(None),
                Qbittorrent::Connecting => Status::Unchanged,
                Qbittorrent::Unreachable { reason, .. } => {
                    Status::Active(Notice::QbittorrentUnreachable { reason: *reason })
                }
                Qbittorrent::Connected(_) => Status::Clear(Some(Notice::QbittorrentReachable)),
            },
        ),
        (
            Condition::QbittorrentVersion,
            session.map_or(Status::Unchanged, |session| {
                let version = session.version.clone();
                if session.approval == super::MamApproval::NotAllowed {
                    Status::Active(Notice::QbittorrentVersionNotAllowed { version })
                } else {
                    Status::Clear(Some(Notice::QbittorrentVersionAllowed { version }))
                }
            }),
        ),
        (
            Condition::TorrentErrors,
            counted(
                count(TorrentState::Error),
                |count| Notice::TorrentErrors { count },
                Notice::TorrentErrorsCleared,
            ),
        ),
        (
            Condition::FilesMissing,
            counted(
                count(TorrentState::MissingFiles),
                |count| Notice::FilesMissing { count },
                Notice::FilesMissingCleared,
            ),
        ),
        (
            Condition::TorrentsMissing,
            counted(
                missing,
                |count| Notice::TorrentsMissing { count },
                Notice::TorrentsMissingCleared,
            ),
        ),
        (
            Condition::RegistrySave,
            match facts.registry {
                Registry::Loaded(records) if records.save_failed() => {
                    Status::Active(Notice::RegistryNotSaved)
                }
                Registry::Loaded(_) => Status::Clear(Some(Notice::RegistrySaved)),
                _ => Status::Unchanged,
            },
        ),
        (
            Condition::SeedingBehind,
            counted(
                seeding_behind(now, facts.registry),
                |count| Notice::SeedingBehind { count },
                Notice::SeedingOnTrack,
            ),
        ),
    ];
    conditions.extend(mam(now, facts.mam, facts.reserve));
    conditions
}

/// Active torrents completed 20 or more days ago with less than 72 hours seeded.
fn seeding_behind(now: SystemTime, registry: &Registry) -> Option<usize> {
    let Registry::Loaded(records) = registry else {
        return None;
    };
    Some(
        records
            .records()
            .filter(|record| record.desired == Desired::Active)
            .filter_map(|record| record.observation.as_ref())
            .filter(|observation| {
                observation.torrent.seeding_time < SEEDING_REQUIRED
                    && observation.torrent.completed_at.is_some_and(|completed| {
                        now.duration_since(completed)
                            .is_ok_and(|since| since >= SEEDING_DEADLINE_WARNING)
                    })
            })
            .count(),
    )
}

/// The account's lasting conditions, as MAM reports them.
#[expect(
    clippy::too_many_lines,
    reason = "one entry per condition reads best as one table"
)]
fn mam(now: SystemTime, mam: &Mam, reserve: Option<Points>) -> Vec<(Condition, Status)> {
    let read = |active: Option<Notice>, cleared: Option<Notice>| match (mam.account.data(), active)
    {
        (None, _) => Status::Unchanged,
        (Some(_), Some(notice)) => Status::Active(notice),
        (Some(_), None) => Status::Clear(cleared),
    };
    let account = &mam.account;
    let hold = mam.acquisition.hold;
    vec![
        (
            Condition::MamSession,
            match mam.session {
                MamSession::Rejected { rejection, .. } => {
                    Status::Active(Notice::MamSessionRejected { rejection })
                }
                MamSession::Verified { .. } => Status::Clear(Some(Notice::MamSessionAccepted)),
                MamSession::Unverified => Status::Unchanged,
            },
        ),
        (
            Condition::SeedboxRegistration,
            match &mam.seedbox.status {
                SeedboxStatus::Failed {
                    failure: super::RegistrationFailure::Mam(super::MamFailure::TooSoon { .. }),
                    ..
                }
                | SeedboxStatus::Unconfirmed => Status::Unchanged,
                SeedboxStatus::Failed { .. } => Status::Active(Notice::SeedboxNotRegistered),
                SeedboxStatus::Confirmed { ip, .. } => {
                    Status::Clear(Some(Notice::SeedboxRegistered { ip: *ip }))
                }
            },
        ),
        (
            Condition::MamIp,
            if mam.network.failures > 0 {
                Status::Active(Notice::MamIpUnknown {
                    failures: mam.network.failures,
                })
            } else if mam.network.current().is_some() {
                Status::Clear(Some(Notice::MamIpKnown))
            } else {
                Status::Unchanged
            },
        ),
        (
            Condition::Ratio,
            read(
                account
                    .ratio_low()
                    .map(|(ratio, urgent)| Notice::RatioLow { ratio, urgent }),
                Some(Notice::RatioRestored),
            ),
        ),
        (
            Condition::UnsatisfiedLimit,
            read(
                account
                    .unsatisfied_near_limit()
                    .map(|(count, limit)| Notice::UnsatisfiedNearLimit { count, limit }),
                Some(Notice::UnsatisfiedBelowLimit),
            ),
        ),
        (
            Condition::UnsatisfiedNotSeeding,
            read(
                account
                    .not_seeding_unsatisfied()
                    .filter(|count| *count > 0)
                    .map(|count| Notice::UnsatisfiedNotSeeding { count }),
                Some(Notice::UnsatisfiedSeeding),
            ),
        ),
        (
            Condition::MamFlags,
            read(
                account
                    .attention()
                    .filter(super::Attention::any)
                    .map(|attention| Notice::MamFlagged {
                        partial: attention.partial,
                        tracker_errors: attention.tracker_errors,
                        flagged: attention.flagged,
                    }),
                Some(Notice::MamFlagsCleared),
            ),
        ),
        (
            Condition::MamMessages,
            read(
                account
                    .data()
                    .and_then(|data| data.notices)
                    .filter(|notices| notices.total() > 0)
                    .map(|notices| Notice::MamMessages {
                        count: notices.total(),
                        client_warning: notices.about_to_drop_client > 0,
                    }),
                None,
            ),
        ),
        (
            Condition::DownloadsHeld,
            match hold {
                Some(Hold::MamUnavailable) => Status::Unchanged,
                Some(hold) => Status::Active(Notice::DownloadsHeld { hold }),
                None => Status::Clear(Some(Notice::DownloadsResumed)),
            },
        ),
        (
            Condition::Wedges,
            if hold == Some(Hold::NoWedge) {
                Status::Active(Notice::NoWedgesLeft)
            } else {
                Status::Clear(None)
            },
        ),
        (
            Condition::PointsReserve,
            read(
                account
                    .data()
                    .zip(reserve)
                    .filter(|(data, reserve)| data.points < *reserve)
                    .map(|(data, reserve)| Notice::PointsBelowReserve {
                        points: data.points,
                        reserve,
                    }),
                None,
            ),
        ),
        (
            Condition::VipEnding,
            read(
                account
                    .vip_ending(now)
                    .map(|until| Notice::VipEnding { until }),
                None,
            ),
        ),
        (
            Condition::DownloadsRefused,
            if mam.acquisition.refused.is_some() {
                Status::Active(Notice::DownloadsRefused)
            } else {
                Status::Clear(None)
            },
        ),
    ]
}

/// VPN protection, which clears only on a successful check, and blocked
/// failover, whose end other notices already announce.
fn vpn(now: SystemTime, tunnel: &Tunnel) -> (Status, Status) {
    let Some(watch) = &tunnel.watch else {
        return (Status::Clear(None), Status::Clear(None));
    };
    let failing = watch.failures >= super::FAILURES_BEFORE_SWITCH;
    let protection = match watch.protection {
        _ if failing => Status::Active(Notice::VpnProtectionLost {
            failures: watch.failures,
        }),
        Protection::Available { exit_ip, .. } => {
            Status::Clear(Some(Notice::VpnProtectionRestored {
                exit_ip,
                server: watch.current.clone(),
            }))
        }
        Protection::Unknown | Protection::Unavailable { .. } => Status::Unchanged,
    };
    let reason = match tunnel.switch_allowance(now) {
        SwitchAllowance::Now => None,
        SwitchAllowance::After(at) => Some(SwitchBlock::Until(at)),
        SwitchAllowance::Unknown => Some(SwitchBlock::LastSwitchUnknown),
        SwitchAllowance::NoAlternative => Some(SwitchBlock::NoOtherServer),
    };
    let blocked = match reason {
        Some(reason) if failing && watch.failover.is_none() => {
            Status::Active(Notice::FailoverBlocked { reason })
        }
        _ => Status::Clear(None),
    };
    (protection, blocked)
}

/// A switch drops the lease on purpose, so a missing port then changes nothing.
fn forwarded_port(forwarding: &PortForwarding, tunnel: &Tunnel) -> Status {
    let switching = tunnel
        .watch
        .as_ref()
        .is_some_and(|watch| watch.failover.is_some());
    match forwarding {
        PortForwarding::Disabled => Status::Clear(None),
        PortForwarding::Enabled {
            lease: Some(lease), ..
        } => Status::Clear(Some(Notice::ForwardedPortRestored { port: lease.tcp })),
        PortForwarding::Enabled { lease: None, .. } if switching => Status::Unchanged,
        PortForwarding::Enabled { lease: None, .. } => Status::Active(Notice::ForwardedPortLost),
    }
}
