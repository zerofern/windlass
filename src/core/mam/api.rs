//! What Windlass asks MAM, and what MAM answers, as the shell reports it.
//!
//! The shapes follow the responses the owner captured (B-0011). Display text,
//! such as MAM's `ratio` or a search result's `size`, is never carried here.
//! The shell parses each answer into these types, so every value here is known
//! to be valid.

use std::{
    net::IpAddr,
    time::{Duration, SystemTime},
};

use derive_more::Display;
use nutype::nutype;
use serde::Serialize;

use super::super::{ByteSize, ClientVersion, MamTorrentId, Ratio, TorrentHash};

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
        start: BookmarkCount,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct Points(u64);

impl Points {
    /// This many points.
    #[must_use]
    pub const fn new(points: u64) -> Self {
        Self(points)
    }

    /// The number of points.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// These points and those, saturating.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// These points less those, or none when those are more.
    #[must_use]
    pub const fn less(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// These points this many times, saturating.
    #[must_use]
    pub const fn times(self, count: u64) -> Self {
        Self(self.0.saturating_mul(count))
    }

    /// Points from MAM's balance, which may carry a fraction; a negative or
    /// non-finite balance is not one.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the balance is checked to be finite and non-negative; the cast saturates"
    )]
    pub fn from_balance(balance: f64) -> Option<Self> {
        (balance.is_finite() && balance >= 0.0).then(|| Self(balance.floor() as u64))
    }
}

/// The name of the network an address belongs to, such as `ProtonVPN Proton AG`.
#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct NetworkName(String);

/// A torrent's title on MAM, for the dashboard only; notices never carry it.
#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct BookTitle(String);

/// MAM's name for a snatch summary category, such as `Duplicate peer entries`.
#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct CategoryName(String);

/// What a red snatch summary category is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RedKind {
    /// Unsatisfied or hit-and-run torrents that are not seeding; these have
    /// their own watch.
    NotSeeding,
    /// Anything else MAM flags, such as duplicate peers.
    Other,
}

/// MAM's name for a member class Windlass does not know by name.
#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct ClassName(String);

/// What MAM said when it refused a request.
#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct MamMessage(String);

/// Torrents MAM counts as unsatisfied: downloaded but not yet seeded 72 hours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct UnsatisfiedCount(u32);

impl UnsatisfiedCount {
    /// A count of unsatisfied torrents.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// This count with `more` torrents added.
    #[must_use]
    pub const fn plus(self, more: Self) -> Self {
        Self(self.0.saturating_add(more.0))
    }
}

/// The most unsatisfied torrents the account's class may hold, as MAM reports
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct UnsatisfiedLimit(u32);

impl UnsatisfiedLimit {
    /// A limit as MAM reports it.
    #[must_use]
    pub const fn new(limit: u32) -> Self {
        Self(limit)
    }

    /// The limit.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whether one more torrent still leaves `reserve` slots free below it.
    #[must_use]
    pub const fn admits_one_more(self, count: UnsatisfiedCount, reserve: UnsatisfiedCount) -> bool {
        count.0.saturating_add(1).saturating_add(reserve.0) <= self.0
    }

    /// Whether the count has reached 80% of a limit above zero.
    #[must_use]
    pub const fn nearly_reached_by(self, count: UnsatisfiedCount) -> bool {
        self.0 > 0 && (count.0 as u64) * 5 >= (self.0 as u64) * 4
    }
}

/// Torrents in one snatch summary category, or summed over several.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct TorrentCount(u32);

impl TorrentCount {
    /// A count of torrents.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    /// The number of torrents among these, saturating.
    pub fn counting<T>(torrents: impl Iterator<Item = T>) -> Self {
        Self(u32::try_from(torrents.count()).unwrap_or(u32::MAX))
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// This count and another together.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

/// Notifications MAM has for the owner, of one kind or in total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct NoticeCount(u32);

impl NoticeCount {
    /// A count of notifications.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// This count and another together.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

/// Freeleech wedges the account holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct WedgeCount(u32);

impl WedgeCount {
    /// A count of wedges.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    /// Whether none is left.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

/// The owner's bookmarks, as MAM counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct BookmarkCount(u32);

impl BookmarkCount {
    /// A count of bookmarks.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    /// The number of bookmarks in this list, or `None` past what MAM counts.
    #[must_use]
    pub fn of(listed: &[Bookmark]) -> Option<Self> {
        u32::try_from(listed.len()).ok().map(Self)
    }

    /// Whether a list this long holds them all.
    #[must_use]
    pub fn is_reached_by(self, listed: usize) -> bool {
        usize::try_from(self.0).is_ok_and(|count| listed >= count)
    }
}

/// The account's member class.
#[derive(Debug, Clone, PartialEq, Eq, Display, Serialize)]
pub enum UserClass {
    /// Mouse.
    Mouse,
    /// User.
    User,
    /// Power User.
    #[display("Power User")]
    PowerUser,
    /// VIP.
    #[display("VIP")]
    Vip,
    /// Elite VIP.
    #[display("Elite VIP")]
    EliteVip,
    /// Another class, such as a staff class.
    Other(ClassName),
}

impl UserClass {
    /// Read MAM's `classname`; text that is empty is not a class.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.trim() {
            "Mouse" => Self::Mouse,
            "User" => Self::User,
            "Power User" => Self::PowerUser,
            "VIP" => Self::Vip,
            "Elite VIP" => Self::EliteVip,
            other => Self::Other(ClassName::try_new(other).ok()?),
        })
    }
}

/// An autonomous system number, which MAM uses to lock API sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Display, Serialize)]
#[display("AS{_0}")]
pub struct Asn(u32);

impl Asn {
    /// The system with this number.
    #[must_use]
    pub const fn new(number: u32) -> Self {
        Self(number)
    }
}

/// The address MAM sees, and the network it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpObservation {
    /// The address.
    pub ip: IpAddr,
    /// Its autonomous system.
    pub asn: Asn,
    /// The network's name, when MAM gives one.
    pub provider: Option<NetworkName>,
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
    pub name: CategoryName,
    /// What it is about.
    pub kind: RedKind,
    /// How many torrents it holds.
    pub count: TorrentCount,
}

/// Whether MAM sees a partial download.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum Partials {
    /// It does not.
    #[default]
    NotSeen,
    /// It does.
    Seen,
}

/// MAM's breakdown of the account's torrents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnatchSummary {
    /// Torrents MAM counts as unsatisfied.
    pub unsatisfied: UnsatisfiedCount,
    /// The most unsatisfied torrents the account's class may hold.
    pub unsatisfied_limit: UnsatisfiedLimit,
    /// Unsatisfied torrents that are not seeding.
    pub unsatisfied_not_seeding: TorrentCount,
    /// Hit-and-run torrents that are not seeding.
    pub hit_and_run_not_seeding: TorrentCount,
    /// Whether MAM sees a partial download.
    pub partial: Partials,
    /// Duplicate peer entries.
    pub duplicates: TorrentCount,
    /// Important tracker errors.
    pub tracker_errors: TorrentCount,
    /// Whether MAM can reach the client.
    pub connectable: Connectable,
    /// Categories MAM marks red that hold torrents.
    pub red: Vec<RedCategory>,
}

/// MAM's notification counts; their text is never read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MamNotices {
    /// Unread private messages.
    pub messages: NoticeCount,
    /// Open support tickets.
    pub tickets: NoticeCount,
    /// Support tickets waiting for the owner.
    pub waiting_tickets: NoticeCount,
    /// Request notices.
    pub requests: NoticeCount,
    /// Forum topic notices.
    pub topics: NoticeCount,
    /// Warnings that MAM is about to stop accepting a client.
    pub about_to_drop_client: NoticeCount,
}

impl MamNotices {
    /// The number of notices of every kind.
    #[must_use]
    pub const fn total(&self) -> NoticeCount {
        self.messages
            .plus(self.tickets)
            .plus(self.waiting_tickets)
            .plus(self.requests)
            .plus(self.topics)
            .plus(self.about_to_drop_client)
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
    pub wedges: WedgeCount,
    /// The account's class.
    pub class: UserClass,
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
        self.uploaded.less(self.downloaded)
    }

    /// The ratio, or `None` when nothing was downloaded.
    #[must_use]
    pub fn ratio(&self) -> Option<Ratio> {
        Ratio::of(self.uploaded, self.downloaded)
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
    pub refusal: ClientRefusal,
}

/// Whether MAM's client list marks a client as not allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ClientRefusal {
    /// The entry does not refuse it.
    #[default]
    NotRefused,
    /// The entry marks it as not allowed.
    Refused,
}

/// Whether a torrent is a VIP torrent, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum VipTorrent {
    /// It is not.
    No,
    /// It is, with no end set.
    Indefinitely,
    /// It is until this time.
    Until(SystemTime),
}

impl VipTorrent {
    /// Whether it is still a VIP torrent at this time.
    #[must_use]
    pub fn at(self, time: SystemTime) -> bool {
        match self {
            Self::No => false,
            Self::Indefinitely => true,
            Self::Until(end) => end > time,
        }
    }
}

/// Whether a torrent is freeleech for everyone, site-wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Freeleech {
    /// It is not.
    No,
    /// It is.
    ForEveryone,
}

/// Whether the owner snatched a torrent before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Snatched {
    /// Never.
    Never,
    /// Before; MAM's flag lags 5 to 20 minutes.
    Before,
}

/// A bookmarked torrent as Torrent Search lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bookmark {
    /// The torrent.
    pub id: MamTorrentId,
    /// Its title, for the dashboard only; notices never carry it.
    pub title: BookTitle,
    /// Whether it is a VIP torrent.
    pub vip: VipTorrent,
    /// Whether it is freeleech for everyone.
    pub freeleech: Freeleech,
    /// Whether it is freeleech for the owner alone; this lags a wedge.
    pub personal: PersonalFreeleech,
    /// Whether the owner snatched it before.
    pub snatched: Snatched,
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
    pub found: BookmarkCount,
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
        message: Option<MamMessage>,
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
