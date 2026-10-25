//! Bookmarks become downloads (B-0005), each admitted only when every account
//! rule allows it (B-0011's admission order).

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use super::{
    Bookmark, BookmarkPage, MamFailure, PersonalFreeleech, WedgeSpent,
    account::{Account, ClientStanding},
};
use crate::core::{Download, MamTorrentId, UserData};

/// How often bookmarks are listed.
pub const BOOKMARKS_EVERY: Duration = Duration::from_mins(15);
/// Listing stops after this many pages of 100.
pub const MAX_BOOKMARK_PAGES: usize = 10;
/// Downloads admitted this recently count against the unsatisfied limit even
/// if MAM's count may not show them yet.
pub const RECENT_ADMISSION: Duration = Duration::from_hours(1);
/// How recent a torrent's flags must be before it is downloaded, from the
/// bookmark listing or a lookup by its ID.
pub const LOOKUP_FRESH_FOR: Duration = Duration::from_mins(5);
/// How recent a running download's flags must be.
///
/// Bookmarks are listed every 15 minutes, which refreshes every bookmarked
/// download at once; this leaves 5 minutes for a listing to finish before a
/// download is looked up by itself. A download must stay free for at least
/// this long to need no wedge.
pub const WATCH_FRESH_FOR: Duration = Duration::from_mins(20);
/// Bonus history read this recently can decide whether a wedge is needed.
pub const HISTORY_FRESH_FOR: Duration = Duration::from_mins(5);
/// The bonus history is read this long after a wedged download, so MAM has
/// recorded the wedge.
pub const WEDGE_RECORDED_AFTER: Duration = Duration::from_mins(1);
const RETRY_AFTER: Duration = Duration::from_mins(5);
const PAGE_SIZE: usize = 100;

/// Why the next queued torrent is not downloaded yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Hold {
    /// MAM requests are paused, the VPN is down, or the session is not usable.
    MamUnavailable,
    /// MAM has not confirmed the seedbox for the current IP.
    SeedboxUnconfirmed,
    /// MAM refused a download; downloads stay held until restart or a new cookie.
    DownloadsRefused,
    /// qBittorrent is not connected or runs a version MAM does not allow.
    ClientUnavailable,
    /// MAM's client list does not show the running qBittorrent as allowed.
    MamClientList(ClientStanding),
    /// The account has not been read yet.
    AccountUnknown,
    /// The ratio is below 2.0.
    RatioLow,
    /// Another download would leave fewer than the reserved unsatisfied slots.
    UnsatisfiedLimit {
        /// MAM's count plus recent admissions.
        count: u32,
        /// MAM's limit.
        limit: u32,
    },
    /// The torrent needs a wedge and none is left.
    NoWedge,
    /// The queued torrents are no longer bookmarked, or MAM no longer lists them.
    NotBookmarked,
}

/// A complete list of bookmarks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// When the listing finished.
    pub at: SystemTime,
    /// When, by the process clock.
    pub taken: Instant,
    /// Bookmarks in total, as MAM counts them.
    pub found: u32,
    /// The bookmarks.
    pub bookmarks: Vec<Bookmark>,
}

/// Listing bookmarks, a page at a time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmarks {
    /// The latest complete list.
    pub latest: Option<Listed>,
    /// When the next listing is due; now when absent.
    pub next: Option<Instant>,
    /// Listings that failed since the last success.
    pub failures: u32,
    /// Why the latest listing failed.
    pub last_failure: Option<MamFailure>,
    listing: Option<Vec<Bookmark>>,
}

impl Bookmarks {
    /// The page to request now: the next of a listing in progress, or the first
    /// of a due listing.
    pub(super) fn wanted(&self, now: Instant) -> Option<u32> {
        self.listing.as_ref().map_or_else(
            || self.next.is_none_or(|next| next <= now).then_some(0),
            |listing| u32::try_from(listing.len()).ok(),
        )
    }

    /// When the next listing is due.
    pub(super) const fn next_due(&self) -> Option<Instant> {
        if self.listing.is_some() {
            return None;
        }
        self.next
    }

    /// Add a page; the complete list once the last page arrived.
    pub(super) fn page(
        &mut self,
        now: Instant,
        wall: SystemTime,
        page: BookmarkPage,
    ) -> Option<&Listed> {
        let empty = page.bookmarks.is_empty();
        let listing = self.listing.get_or_insert_with(Vec::new);
        listing.extend(page.bookmarks);
        let done = empty
            || listing.len() >= usize::try_from(page.found).unwrap_or(usize::MAX)
            || listing.len() >= MAX_BOOKMARK_PAGES * PAGE_SIZE;
        if !done {
            return None;
        }
        self.latest = Some(Listed {
            at: wall,
            taken: now,
            found: page.found,
            bookmarks: self.listing.take().unwrap_or_default(),
        });
        self.failures = 0;
        self.last_failure = None;
        self.next = now.checked_add(BOOKMARKS_EVERY);
        self.latest.as_ref()
    }

    pub(super) fn failed(&mut self, now: Instant, failure: MamFailure) {
        let wait = match &failure {
            MamFailure::TooSoon {
                retry_after: Some(after),
            } => RETRY_AFTER.max(*after),
            _ => RETRY_AFTER,
        };
        self.listing = None;
        self.failures = self.failures.saturating_add(1);
        self.last_failure = Some(failure);
        self.next = now.checked_add(wait);
    }

    /// The latest listing of this torrent.
    #[must_use]
    pub fn find(&self, id: MamTorrentId) -> Option<&Bookmark> {
        self.latest
            .as_ref()?
            .bookmarks
            .iter()
            .find(|bookmark| bookmark.id == id)
    }
}

/// A torrent as MAM's search reported it, looked up by its ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    /// The listing; `None` when MAM no longer lists the torrent.
    pub listing: Option<Bookmark>,
    /// When.
    pub at: Instant,
}

/// The torrents User Bonus History shows a wedge spent on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WedgeHistory {
    /// The torrents.
    pub wedged: BTreeSet<MamTorrentId>,
    /// When it was read.
    pub at: Instant,
}

/// What Windlass knows about a running download it added: it must stay free
/// for the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DownloadWatch {
    /// The bonus history shows a wedge spent on it, so it stays free.
    Wedged,
    /// The latest lookup shows it free for the owner, for now.
    FreeForNow,
    /// It must be looked up on MAM.
    NeedLookup,
    /// The bonus history must show whether a wedge was spent on it.
    NeedHistory,
    /// It is not free for the owner.
    NotFree {
        /// Whether Windlass spent a wedge on it that the history does not show.
        wedge_missing: bool,
        /// Whether MAM still lists it, so a wedge could be spent.
        listed: bool,
    },
}

/// The next request admission or the watch over running downloads wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Next {
    /// Nothing is wanted.
    Idle,
    /// Download it, with a wedge or without.
    Fetch {
        /// The torrent.
        id: MamTorrentId,
        /// Whether to spend a wedge.
        wedge: bool,
    },
    /// Look the torrent up on MAM.
    LookUp(MamTorrentId),
    /// Read the bonus history.
    NeedHistory,
    /// A fresh reading of the account must come first.
    NeedReading,
    /// It waits.
    Hold(Hold),
}

/// What admission depends on outside acquisition.
#[derive(Debug, Clone, Copy)]
pub(super) struct Facts<'a> {
    pub(super) now: Instant,
    pub(super) wall: SystemTime,
    pub(super) mam_available: bool,
    pub(super) seedbox_confirmed: bool,
    pub(super) client_ready: bool,
    pub(super) client: ClientStanding,
    /// Downloads qBittorrent added in the last hour, or will add.
    pub(super) recent: u32,
    pub(super) account: &'a Account,
}

/// Bookmark acquisition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Acquisition {
    /// Listing bookmarks.
    pub bookmarks: Bookmarks,
    /// When MAM refused a download; downloads are held since.
    pub refused: Option<SystemTime>,
    /// Why the next queued torrent waits, if it does.
    pub hold: Option<Hold>,
    /// The download being fetched, and whether with a wedge.
    pub fetching: Option<(MamTorrentId, bool)>,
    /// When the latest download was admitted.
    pub last_admission: Option<Instant>,
    /// Torrents looked up on MAM by ID.
    pub lookups: BTreeMap<MamTorrentId, Lookup>,
    /// The latest bonus history.
    pub history: Option<WedgeHistory>,
    /// Wedged downloads sent, by torrent; the bonus history must show each.
    pub wedge_attempts: BTreeMap<MamTorrentId, Instant>,
    /// What Windlass knows about each running download it added.
    pub watched: Vec<(Download, DownloadWatch)>,
    /// Running downloads the owner was told about.
    pub reported: BTreeSet<MamTorrentId>,
    lookup_failed: Option<Instant>,
    history_failed: Option<Instant>,
}

impl Acquisition {
    pub(super) const fn new() -> Self {
        Self {
            bookmarks: Bookmarks {
                latest: None,
                next: None,
                failures: 0,
                last_failure: None,
                listing: None,
            },
            refused: None,
            hold: None,
            fetching: None,
            last_admission: None,
            lookups: BTreeMap::new(),
            history: None,
            wedge_attempts: BTreeMap::new(),
            watched: Vec::new(),
            reported: BTreeSet::new(),
            lookup_failed: None,
            history_failed: None,
        }
    }

    /// The torrent as MAM reported it most recently, if no longer ago than
    /// `within`.
    fn current(
        &self,
        id: MamTorrentId,
        now: Instant,
        within: Duration,
    ) -> Option<(Option<&Bookmark>, Instant)> {
        self.latest(id)
            .filter(|(_, at)| now.saturating_duration_since(*at) <= within)
    }

    /// The torrent as MAM reported it most recently, by the bookmark listing
    /// or a lookup by its ID, and when; `None` inside when MAM no longer lists
    /// it.
    fn latest(&self, id: MamTorrentId) -> Option<(Option<&Bookmark>, Instant)> {
        let looked_up = self
            .lookups
            .get(&id)
            .map(|lookup| (lookup.listing.as_ref(), lookup.at));
        let listed = self.bookmarks.latest.as_ref().and_then(|latest| {
            latest
                .bookmarks
                .iter()
                .find(|bookmark| bookmark.id == id)
                .map(|bookmark| (Some(bookmark), latest.taken))
        });
        [looked_up, listed]
            .into_iter()
            .flatten()
            .max_by_key(|(_, at)| *at)
    }

    /// Whether the bonus history was read recently enough, and long enough
    /// after the last wedged download of this torrent to show its wedge.
    fn history_settles(&self, id: MamTorrentId, now: Instant) -> Option<&WedgeHistory> {
        self.history.as_ref().filter(|history| {
            now.saturating_duration_since(history.at) <= HISTORY_FRESH_FOR
                && self
                    .wedge_recorded(id)
                    .is_none_or(|recorded| history.at >= recorded)
        })
    }

    /// When MAM has recorded the wedge of this torrent's last wedged download.
    fn wedge_recorded(&self, id: MamTorrentId) -> Option<Instant> {
        self.wedge_attempts
            .get(&id)
            .and_then(|attempt| attempt.checked_add(WEDGE_RECORDED_AFTER))
    }

    /// Whether the bonus history may be asked for now about this torrent.
    fn may_ask_history(&self, id: MamTorrentId, now: Instant) -> bool {
        Self::may_ask(self.history_failed, now)
            && self
                .wedge_recorded(id)
                .is_none_or(|recorded| now >= recorded)
    }

    /// Whether a lookup or the bonus history may be asked for now.
    fn may_ask(failed: Option<Instant>, now: Instant) -> bool {
        failed.is_none_or(|failed| now.saturating_duration_since(failed) >= RETRY_AFTER)
    }

    /// Ask for this now if allowed, or wait.
    const fn ask(allowed: bool, next: Next) -> Next {
        if allowed { next } else { Next::Idle }
    }

    /// Decide for the first queued torrent that is still bookmarked and listed.
    pub(super) fn next(
        &self,
        mut queued: impl Iterator<Item = MamTorrentId>,
        facts: Facts<'_>,
    ) -> Next {
        let mut any = false;
        let bookmark = queued.find_map(|id| {
            any = true;
            let gone = self
                .lookups
                .get(&id)
                .is_some_and(|lookup| lookup.listing.is_none());
            self.bookmarks.find(id).filter(|_| !gone)
        });
        let Some(bookmark) = bookmark else {
            return if any {
                Next::Hold(Hold::NotBookmarked)
            } else {
                Next::Idle
            };
        };
        if !facts.mam_available {
            return Next::Hold(Hold::MamUnavailable);
        }
        if !facts.seedbox_confirmed {
            return Next::Hold(Hold::SeedboxUnconfirmed);
        }
        if self.refused.is_some() {
            return Next::Hold(Hold::DownloadsRefused);
        }
        if !facts.client_ready {
            return Next::Hold(Hold::ClientUnavailable);
        }
        let Some(data) = facts.account.fresh(facts.now, self.last_admission) else {
            return Next::NeedReading;
        };
        if !facts.client.admits() {
            return Next::Hold(Hold::MamClientList(facts.client));
        }
        if facts.account.ratio_low().is_some() {
            return Next::Hold(Hold::RatioLow);
        }
        let Some(summary) = &data.summary else {
            return Next::Hold(Hold::AccountUnknown);
        };
        let count = summary.unsatisfied.saturating_add(facts.recent);
        let limit = summary.unsatisfied_limit;
        if count
            .saturating_add(1)
            .saturating_add(super::account::UNSATISFIED_RESERVE)
            > limit
        {
            return Next::Hold(Hold::UnsatisfiedLimit { count, limit });
        }
        let id = bookmark.id;
        // The torrent as MAM lists it now decides whether a wedge is needed.
        let Some((Some(listing), _)) = self.current(id, facts.now, LOOKUP_FRESH_FOR) else {
            return Self::ask(
                Self::may_ask(self.lookup_failed, facts.now),
                Next::LookUp(id),
            );
        };
        if free_for_owner(listing, data, facts.wall) {
            return Next::Fetch { id, wedge: false };
        }
        // A wedge spent before, or by an attempt whose answer was lost, is not
        // spent again.
        let Some(history) = self.history_settles(id, facts.now) else {
            return Self::ask(self.may_ask_history(id, facts.now), Next::NeedHistory);
        };
        let wedge = !history.wedged.contains(&id);
        if wedge && data.wedges == 0 {
            return Next::Hold(Hold::NoWedge);
        }
        Next::Fetch { id, wedge }
    }

    pub(super) fn fetching(&mut self, now: Instant, id: MamTorrentId, wedge: bool, admitted: bool) {
        self.fetching = Some((id, wedge));
        if wedge {
            self.wedge_attempts.insert(id, now);
        }
        if admitted {
            self.last_admission = Some(now);
        }
    }

    /// What Windlass knows about each running download it added: unfinished,
    /// or added within the last hour.
    pub(super) fn watch_all(
        &self,
        downloads: impl Iterator<Item = Download>,
        data: Option<&UserData>,
        now: Instant,
        wall: SystemTime,
    ) -> Vec<(Download, DownloadWatch)> {
        downloads
            .filter(|download| !download.finished || recent(download, wall))
            .map(|download| {
                let watch = self.watch(&download, data, now, wall);
                (download, watch)
            })
            .collect()
    }

    /// What Windlass knows about a running download it added.
    fn watch(
        &self,
        download: &Download,
        data: Option<&UserData>,
        now: Instant,
        wall: SystemTime,
    ) -> DownloadWatch {
        let history = self.history.as_ref();
        if history.is_some_and(|history| history.wedged.contains(&download.mam)) {
            return DownloadWatch::Wedged;
        }
        if let Some(recorded) = self.wedge_recorded(download.mam) {
            // Only a history read once MAM recorded the wedge can show it.
            return if history.is_some_and(|history| history.at >= recorded) {
                DownloadWatch::NotFree {
                    wedge_missing: true,
                    listed: true,
                }
            } else {
                DownloadWatch::NeedHistory
            };
        }
        let Some((listing, seen)) = self.current(download.mam, now, WATCH_FRESH_FOR) else {
            return DownloadWatch::NeedLookup;
        };
        let free = listing
            .zip(data)
            .is_some_and(|(listing, data)| free_for_owner(listing, data, wall));
        if free {
            DownloadWatch::FreeForNow
        } else if history.is_some_and(|history| history.at >= seen) {
            DownloadWatch::NotFree {
                wedge_missing: false,
                listed: listing.is_some(),
            }
        } else {
            DownloadWatch::NeedHistory
        }
    }

    /// The request the watch over running downloads wants now, if any.
    pub(super) fn watch_next(&self, account: &Account, now: Instant) -> Next {
        let mut wanted = Next::Idle;
        for (download, watch) in &self.watched {
            let next = match *watch {
                DownloadWatch::NeedLookup if Self::may_ask(self.lookup_failed, now) => {
                    Next::LookUp(download.mam)
                }
                DownloadWatch::NeedHistory if self.may_ask_history(download.mam, now) => {
                    Next::NeedHistory
                }
                DownloadWatch::NotFree {
                    wedge_missing: false,
                    listed: true,
                } if !download.finished => match account.fresh(now, None) {
                    Some(data) if data.wedges > 0 => Next::Fetch {
                        id: download.mam,
                        wedge: true,
                    },
                    Some(_) => Next::Idle,
                    None => Next::NeedReading,
                },
                _ => Next::Idle,
            };
            if next != Next::Idle {
                wanted = next;
                break;
            }
        }
        wanted
    }

    /// When a lookup or the bonus history may next be asked for, a wedge has
    /// been recorded, or a download free for now must be checked again.
    pub(super) fn next_due(&self) -> Option<Instant> {
        let free_until = self
            .watched
            .iter()
            .filter(|(_, watch)| *watch == DownloadWatch::FreeForNow)
            .filter_map(|(download, _)| self.latest(download.mam))
            .filter_map(|(_, seen)| seen.checked_add(WATCH_FRESH_FOR))
            .min();
        let retry =
            |failed: Option<Instant>| failed.and_then(|failed| failed.checked_add(RETRY_AFTER));
        let recorded = self
            .wedge_attempts
            .keys()
            .filter_map(|id| self.wedge_recorded(*id))
            .filter(|recorded| {
                self.history
                    .as_ref()
                    .is_none_or(|history| history.at < *recorded)
            })
            .min();
        [
            free_until,
            recorded,
            retry(self.lookup_failed),
            retry(self.history_failed),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(super) fn looked_up(&mut self, now: Instant, id: MamTorrentId, listing: Option<Bookmark>) {
        self.lookup_failed = None;
        self.lookups.insert(id, Lookup { listing, at: now });
    }

    pub(super) const fn lookup_failed(&mut self, now: Instant) {
        self.lookup_failed = Some(now);
    }

    pub(super) fn history_read(&mut self, now: Instant, spent: &[WedgeSpent]) {
        self.history_failed = None;
        self.history = Some(WedgeHistory {
            wedged: spent.iter().map(|entry| entry.torrent).collect(),
            at: now,
        });
    }

    pub(super) const fn history_failed(&mut self, now: Instant) {
        self.history_failed = Some(now);
    }
}

/// Whether qBittorrent added the download within the last hour, or will add it.
pub(super) fn recent(download: &Download, wall: SystemTime) -> bool {
    download.added_at.is_none_or(|at| {
        wall.duration_since(at)
            .map_or(true, |age| age < RECENT_ADMISSION)
    })
}

/// Whether downloading costs the owner no ratio without a wedge, now and until
/// the next check: freeleech for everyone, a personal freeleech, or a VIP
/// torrent while both it and the account are VIP.
fn free_for_owner(listing: &Bookmark, data: &UserData, now: SystemTime) -> bool {
    let until = now.checked_add(WATCH_FRESH_FOR).unwrap_or(now);
    let account_vip = data.is_vip(until);
    let torrent_vip = listing.vip && listing.vip_expires.is_none_or(|expires| expires > until);
    listing.free
        || listing.personal == PersonalFreeleech::ThroughWedge
        || (listing.personal == PersonalFreeleech::ThroughVip && account_vip)
        || (torrent_vip && account_vip)
}
