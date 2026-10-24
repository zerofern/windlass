//! Bookmarks become downloads (B-0005), each admitted only when every account
//! rule allows it (B-0011's admission order).

use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use super::{Bookmark, BookmarkPage, MamFailure, account::Account};
use crate::core::{MamTorrentId, TorrentHash, UserData};

/// How often bookmarks are listed.
pub const BOOKMARKS_EVERY: Duration = Duration::from_mins(15);
/// Listing stops after this many pages of 100.
pub const MAX_BOOKMARK_PAGES: usize = 10;
/// Downloads admitted this recently count against the unsatisfied limit even
/// if MAM's count may not show them yet.
pub const RECENT_ADMISSION: Duration = Duration::from_hours(1);
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
    /// The queued torrents are no longer bookmarked.
    NotBookmarked,
}

/// A complete list of bookmarks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// When the listing finished.
    pub at: SystemTime,
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

/// A wedged download waiting for a reading to show the wedge spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WedgeCheck {
    /// The most wedges the next reading may show.
    pub expected: u32,
    /// The torrent, held if the wedge was not spent.
    pub hash: TorrentHash,
}

/// What the admission order decides for the next queued torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Next {
    /// Nothing is queued.
    Idle,
    /// Download it, with a wedge or without.
    Fetch {
        /// The torrent.
        id: MamTorrentId,
        /// Whether to spend a wedge.
        wedge: bool,
    },
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
    pub(super) account: &'a Account,
}

/// Bookmark acquisition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Acquisition {
    /// Listing bookmarks.
    pub bookmarks: Bookmarks,
    /// Bookmarks MAM reports snatched before, which are not downloaded again
    /// automatically.
    pub suppressed: Vec<MamTorrentId>,
    /// When MAM refused a download; downloads are held since.
    pub refused: Option<SystemTime>,
    /// Why the next queued torrent waits, if it does.
    pub hold: Option<Hold>,
    /// A wedged download whose wedge the next reading must show spent.
    pub wedge_check: Option<WedgeCheck>,
    /// The download being fetched.
    pub fetching: Option<(MamTorrentId, bool)>,
    /// Downloads admitted, most recent last.
    pub admitted: Vec<Instant>,
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
            suppressed: Vec::new(),
            refused: None,
            hold: None,
            wedge_check: None,
            fetching: None,
            admitted: Vec::new(),
        }
    }

    /// When the latest download was admitted.
    #[must_use]
    pub fn last_admission(&self) -> Option<Instant> {
        self.admitted.last().copied()
    }

    /// Decide for the first queued torrent that is still bookmarked.
    pub(super) fn next(
        &self,
        mut queued: impl Iterator<Item = MamTorrentId>,
        facts: Facts<'_>,
    ) -> Next {
        let mut any = false;
        let bookmark = queued.find_map(|id| {
            any = true;
            self.bookmarks.find(id)
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
        let Some(data) = facts.account.fresh(facts.now, self.last_admission()) else {
            return Next::NeedReading;
        };
        if facts.account.ratio_low().is_some() {
            return Next::Hold(Hold::RatioLow);
        }
        let Some(summary) = &data.summary else {
            return Next::Hold(Hold::AccountUnknown);
        };
        let recent = self
            .admitted
            .iter()
            .filter(|at| facts.now.saturating_duration_since(**at) < RECENT_ADMISSION)
            .count();
        let count = summary
            .unsatisfied
            .saturating_add(u32::try_from(recent).unwrap_or(u32::MAX));
        let limit = summary.unsatisfied_limit;
        if count
            .saturating_add(1)
            .saturating_add(super::account::UNSATISFIED_RESERVE)
            > limit
        {
            return Next::Hold(Hold::UnsatisfiedLimit { count, limit });
        }
        let wedge = !free_for_owner(bookmark, data, facts.wall);
        if wedge && data.wedges == 0 {
            return Next::Hold(Hold::NoWedge);
        }
        Next::Fetch {
            id: bookmark.id,
            wedge,
        }
    }

    pub(super) fn fetching(&mut self, now: Instant, id: MamTorrentId, wedge: bool) {
        self.fetching = Some((id, wedge));
        self.admitted.push(now);
        self.admitted
            .retain(|at| now.saturating_duration_since(*at) < RECENT_ADMISSION);
    }

    /// A reading arrived: check the wedge spent on the last download. The hash
    /// of a download whose wedge was not spent, to hold.
    pub(super) fn check_wedges(&mut self, data: &UserData) -> Option<TorrentHash> {
        let check = self.wedge_check.take()?;
        (data.wedges > check.expected).then_some(check.hash)
    }
}

/// Whether downloading costs the owner no ratio without a wedge.
fn free_for_owner(bookmark: &Bookmark, data: &UserData, now: SystemTime) -> bool {
    bookmark.personal != super::PersonalFreeleech::No || (bookmark.vip && data.is_vip(now))
}
