//! What Windlass asks MAM, and what MAM answers, as the shell reports it.
//!
//! The shapes follow the responses the owner captured (B-0011). Display text,
//! such as MAM's `ratio` or a search result's `size`, is never carried here.

use std::{
    fmt,
    net::IpAddr,
    time::{Duration, SystemTime},
};

use nutype::nutype;
use serde::Serialize;

use super::super::{ByteSize, ClientVersion, MamTorrentId, TorrentHash};

/// A request Windlass makes to MAM. Only one is outstanding at a time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MamRequest {
    /// Ask which IP and ASN MAM sees; needs no cookie.
    LookUpIp,
    /// Register the current IP as the dynamic seedbox IP.
    RegisterSeedbox,
    /// Load the account, its snatch summary, and its notice counts.
    LoadUserData {
        /// Whether to include the client list, which MAM caches for 30 minutes.
        client_stats: bool,
    },
    /// List one page of the owner's bookmarks.
    ListBookmarks {
        /// How many bookmarks to skip.
        start: u32,
    },
    /// Download a torrent file and keep it for qBittorrent.
    FetchTorrent {
        /// The torrent.
        id: MamTorrentId,
        /// Whether to spend a wedge on it.
        wedge: bool,
    },
    /// Buy from the bonus point store.
    Buy(Purchase),
    /// Read the wedges spent on personal freeleech, from User Bonus History.
    WedgeHistory,
    /// Look one torrent up by its ID, for its flags as MAM reports them now.
    LookUpTorrent {
        /// The torrent.
        id: MamTorrentId,
    },
}

impl MamRequest {
    /// Whether the request needs the API cookie.
    #[must_use]
    pub const fn needs_cookie(&self) -> bool {
        !matches!(self, Self::LookUpIp)
    }
}

/// An amount of upload credit in whole GiB, as the store sells it: 50 or more.
#[nutype(
    validate(greater_or_equal = 50),
    derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)
)]
pub struct UploadCredit(u32);

/// A bonus store purchase Windlass can make. Gifts are not expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Purchase {
    /// Upload credit.
    Upload(UploadCredit),
    /// VIP, filled to 90 days from the purchase.
    Vip,
}

/// Bonus points, as a whole number; MAM's fractions are dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Points(pub u64);

impl Points {
    /// Points from MAM's balance, which may carry a fraction.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the balance is clamped to be non-negative, and balances beyond u64 are not real"
    )]
    pub const fn from_balance(balance: f64) -> Self {
        Self(balance.max(0.0).floor() as u64)
    }
}

impl fmt::Display for Points {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// An autonomous system number, which MAM uses to lock API sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Asn(pub u32);

impl fmt::Display for Asn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "AS{}", self.0)
    }
}

/// The address MAM sees, and the network it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpObservation {
    /// The address.
    pub ip: IpAddr,
    /// Its autonomous system.
    pub asn: Asn,
    /// The provider's name, such as `ProtonVPN Proton AG`.
    pub provider: String,
}

/// How MAM answered a seedbox registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Registration {
    /// MAM changed the seedbox IP.
    Completed,
    /// The IP was already the seedbox IP.
    NoChange,
}

/// Whether MAM can reach the owner's clients, as its snatch summary says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub enum Connectable {
    /// MAM reaches the client.
    Yes,
    /// MAM cannot reach the client.
    No,
    /// Nothing is seeding, so MAM has nothing to test.
    #[default]
    Offline,
    /// A value Windlass does not recognise yet.
    Unrecognised(String),
}

impl Connectable {
    /// Read MAM's text, ignoring case.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "yes" | "true" | "connectable" => Self::Yes,
            "no" | "false" | "not connectable" | "unconnectable" => Self::No,
            "offline" => Self::Offline,
            _ => Self::Unrecognised(value.trim().to_owned()),
        }
    }
}

/// A snatch summary category MAM marks red, with torrents in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedCategory {
    /// MAM's name for the category.
    pub name: String,
    /// How many torrents it holds.
    pub count: u32,
}

/// MAM's breakdown of the account's torrents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SnatchSummary {
    /// Torrents MAM counts as unsatisfied.
    pub unsatisfied: u32,
    /// The most unsatisfied torrents the account's class may hold.
    pub unsatisfied_limit: u32,
    /// Unsatisfied torrents that are not seeding.
    pub unsatisfied_not_seeding: u32,
    /// Hit-and-run torrents that are not seeding.
    pub hit_and_run_not_seeding: u32,
    /// Whether MAM sees a partial download.
    pub partial: bool,
    /// Duplicate peer entries.
    pub duplicates: u32,
    /// Important tracker errors.
    pub tracker_errors: u32,
    /// Whether MAM can reach the client.
    pub connectable: Connectable,
    /// Categories MAM marks red that hold torrents.
    pub red: Vec<RedCategory>,
}

/// MAM's notification counts; their text is never read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MamNotices {
    /// Unread private messages.
    pub messages: u32,
    /// Open support tickets.
    pub tickets: u32,
    /// Support tickets waiting for the owner.
    pub waiting_tickets: u32,
    /// Request notices.
    pub requests: u32,
    /// Forum topic notices.
    pub topics: u32,
    /// A warning that MAM is about to stop accepting a client.
    pub about_to_drop_client: u32,
}

impl MamNotices {
    /// The number of notices of every kind.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.messages
            .saturating_add(self.tickets)
            .saturating_add(self.waiting_tickets)
            .saturating_add(self.requests)
            .saturating_add(self.topics)
            .saturating_add(self.about_to_drop_client)
    }
}

/// The account as MAM reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserData {
    /// Bytes uploaded in total.
    pub uploaded: ByteSize,
    /// Bytes downloaded in total.
    pub downloaded: ByteSize,
    /// The bonus point balance.
    pub points: Points,
    /// Freeleech wedges held.
    pub wedges: u32,
    /// The account's class, such as `VIP`.
    pub class: String,
    /// When VIP ends, if the account is VIP.
    pub vip_until: Option<SystemTime>,
    /// The snatch summary, when requested.
    pub summary: Option<SnatchSummary>,
    /// Notification counts, when requested.
    pub notices: Option<MamNotices>,
    /// The clients MAM lists, when requested.
    pub clients: Option<Vec<ListedClient>>,
}

impl UserData {
    /// Uploaded minus downloaded bytes; zero when downloads exceed uploads.
    #[must_use]
    pub const fn buffer(&self) -> ByteSize {
        ByteSize(self.uploaded.0.saturating_sub(self.downloaded.0))
    }

    /// The ratio in thousandths, or `None` when nothing was downloaded.
    #[must_use]
    pub fn ratio_thousandths(&self) -> Option<u64> {
        (self.downloaded.0 > 0).then(|| {
            u64::try_from(u128::from(self.uploaded.0) * 1000 / u128::from(self.downloaded.0))
                .unwrap_or(u64::MAX)
        })
    }

    /// Whether the account is VIP at this time.
    #[must_use]
    pub fn is_vip(&self, now: SystemTime) -> bool {
        self.vip_until.is_some_and(|until| until > now)
    }
}

/// A client MAM lists for the account, as far as Windlass can read the entry.
/// The entry's shape is not yet captured, so each part is optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedClient {
    /// The qBittorrent version MAM names for it, if Windlass finds one.
    pub qbittorrent: Option<ClientVersion>,
    /// Whether MAM can reach it, if the entry says.
    pub connectable: Option<Connectable>,
    /// Whether MAM marks it as not allowed.
    pub refused: bool,
}

/// A bookmarked torrent as Torrent Search lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bookmark {
    /// The torrent.
    pub id: MamTorrentId,
    /// Its title, for the dashboard only; notices never carry it.
    pub title: String,
    /// Whether it is a VIP torrent.
    pub vip: bool,
    /// Whether it is freeleech for everyone.
    pub free: bool,
    /// Whether it is freeleech for the owner alone; this lags a wedge.
    pub personal: PersonalFreeleech,
    /// Whether the owner snatched it before.
    pub snatched: bool,
    /// When its VIP status ends, if it does.
    pub vip_expires: Option<SystemTime>,
    /// When the owner bookmarked it.
    pub bookmarked_at: Option<SystemTime>,
}

/// Whether a torrent is freeleech for the owner alone, as search reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum PersonalFreeleech {
    /// It is not.
    #[default]
    No,
    /// Through the owner's VIP status.
    ThroughVip,
    /// Through a wedge the owner spent.
    ThroughWedge,
}

impl PersonalFreeleech {
    /// Read search's `personal_freeleech` and `fl_vip` flags.
    #[must_use]
    pub const fn from_flags(personal_freeleech: bool, freeleech_for_owner: bool) -> Self {
        match (personal_freeleech, freeleech_for_owner) {
            (true, _) => Self::ThroughWedge,
            (false, true) => Self::ThroughVip,
            (false, false) => Self::No,
        }
    }
}

/// One page of bookmarks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BookmarkPage {
    /// Bookmarks in total.
    pub found: u32,
    /// The bookmarks on this page.
    pub bookmarks: Vec<Bookmark>,
}

/// A torrent file Windlass downloaded and kept for qBittorrent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FetchedTorrent {
    /// The torrent's info hash.
    pub hash: TorrentHash,
    /// The size of all its files.
    pub size: ByteSize,
}

/// A successful answer from MAM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MamReply {
    /// The IP lookup.
    Ip(IpObservation),
    /// The seedbox registration.
    Registered {
        /// What MAM did.
        registration: Registration,
        /// The address MAM registered.
        ip: IpAddr,
    },
    /// The account.
    UserData(Box<UserData>),
    /// A page of bookmarks.
    Bookmarks(BookmarkPage),
    /// A torrent file, kept for qBittorrent.
    Fetched(FetchedTorrent),
    /// A purchase, and the balance after it.
    Purchased {
        /// The points left.
        points: Points,
    },
    /// Wedges spent on personal freeleech, newest first.
    WedgeHistory(Vec<WedgeSpent>),
    /// A torrent looked up by its ID; `None` when MAM does not list it.
    Torrent(Option<Box<Bookmark>>),
}

/// A wedge spent to make a torrent a personal freeleech, as User Bonus History
/// lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WedgeSpent {
    /// The torrent.
    pub torrent: MamTorrentId,
    /// When.
    pub at: SystemTime,
}

/// Why MAM refused a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SessionRejection {
    /// The cookie is missing, invalid, or expired.
    InvalidCookie,
    /// The session is locked to another IP.
    IpMismatch,
    /// The session is locked to other ASNs.
    AsnMismatch,
    /// The session may not register a seedbox.
    NotAllowed,
    /// Another reason.
    Other,
}

/// Why a request did not succeed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MamFailure {
    /// MAM refused the session.
    Session(SessionRejection),
    /// MAM asked Windlass to wait.
    TooSoon {
        /// How long, if MAM said.
        retry_after: Option<Duration>,
    },
    /// MAM refused the request itself: a download without a torrent file, or a
    /// purchase that did not go through.
    Refused {
        /// MAM's message, if it gave one.
        message: Option<String>,
    },
    /// The torrent file arrived but could not be kept.
    NotKept,
    /// No connection, or no answer in time.
    Unreachable,
    /// An unexpected HTTP status.
    Rejected {
        /// The status code.
        status: u16,
    },
    /// The answer could not be understood.
    Malformed,
}

impl MamFailure {
    /// Whether MAM may have acted on the request even though it failed.
    #[must_use]
    pub const fn uncertain(&self) -> bool {
        matches!(
            self,
            Self::Unreachable | Self::Rejected { .. } | Self::Malformed | Self::NotKept
        )
    }
}
