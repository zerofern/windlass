//! Windlass's work with MAM: the API session, seedbox registration, the account,
//! bonus point spending, and bookmark acquisition.
//!
//! One request is outstanding at a time. Each step, when nothing is
//! outstanding, the most urgent wanted request is sent if MAM requests are not
//! paused, VPN protection is available, and the shared token bucket allows it.

mod account;
mod acquisition;
mod api;
mod budget;
mod connectability;
mod network;

use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

pub use account::{
    ACCOUNT_FRESH_FOR, ACCOUNT_READ_EVERY, Account, Attention, CLIENT_STATS_EVERY, ClientList,
    ClientStanding, PurchaseOutcome, PurchaseRecord, RATIO_FLOOR, RATIO_TARGET, RATIO_URGENT,
    Reading, SettledBy, UNSATISFIED_RESERVE, UPLOAD_BUFFER, UPLOAD_PRICE_PER_GIB, UPLOAD_STEP,
    VIP_FILL, VIP_PRICE_PER_WEEK, VIP_RENEW_WITHIN, VIP_WARN_WITHIN,
};
pub use acquisition::{
    Acquisition, BOOKMARKS_EVERY, Bookmarks, DownloadWatch, HISTORY_FRESH_FOR, Hold,
    LOOKUP_FRESH_FOR, Listed, Lookup, MAX_BOOKMARK_PAGES, RECENT_ADMISSION, WATCH_FRESH_FOR,
    WEDGE_RECORDED_AFTER, WedgeHistory,
};
pub use api::{
    Asn, BookTitle, BookTitleError, Bookmark, BookmarkCount, BookmarkPage, CategoryName,
    CategoryNameError, ClassName, ClassNameError, ClientRefusal, Connectable, FetchedTorrent,
    Freeleech, IpObservation, ListedClient, MamFailure, MamMessage, MamMessageError, MamNotices,
    MamReply, MamRequest, NetworkName, NetworkNameError, NoticeCount, Partials, PersonalFreeleech,
    Points, Purchase, RedCategory, RedKind, Registration, SessionRejection, SnatchSummary,
    Snatched, TorrentCount, UnsatisfiedCount, UnsatisfiedLimit, UploadCredit, UploadCreditError,
    UserClass, UserData, VipTorrent, WedgeCount, WedgeSpent,
};
pub use budget::{Budget, REQUEST_BURST, REQUEST_REFILL};
pub use connectability::{
    Connectability, Connection, Findings, IpFinding, PortFinding, REPAIR_SETTLES_AFTER,
};
pub use network::{
    LOOKUP_SPACING, LastAttempt, Network, OBSERVE_EVERY, Observed, REGISTRATION_SPACING,
    RegistrationAllowance, RegistrationFailure, Seedbox, SeedboxStatus, Session,
};

use self::acquisition::{Facts, Next};
use super::{
    Action, ApiCookie, ClientVersion, ClockReading, Desired, DownloadingLimit, MamRequestsStatus,
    MamTorrentId, Notice, Records, Registry, TimerId, TimerName, timer::Timers,
};

/// The owner is reminded to visit MAM this often.
pub const REMINDER_EVERY: Duration = Duration::from_hours(7 * 24);

/// A MAM command or result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MamEvent {
    /// Start working with MAM; sent when MAM's hosts are configured.
    Enable,
    /// The outstanding request finished.
    Answered {
        /// MAM's answer, or why there was none.
        result: Result<MamReply, MamFailure>,
        /// A new cookie value MAM issued, if any.
        rotated: Option<ApiCookie>,
    },
    /// Saved MAM state was read.
    StateRead {
        /// The last seedbox registration attempt.
        last_registration: Option<SystemTime>,
        /// The last visit reminder.
        last_reminder: Option<SystemTime>,
    },
    /// Saved MAM state could not be read.
    StateReadFailed,
    /// The registration attempt about to be sent was saved.
    RegistrationAttemptSaved,
    /// Saving the registration attempt failed; it is not sent.
    RegistrationAttemptSaveFailed,
    /// The owner checked on MAM whether an unresolved purchase went through.
    PurchaseChecked {
        /// Whether it did.
        made: bool,
    },
    /// The reminder time was saved.
    ReminderSaved,
    /// Saving the reminder time failed.
    ReminderSaveFailed,
}

/// The request or save outstanding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outstanding {
    /// A registration attempt is being saved before it is sent.
    SavingRegistration {
        /// When the attempt is made.
        at: SystemTime,
        /// The cookie to send it with.
        cookie: ApiCookie,
        /// The connection it will be sent on.
        epoch: u64,
    },
    /// A request was sent.
    Request {
        /// The request.
        request: MamRequest,
        /// When.
        sent: Instant,
        /// The connection it was sent on; a pause, lost protection, or a new
        /// cookie starts another.
        epoch: u64,
    },
}

/// What Windlass knows about the last visit reminder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LastReminder {
    /// Storage is not open yet.
    #[default]
    Unread,
    /// Reading it.
    Reading,
    /// Read: the last reminder, if there was one.
    Known(Option<SystemTime>),
    /// It could not be read; reminders count from now.
    ReadFailed,
}

/// The core state that MAM work depends on.
#[derive(Debug)]
pub(super) struct Context<'a> {
    pub(super) requests: MamRequestsStatus,
    pub(super) protected: bool,
    /// The saved cookie, when no change to it is being saved.
    pub(super) cookie: Option<&'a ApiCookie>,
    /// The cookie the owner last asked for, or one MAM issued being saved.
    pub(super) requested_cookie: Option<&'a ApiCookie>,
    pub(super) reserve: Option<Points>,
    pub(super) client_ready: bool,
    /// The running qBittorrent's version, once connected.
    pub(super) client_version: Option<ClientVersion>,
    /// What the rest of Windlass knows about the connection.
    pub(super) connection: Connection,
    /// How many torrents may download at once.
    pub(super) downloading_limit: DownloadingLimit,
    pub(super) registry: &'a Registry,
}

/// Windlass's work with MAM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mam {
    /// Whether MAM's hosts are configured.
    pub enabled: bool,
    /// Whether requests may be sent: not paused, and VPN protection available.
    pub available: bool,
    /// The API session.
    pub session: Session,
    /// The IP MAM sees.
    pub network: Network,
    /// The dynamic seedbox registration.
    pub seedbox: Seedbox,
    /// The account and spending.
    pub account: Account,
    /// Bookmark acquisition.
    pub acquisition: Acquisition,
    /// The shared request budget.
    pub budget: Budget,
    /// The request or save outstanding.
    pub outstanding: Option<Outstanding>,
    /// The last visit reminder.
    pub last_reminder: LastReminder,
    /// What MAM's client list says about the running qBittorrent.
    pub client: ClientStanding,
    /// Whether MAM can reach qBittorrent, and what Windlass did about it.
    pub connectability: Connectability,
    renew_lease: bool,
    cookie_in_use: Option<ApiCookie>,
    issued_cookie: Option<ApiCookie>,
    epoch: u64,
    wake: Option<(TimerId, Instant)>,
    notices: Vec<Notice>,
}

impl Mam {
    /// Before MAM's hosts are configured.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            enabled: false,
            available: false,
            session: Session::Unverified,
            network: Network::new(),
            seedbox: Seedbox::new(),
            account: Account::new(),
            acquisition: Acquisition::new(),
            budget: Budget::new(),
            outstanding: None,
            last_reminder: LastReminder::Unread,
            client: ClientStanding::Unknown,
            connectability: Connectability::Fine,
            renew_lease: false,
            cookie_in_use: None,
            issued_cookie: None,
            epoch: 0,
            wake: None,
            notices: Vec::new(),
        }
    }

    pub(super) fn start_reading(&mut self) -> Vec<Action> {
        self.seedbox.last_attempt = LastAttempt::Reading;
        self.last_reminder = LastReminder::Reading;
        vec![Action::ReadMamState]
    }

    /// Notices decided since they were last taken.
    pub(super) fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    /// The VPN moved to another server: the IP MAM sees may have changed.
    pub(super) const fn vpn_switched(&mut self) {
        self.network.unconfirm();
    }

    pub(super) fn timer_expired(&mut self, id: TimerId) {
        if self.wake.is_some_and(|(timer, _)| timer == id) {
            self.wake = None;
        }
    }

    /// Handle an event; a new cookie MAM issued, for settings to save, comes
    /// back separately.
    pub(super) fn handle(
        &mut self,
        now: ClockReading,
        event: MamEvent,
        registry: &mut Registry,
    ) -> (Vec<Action>, Option<ApiCookie>) {
        match event {
            MamEvent::Enable => {
                self.enabled = true;
                // A previous run may have looked the IP up moments ago.
                self.network.sent(now.monotonic);
                (Vec::new(), None)
            }
            // The state is read once, at startup, before anything changes it.
            MamEvent::StateRead {
                last_registration,
                last_reminder,
            } => {
                self.seedbox.last_attempt = LastAttempt::Known(last_registration);
                self.last_reminder = LastReminder::Known(last_reminder);
                (Vec::new(), None)
            }
            MamEvent::StateReadFailed => {
                self.seedbox.last_attempt = LastAttempt::ReadFailed;
                self.last_reminder = LastReminder::ReadFailed;
                (Vec::new(), None)
            }
            MamEvent::RegistrationAttemptSaved => {
                let Some(Outstanding::SavingRegistration { at, cookie, epoch }) =
                    self.outstanding.take_if(|outstanding| {
                        matches!(outstanding, Outstanding::SavingRegistration { .. })
                    })
                else {
                    return (Vec::new(), None);
                };
                // The attempt counts against the spacing even if it is not sent.
                self.seedbox.saved(at);
                if epoch != self.epoch {
                    return (Vec::new(), None);
                }
                let request = MamRequest::RegisterSeedbox;
                self.outstanding = Some(Outstanding::Request {
                    request: request.clone(),
                    sent: now.monotonic,
                    epoch,
                });
                (
                    vec![Action::MamRequest {
                        request,
                        cookie: Some(cookie),
                    }],
                    None,
                )
            }
            MamEvent::RegistrationAttemptSaveFailed => {
                if matches!(
                    self.outstanding,
                    Some(Outstanding::SavingRegistration { .. })
                ) {
                    self.outstanding = None;
                    self.seedbox.save_failed(now.wall);
                }
                (Vec::new(), None)
            }
            MamEvent::ReminderSaved | MamEvent::ReminderSaveFailed => (Vec::new(), None),
            MamEvent::PurchaseChecked { made } => {
                if let Some(purchase) = self.account.checked(made) {
                    self.notices
                        .push(Notice::PurchaseSettled { purchase, made });
                }
                (Vec::new(), None)
            }
            MamEvent::Answered { result, rotated } => {
                let Some(Outstanding::Request { request, epoch, .. }) = self
                    .outstanding
                    .take_if(|outstanding| matches!(outstanding, Outstanding::Request { .. }))
                else {
                    return (Vec::new(), None);
                };
                let current = epoch == self.epoch;
                let actions = self.answered(now, &request, result, current, registry);
                let rotated = rotated.filter(|_| current && request.needs_cookie());
                if let Some(cookie) = &rotated {
                    self.issued_cookie = Some(cookie.clone());
                }
                (actions, rotated)
            }
        }
    }

    fn answered(
        &mut self,
        now: ClockReading,
        request: &MamRequest,
        result: Result<MamReply, MamFailure>,
        current: bool,
        registry: &mut Registry,
    ) -> Vec<Action> {
        if current && request.needs_cookie() {
            self.follow_session(now.wall, request, &result);
        }
        match (request, result) {
            (MamRequest::LookUpIp, Ok(MamReply::Ip(observation))) if current => {
                self.network.observed(now.monotonic, now.wall, observation);
                Vec::new()
            }
            (MamRequest::LookUpIp, Err(failure)) if current => {
                self.network.failed(now.monotonic, failure);
                Vec::new()
            }
            (MamRequest::RegisterSeedbox, Ok(MamReply::Registered { ip, .. })) if current => {
                self.seedbox.confirmed(ip, now.wall);
                Vec::new()
            }
            (MamRequest::RegisterSeedbox, Err(failure)) if current => {
                self.seedbox.failed(failure, now.wall);
                Vec::new()
            }
            (MamRequest::LoadUserData { .. }, Ok(MamReply::UserData(data))) if current => {
                if let Some((purchase, PurchaseOutcome::Settled { made, .. })) =
                    self.account.read(now.monotonic, now.wall, *data)
                {
                    self.notices
                        .push(Notice::PurchaseSettled { purchase, made });
                }
                self.follow_downloads(now, registry)
            }
            (MamRequest::WedgeHistory, Ok(MamReply::WedgeHistory(spent))) if current => {
                self.acquisition.history_read(now.monotonic, &spent);
                self.follow_downloads(now, registry)
            }
            (MamRequest::WedgeHistory, Err(_)) if current => {
                self.acquisition.history_failed(now.monotonic);
                Vec::new()
            }
            (MamRequest::LookUpTorrent { id }, Ok(MamReply::Torrent(listing))) if current => {
                let snatched = listing
                    .as_ref()
                    .is_some_and(|listing| listing.snatched == Snatched::Before);
                self.acquisition
                    .looked_up(now.monotonic, *id, listing.map(|listing| *listing));
                let mut actions = match registry.records_mut() {
                    Some(records) if snatched => records.snatched(now.monotonic, *id),
                    _ => Vec::new(),
                };
                actions.extend(self.follow_downloads(now, registry));
                actions
            }
            (MamRequest::LookUpTorrent { .. }, Err(_)) if current => {
                self.acquisition.lookup_failed(now.monotonic);
                Vec::new()
            }
            (MamRequest::LoadUserData { .. }, Err(failure)) if current => {
                self.account.failed(now.monotonic, failure);
                Vec::new()
            }
            (MamRequest::Buy(purchase), result) => {
                self.purchased(now.monotonic, now.wall, *purchase, result);
                Vec::new()
            }
            (MamRequest::FetchTorrent { id, wedge }, result) => {
                self.fetched(now, *id, *wedge, result, registry)
            }
            (MamRequest::ListBookmarks { .. }, Ok(MamReply::Bookmarks(page))) if current => {
                self.listed(now, page, registry)
            }
            (MamRequest::ListBookmarks { .. }, Err(failure)) if current => {
                self.acquisition.bookmarks.failed(now.monotonic, failure);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A refusal stops requests that need the cookie; a success proves it.
    /// A session that may not register the seedbox may still do the rest.
    fn follow_session(
        &mut self,
        now: SystemTime,
        request: &MamRequest,
        result: &Result<MamReply, MamFailure>,
    ) {
        match result {
            Err(MamFailure::Session(SessionRejection::NotAllowed))
                if *request == MamRequest::RegisterSeedbox => {}
            Err(MamFailure::Session(rejection)) => {
                self.session = Session::Rejected {
                    rejection: *rejection,
                    seen: self
                        .network
                        .current()
                        .map(|observed| (observed.ip, observed.asn)),
                    at: now,
                };
            }
            Ok(_) => self.session = Session::Verified { at: now },
            Err(_) => {}
        }
    }

    /// A purchase's outcome is recorded whenever it arrives.
    fn purchased(
        &mut self,
        now: Instant,
        wall: SystemTime,
        purchase: Purchase,
        result: Result<MamReply, MamFailure>,
    ) {
        let outcome = match result {
            Ok(MamReply::Purchased { points }) => {
                self.notices.push(Notice::PurchaseMade { purchase, points });
                PurchaseOutcome::Made(points)
            }
            Err(failure) if failure.uncertain() => {
                self.notices.push(Notice::PurchaseUncertain { purchase });
                PurchaseOutcome::Uncertain
            }
            Err(MamFailure::Refused { message }) => {
                self.notices.push(Notice::PurchaseRefused { purchase });
                PurchaseOutcome::Refused(message)
            }
            Err(_) | Ok(_) => PurchaseOutcome::Refused(None),
        };
        self.account.bought(purchase, now, wall, outcome);
    }

    /// A fetched torrent is recorded whenever it arrives, since a wedge may
    /// have been spent on it.
    fn fetched(
        &mut self,
        now: ClockReading,
        id: MamTorrentId,
        wedge: bool,
        result: Result<MamReply, MamFailure>,
        registry: &mut Registry,
    ) -> Vec<Action> {
        self.acquisition.fetching = None;
        // Only a loaded registry has rows to fetch for.
        let queued = registry.records_mut().is_some_and(|records| {
            records
                .find_mam(id)
                .is_some_and(|record| record.desired == Desired::Queued)
        });
        match result {
            // A queued torrent becomes a download.
            Ok(MamReply::Fetched(fetched)) if queued => registry
                .records_mut()
                .map(|records| records.fetched(now.monotonic, id, fetched.hash))
                .unwrap_or_default(),
            // A running download was wedged because its freeleech ended; the
            // bonus history must show the wedge before it counts.
            Ok(MamReply::Fetched(_)) => {
                if wedge {
                    self.notices.push(Notice::WedgedMidDownload);
                }
                self.follow_downloads(now, registry)
            }
            Err(MamFailure::Refused { .. }) => {
                self.acquisition.refused = Some(now.wall);
                Vec::new()
            }
            // A lost answer may have spent a wedge; the bonus history shows
            // whether before another is spent.
            Err(_) | Ok(_) => Vec::new(),
        }
    }

    /// Record new bookmarks once the listing is complete; bookmarks MAM reports
    /// snatched before wait for the owner.
    fn listed(
        &mut self,
        now: ClockReading,
        page: BookmarkPage,
        registry: &mut Registry,
    ) -> Vec<Action> {
        let (Some(listed), Some(records)) = (
            self.acquisition
                .bookmarks
                .page(now.monotonic, now.wall, page),
            registry.records_mut(),
        ) else {
            return Vec::new();
        };
        // A queued bookmark MAM now reports snatched waits for the owner.
        let mut actions = Vec::new();
        for bookmark in listed
            .bookmarks
            .iter()
            .filter(|bookmark| bookmark.snatched == Snatched::Before)
        {
            actions.extend(records.snatched(now.monotonic, bookmark.id));
        }
        let new: Vec<(MamTorrentId, Desired)> = listed
            .bookmarks
            .iter()
            .filter(|bookmark| !records.knows(bookmark.id))
            .map(|bookmark| {
                let desired = match bookmark.snatched {
                    Snatched::Before => Desired::Snatched,
                    Snatched::Never => Desired::Queued,
                };
                (bookmark.id, desired)
            })
            .collect();
        actions.extend(records.bookmarked(now.monotonic, new));
        actions
    }

    /// Choose and send the next request, if one is wanted and allowed.
    pub(super) fn decide(
        &mut self,
        now: ClockReading,
        context: &Context<'_>,
        timers: &mut Timers,
    ) -> Vec<Action> {
        let mut actions = self.remind(now);
        let available =
            self.enabled && context.requests == MamRequestsStatus::Running && context.protected;
        if self.available && !available {
            self.disconnect();
        }
        self.available = available;
        self.follow_cookie(context);
        let observed = self.network.current().cloned();
        let cookie = context
            .cookie
            .filter(|_| !self.session.blocks(observed.as_ref()))
            .cloned();
        self.client = self.account.client_standing(context.client_version);
        self.follow_connectability(now, context);
        let records = match context.registry {
            Registry::Loaded(records) => Some(records),
            _ => None,
        };
        let recent = self.watch_downloads(now, records);
        let downloading = records.map_or(0, |records| {
            let running = records
                .downloads()
                .filter(|download| !download.finished && !download.held)
                .count();
            u32::try_from(running).unwrap_or(u32::MAX)
        });
        let facts = Facts {
            now: now.monotonic,
            wall: now.wall,
            mam_available: available && cookie.is_some(),
            seedbox_confirmed: self.seedbox.confirms(observed.as_ref()),
            client_ready: context.client_ready,
            client: self.client,
            recent,
            downloading,
            downloading_limit: context.downloading_limit,
            account: &self.account,
        };
        let next = records.map_or(Next::Idle, |records| {
            self.acquisition.next(records.queued(), facts)
        });
        let watching = self.acquisition.watch_next(&self.account, now.monotonic);
        self.acquisition.hold = match next {
            Next::Hold(hold) => Some(hold),
            Next::NeedReading if self.account.reading.is_none() => Some(Hold::AccountUnknown),
            _ => None,
        };
        // An uncertain purchase is settled by the next reading, which comes early.
        let early = next == Next::NeedReading
            || watching == Next::NeedReading
            || self.account.purchase_uncertain();
        if available && self.outstanding.is_none() {
            let wanted = self.wanted(
                now,
                cookie.is_some(),
                context.reserve,
                (watching, next),
                early,
            );
            match (wanted, &cookie) {
                (Some(Wanted::Register), Some(cookie)) if self.budget.take(now.monotonic) => {
                    self.outstanding = Some(Outstanding::SavingRegistration {
                        at: now.wall,
                        cookie: cookie.clone(),
                        epoch: self.epoch,
                    });
                    actions.push(Action::SaveRegistrationAttempt { at: now.wall });
                }
                (Some(Wanted::Request(request)), _) if self.budget.take(now.monotonic) => {
                    match &request {
                        MamRequest::LookUpIp => self.network.sent(now.monotonic),
                        MamRequest::LoadUserData { .. } => self.account.requested(now.monotonic),
                        MamRequest::Buy(purchase) => {
                            self.account.buying(*purchase, now.monotonic, now.wall);
                        }
                        MamRequest::FetchTorrent { id, wedge } => {
                            let admitted = records.is_some_and(|records| {
                                records
                                    .find_mam(*id)
                                    .is_some_and(|record| record.desired == Desired::Queued)
                            });
                            self.acquisition
                                .fetching(now.monotonic, *id, *wedge, admitted);
                        }
                        _ => {}
                    }
                    let cookie = cookie.filter(|_| request.needs_cookie());
                    self.outstanding = Some(Outstanding::Request {
                        request: request.clone(),
                        sent: now.monotonic,
                        epoch: self.epoch,
                    });
                    actions.push(Action::MamRequest { request, cookie });
                }
                _ => {}
            }
        }
        actions.extend(self.schedule_wake(now, early, timers));
        actions
    }

    /// Whether a new forwarded-port lease is wanted, once.
    pub(super) const fn take_lease_renewal(&mut self) -> bool {
        std::mem::replace(&mut self.renew_lease, false)
    }

    /// Note what Windlass knows about each running download it added; how many
    /// downloads qBittorrent added in the last hour, or will add.
    fn watch_downloads(
        &mut self,
        now: ClockReading,
        records: Option<&Records>,
    ) -> UnsatisfiedCount {
        let Some(records) = records else {
            self.acquisition.watched = Vec::new();
            return UnsatisfiedCount::default();
        };
        self.acquisition.watched = self.acquisition.watch_all(
            records.downloads(),
            self.account.data(),
            now.monotonic,
            now.wall,
        );
        let recent = records
            .downloads()
            .filter(|download| acquisition::recent(download, now.wall))
            .count();
        UnsatisfiedCount::new(u32::try_from(recent).unwrap_or(u32::MAX))
    }

    /// Apply what Windlass knows about each running download it added: a
    /// download shown free runs; one that is not free and cannot be wedged
    /// stops, and the owner is told once.
    fn follow_downloads(&mut self, now: ClockReading, registry: &mut Registry) -> Vec<Action> {
        let Some(records) = registry.records_mut() else {
            return Vec::new();
        };
        let watched = self.acquisition.watch_all(
            records.downloads(),
            self.account.data(),
            now.monotonic,
            now.wall,
        );
        let no_wedges = self
            .account
            .data()
            .is_some_and(|data| data.wedges.is_zero());
        let mut actions = Vec::new();
        for (download, watch) in &watched {
            let notice = match *watch {
                DownloadWatch::Wedged | DownloadWatch::FreeForNow => {
                    if download.held {
                        actions.extend(records.resume(now.monotonic, &download.hash));
                    }
                    self.acquisition.reported.remove(&download.mam);
                    None
                }
                DownloadWatch::NotFree {
                    wedge_missing: true,
                    ..
                } => Some(Notice::WedgeNotSpent),
                DownloadWatch::NotFree { .. } if download.finished => {
                    Some(Notice::DownloadNotFree { stopped: false })
                }
                DownloadWatch::NotFree { listed, .. } if !listed || no_wedges => {
                    Some(Notice::DownloadNotFree { stopped: true })
                }
                // Waiting for MAM, or a wedge is spent on it next.
                DownloadWatch::NeedLookup
                | DownloadWatch::NeedHistory
                | DownloadWatch::NotFree { .. } => None,
            };
            let Some(notice) = notice else {
                continue;
            };
            if !download.finished && !download.held {
                actions.extend(records.hold(now.monotonic, &download.hash));
            }
            if self.acquisition.reported.insert(download.mam) {
                self.notices.push(notice);
            }
        }
        self.acquisition.watched = watched;
        actions
    }

    /// When MAM cannot reach qBittorrent, check the connection, ask for a new
    /// lease, and observe and register again when allowed (A6).
    fn follow_connectability(&mut self, now: ClockReading, context: &Context<'_>) {
        let reading = self
            .account
            .reading
            .as_ref()
            .map(|reading| (reading.taken, self.unreachable(context.client_version)));
        if !self.connectability.follow(reading) {
            return;
        }
        let observed = self.network.current().cloned();
        let findings = Findings::check(
            &context.connection,
            observed.as_ref().map(|observed| observed.ip),
            self.seedbox.confirms(observed.as_ref()),
        );
        self.renew_lease = findings.lease_renewed;
        self.connectability
            .repaired(now.monotonic, now.wall, findings);
        self.network.unconfirm();
        self.seedbox.unconfirm();
    }

    /// Whether the latest reading says MAM cannot reach qBittorrent: the snatch
    /// summary, or the running client's entry in the client list.
    fn unreachable(&self, running: Option<ClientVersion>) -> bool {
        let summary = self
            .account
            .data()
            .and_then(|data| data.summary.as_ref())
            .is_some_and(|summary| summary.connectable == Connectable::No);
        let client = running.is_some_and(|running| {
            self.account
                .running_client(running)
                .any(|client| client.connectable == Some(Connectable::No))
        });
        summary || client
    }

    /// The most urgent request wanted now: the watch over running downloads
    /// before purchases, and admission after them.
    fn wanted(
        &self,
        now: ClockReading,
        cookie: bool,
        reserve: Option<Points>,
        (watching, next): (Next, Next),
        early: bool,
    ) -> Option<Wanted> {
        if self.network.due(now.monotonic) {
            return Some(Wanted::Request(MamRequest::LookUpIp));
        }
        // The session is locked to an ASN, so it is used only once MAM has shown
        // which IP and ASN it sees now: after a start or a server switch, the
        // IP lookup comes first.
        if !cookie || self.network.current().is_none() {
            return None;
        }
        if self.seedbox.wanted(now.wall, self.network.current()) {
            return Some(Wanted::Register);
        }
        if let Some(client_stats) = self.account.wanted(now.monotonic, early) {
            return Some(Wanted::Request(MamRequest::LoadUserData { client_stats }));
        }
        // A due listing refreshes every bookmark's flags in one request, so it
        // comes before looking a single torrent up.
        let listing = self.acquisition.bookmarks.wanted(now.monotonic);
        let single = |next: Next| match (next, listing) {
            (Next::LookUp(_), Some(_)) => None,
            (next, _) => acquisition_request(next),
        };
        if let (Next::LookUp(_), Some(start)) = (watching, listing) {
            return Some(Wanted::Request(MamRequest::ListBookmarks { start }));
        }
        if let Some(request) = single(watching) {
            return Some(Wanted::Request(request));
        }
        if let Some(purchase) = reserve.and_then(|reserve| {
            self.account
                .wanted_purchase(now.monotonic, now.wall, reserve)
        }) {
            return Some(Wanted::Request(MamRequest::Buy(purchase)));
        }
        if let Some(request) = single(next) {
            return Some(Wanted::Request(request));
        }
        listing.map(|start| Wanted::Request(MamRequest::ListBookmarks { start }))
    }

    /// Pause or lost protection: work in progress is dropped, and the session
    /// and registration need confirming again.
    /// A request already sent cannot be recalled; its answer is applied only
    /// where it records a spend.
    fn disconnect(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.session = Session::Unverified;
        self.network.unconfirm();
        self.seedbox.unconfirm();
    }

    /// A cookie the owner supplied replaces the session; one MAM issued
    /// continues it.
    fn follow_cookie(&mut self, context: &Context<'_>) {
        let requested = context.requested_cookie;
        if requested != self.cookie_in_use.as_ref() && requested != self.issued_cookie.as_ref() {
            self.cookie_in_use = requested.cloned();
            self.issued_cookie = None;
            self.epoch = self.epoch.wrapping_add(1);
            self.session = Session::Unverified;
            self.seedbox.unconfirm();
            self.acquisition.refused = None;
        } else if context.cookie.is_some() && context.cookie == self.issued_cookie.as_ref() {
            self.cookie_in_use = self.issued_cookie.take();
        }
    }

    /// Remind the owner weekly to visit MAM; the first reminder comes a week
    /// after the count starts.
    fn remind(&mut self, now: ClockReading) -> Vec<Action> {
        let due = match self.last_reminder {
            LastReminder::Known(None) | LastReminder::ReadFailed => false,
            LastReminder::Known(Some(last)) => last
                .checked_add(REMINDER_EVERY)
                .is_some_and(|due| due <= now.wall),
            LastReminder::Unread | LastReminder::Reading => return Vec::new(),
        };
        let start = matches!(
            self.last_reminder,
            LastReminder::Known(None) | LastReminder::ReadFailed
        );
        if !due && !start {
            return Vec::new();
        }
        if due {
            self.notices.push(Notice::MamVisitReminder);
        }
        self.last_reminder = LastReminder::Known(Some(now.wall));
        vec![Action::SaveReminder { at: now.wall }]
    }

    /// Wake the core when the earliest wanted work becomes due.
    fn schedule_wake(
        &mut self,
        now: ClockReading,
        early: bool,
        timers: &mut Timers,
    ) -> Vec<Action> {
        let monotonic = |at: SystemTime| {
            now.monotonic
                .checked_add(at.duration_since(now.wall).unwrap_or_default())
        };
        let reminder = match self.last_reminder {
            LastReminder::Known(Some(last)) => last.checked_add(REMINDER_EVERY).and_then(monotonic),
            _ => None,
        };
        let registration = match self.seedbox.allowance(now.wall) {
            RegistrationAllowance::After(at) => monotonic(at),
            RegistrationAllowance::Now | RegistrationAllowance::Unknown => None,
        };
        let waiting = self.available && self.outstanding.is_none();
        let work = [
            self.network.next_due(),
            registration,
            self.account.next_due(early),
            self.acquisition.bookmarks.next_due(),
            self.acquisition.next_due(),
            self.budget.next_token(now.monotonic),
        ]
        .into_iter()
        .flatten()
        .filter(|_| waiting);
        let due = reminder
            .into_iter()
            .chain(work)
            .filter(|due| *due > now.monotonic)
            .min();
        let Some(due) = due else {
            return Vec::new();
        };
        if self.wake.is_some_and(|(_, at)| at == due) {
            return Vec::new();
        }
        let (id, deadline) =
            timers.schedule(now.monotonic, due.saturating_duration_since(now.monotonic));
        self.wake = Some((id, due));
        vec![Action::StartTimer {
            name: TimerName::Mam,
            id,
            deadline,
        }]
    }
}

/// The request bookmark acquisition or the watch over downloads wants.
const fn acquisition_request(next: Next) -> Option<MamRequest> {
    match next {
        Next::Fetch { id, wedge } => Some(MamRequest::FetchTorrent { id, wedge }),
        Next::LookUp(id) => Some(MamRequest::LookUpTorrent { id }),
        Next::NeedHistory => Some(MamRequest::WedgeHistory),
        Next::Idle | Next::NeedReading | Next::Hold(_) => None,
    }
}

/// The work chosen for this step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wanted {
    /// Save a registration attempt, then send it.
    Register,
    /// Send this request.
    Request(MamRequest),
}
