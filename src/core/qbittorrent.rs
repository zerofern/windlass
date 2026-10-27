use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, SystemTime},
};

use derive_more::Display;
use nutype::nutype;
use serde::Serialize;

use super::{
    Action, ClockReading, HttpStatus, MamTorrentId, Port, TimerId, TimerName, TorrentCount,
    TorrentLimits, timer::Timers,
};

/// How often qBittorrent's state is read.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Preferences are re-read after this many polls, to notice outside changes.
const POLLS_PER_PREFERENCES_READ: Polls = Polls(6);

/// Polls since qBittorrent's preferences were last read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Polls(u8);

impl Polls {
    const NONE: Self = Self(0);

    const fn plus_one(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A qBittorrent release version, such as 5.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ClientVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl ClientVersion {
    /// Parse a release version as reported by qBittorrent, such as `v5.2.2`.
    /// Alpha, beta, development, and other non-release versions are refused.
    #[must_use]
    pub fn parse(version: &str) -> Option<Self> {
        let number = |part: &str| part.parse::<u32>().ok();
        match *version
            .trim()
            .trim_start_matches('v')
            .split('.')
            .collect::<Vec<_>>()
            .as_slice()
        {
            [major, minor] => Some(Self::new(number(major)?, number(minor)?, 0)),
            [major, minor, patch] => {
                Some(Self::new(number(major)?, number(minor)?, number(patch)?))
            }
            _ => None,
        }
    }

    /// A release version from its parts.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Whether MAM allows this version on its tracker.
    #[must_use]
    pub fn mam_approval(self) -> MamApproval {
        let within = |low: Self, high: Self| low <= self && self <= high;
        if self.major == 5 && self.minor == 2 {
            MamApproval::Recommended
        } else if within(Self::new(5, 0, 1), Self::new(5, 2, u32::MAX)) {
            MamApproval::Allowed
        } else if within(Self::new(4, 5, 2), Self::new(4, 6, 7))
            || within(Self::new(4, 3, 4), Self::new(4, 4, 5))
        {
            MamApproval::Discouraged
        } else {
            MamApproval::NotAllowed
        }
    }
}

impl fmt::Display for ClientVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The version qBittorrent reports, read once when Windlass connects.
#[derive(Debug, Clone, PartialEq, Eq, Display, Serialize)]
pub enum ReportedVersion {
    /// A release version, such as 5.2.2.
    Release(ClientVersion),
    /// Anything else, such as an alpha or a development build.
    Unrecognised(VersionText),
}

impl ReportedVersion {
    /// Read the version text; blank text is no version.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        ClientVersion::parse(text).map_or_else(
            || VersionText::try_new(text).ok().map(Self::Unrecognised),
            |version| Some(Self::Release(version)),
        )
    }

    /// The release version, if it is one.
    #[must_use]
    pub const fn release(&self) -> Option<ClientVersion> {
        match self {
            Self::Release(version) => Some(*version),
            Self::Unrecognised(_) => None,
        }
    }

    /// MAM's position on this version; only releases can be allowed.
    #[must_use]
    pub fn mam_approval(&self) -> MamApproval {
        self.release()
            .map_or(MamApproval::NotAllowed, ClientVersion::mam_approval)
    }
}

/// Version text qBittorrent reports that is not a release version.
#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct VersionText(String);

/// A torrent's name in qBittorrent.
#[nutype(
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct TorrentName(String);

/// The directory a torrent's content is saved under.
#[nutype(
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct SavePath(String);

/// Where a torrent's content is stored: its file, or its top directory.
#[nutype(
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct ContentPath(String);

/// The network interface qBittorrent is bound to, such as `wg0`.
#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Display, Serialize)
)]
pub struct NetworkInterface(String);

/// Whether a stopped torrent had downloaded all its selected content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Completion {
    /// All selected content is downloaded.
    Complete,
    /// Some is still missing.
    Incomplete,
}

/// Whether a torrent runs regardless of qBittorrent's queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ForceStart {
    /// The queue manages it.
    #[default]
    NotForced,
    /// It runs regardless of the queue.
    Forced,
}

/// MAM's position on a qBittorrent version, from its allowed client list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MamApproval {
    /// The latest 5.2.x line.
    Recommended,
    /// From 5.0.1 to the latest 5.2.x.
    Allowed,
    /// Permitted, but without HTTPS certificate verification.
    Discouraged,
    /// Unlisted, non-release, or unrecognised; MAM reports an unauthorized client.
    NotAllowed,
}

/// A torrent's info hash: 40 hexadecimal digits for v1, 64 for v2.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct TorrentHash(String);

impl TorrentHash {
    /// Validate a hash, ignoring case.
    #[must_use]
    pub fn parse(hash: &str) -> Option<Self> {
        (matches!(hash.len(), 40 | 64) && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| Self(hash.to_ascii_lowercase()))
    }

    /// The hash in lowercase hexadecimal.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What qBittorrent is doing with a torrent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum TorrentState {
    /// Fetching metadata from peers.
    FetchingMetadata,
    /// Downloading.
    Downloading,
    /// Downloading, but no peers are sending data.
    DownloadStalled,
    /// Waiting for a download slot.
    DownloadQueued,
    /// Complete and uploading.
    Seeding,
    /// Complete; no peers are downloading.
    SeedStalled,
    /// Complete and waiting for an upload slot.
    SeedQueued,
    /// Stopped by the user or an application.
    Stopped(Completion),
    /// Verifying content on disk.
    Checking,
    /// Moving content to another location.
    Moving,
    /// Content is missing from disk.
    MissingFiles,
    /// qBittorrent reports an error.
    Error,
    /// A state this version of Windlass does not recognise.
    #[default]
    Unknown,
}

/// Download progress in tenths of a percent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Progress(u16);

impl Progress {
    /// Progress from a fraction between 0 and 1; anything else is not one.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the fraction is checked to be within 0..=1, so the value is within 0..=1000"
    )]
    pub fn from_fraction(fraction: f64) -> Option<Self> {
        (0.0..=1.0)
            .contains(&fraction)
            .then(|| Self((fraction * 1000.0).round() as u16))
    }

    /// Progress from tenths of a percent; values above 1000 are refused.
    #[must_use]
    pub const fn from_permille(permille: u16) -> Option<Self> {
        if permille <= 1000 {
            Some(Self(permille))
        } else {
            None
        }
    }

    /// Progress in tenths of a percent, from 0 to 1000.
    #[must_use]
    pub const fn permille(self) -> u16 {
        self.0
    }
}

/// A size in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ByteSize(u64);

impl ByteSize {
    /// This many bytes.
    #[must_use]
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// This many GiB, saturating.
    #[must_use]
    pub const fn gib(gib: u64) -> Self {
        Self(gib.saturating_mul(1 << 30))
    }

    /// The number of bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.0
    }

    /// Whether this is no bytes.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// This size less that one, or nothing when that one is larger.
    #[must_use]
    pub const fn less(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

/// An upload ratio, kept in thousandths, such as 1500 for 1.5; shown with two
/// decimals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
#[display("{}.{:02}", _0 / 1000, _0 % 1000 / 10)]
pub struct Ratio(u64);

impl Ratio {
    /// The ratio this many thousandths make, such as 1500 for 1.5.
    #[must_use]
    pub const fn from_thousandths(thousandths: u64) -> Self {
        Self(thousandths)
    }

    /// The ratio in thousandths.
    #[must_use]
    pub const fn thousandths(self) -> u64 {
        self.0
    }

    /// Uploaded over downloaded, or `None` when nothing was downloaded.
    #[must_use]
    pub fn of(uploaded: ByteSize, downloaded: ByteSize) -> Option<Self> {
        (!downloaded.is_zero()).then(|| {
            Self(
                u64::try_from(u128::from(uploaded.0) * 1000 / u128::from(downloaded.0))
                    .unwrap_or(u64::MAX),
            )
        })
    }

    /// A ratio from qBittorrent's floating-point value; a negative or
    /// non-finite value is not one.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is checked to be finite and non-negative; the cast saturates"
    )]
    pub fn from_float(ratio: f64) -> Option<Self> {
        (ratio.is_finite() && ratio >= 0.0).then(|| Self((ratio * 1000.0).round() as u64))
    }
}

/// Generates a torrent's fields and the matching partial update.
macro_rules! torrent_fields {
    ($($(#[$doc:meta])* $field:ident: $type:ty),* $(,)?) => {
        /// A torrent as qBittorrent last reported it.
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
        pub struct Torrent {
            $($(#[$doc])* pub $field: $type,)*
        }

        /// Changed fields of one torrent; absent fields are unchanged.
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
        pub struct TorrentUpdate {
            $($(#[$doc])* pub $field: Option<$type>,)*
        }

        impl Torrent {
            fn apply(&mut self, update: TorrentUpdate) {
                $(if let Some(value) = update.$field {
                    self.$field = value;
                })*
            }
        }
    };
}

torrent_fields! {
    /// The torrent's display name, once qBittorrent reports it.
    name: Option<TorrentName>,
    /// What qBittorrent is doing with it.
    state: TorrentState,
    /// How much of the selected content is downloaded.
    progress: Progress,
    /// The size of the selected files.
    size: ByteSize,
    /// The size of all files; larger than `size` when some are not selected.
    total_size: ByteSize,
    /// Bytes of selected content downloaded so far.
    downloaded: ByteSize,
    /// Bytes uploaded to peers in total.
    uploaded: ByteSize,
    /// Uploaded bytes divided by downloaded bytes.
    ratio: Ratio,
    /// Time spent seeding the complete torrent.
    seeding_time: Duration,
    /// When the torrent was added.
    added_at: Option<SystemTime>,
    /// When the download completed.
    completed_at: Option<SystemTime>,
    /// Where the torrent's content is stored.
    content_path: Option<ContentPath>,
    /// The directory the content is saved under.
    save_path: Option<SavePath>,
    /// Whether it runs regardless of qBittorrent's queue.
    force_start: ForceStart,
}

/// A change qBittorrent must make so that it matches the torrent registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TorrentCommand {
    /// Start these stopped torrents.
    Start(Vec<TorrentHash>),
    /// Select every file of this torrent; MAM does not allow partial sharing.
    SelectAllFiles(TorrentHash),
    /// Remove these torrents and their files.
    Remove(Vec<TorrentHash>),
    /// Stop these torrents.
    Stop(Vec<TorrentHash>),
    /// Run these torrents regardless of qBittorrent's queue.
    ForceStart(Vec<TorrentHash>),
    /// Let qBittorrent's queue manage these torrents again.
    EndForceStart(Vec<TorrentHash>),
    /// Add this fetched torrent from its kept torrent file.
    Add {
        /// The torrent on MAM, whose file Windlass kept.
        mam: MamTorrentId,
        /// Its info hash.
        hash: TorrentHash,
    },
}

/// qBittorrent's position in its stream of incremental updates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Display, Serialize)]
pub struct SyncPosition(u64);

impl SyncPosition {
    /// The position qBittorrent named.
    #[must_use]
    pub const fn new(position: u64) -> Self {
        Self(position)
    }

    /// The number qBittorrent is asked for changes since.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Whether an update replaces the whole torrent list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum SyncScope {
    /// Only what changed since the last update.
    #[default]
    Changes,
    /// The whole list, replacing what Windlass knew.
    Full,
}

/// One incremental update of qBittorrent's torrent list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncUpdate {
    /// The position to request next.
    pub position: SyncPosition,
    /// Whether this update replaces the whole list.
    pub scope: SyncScope,
    /// New or changed torrents.
    pub torrents: Vec<(TorrentHash, TorrentUpdate)>,
    /// Torrents no longer in qBittorrent.
    pub removed: Vec<TorrentHash>,
}

/// qBittorrent settings Windlass depends on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct QbittorrentPreferences {
    /// The port qBittorrent listens on for peers.
    pub listen_port: Option<Port>,
    /// The network interface qBittorrent is bound to, if any.
    pub interface: Option<NetworkInterface>,
    /// How qBittorrent queues torrents, when it reports it.
    pub queue: Option<QueuePreferences>,
}

/// Whether qBittorrent holds torrents beyond its limits in a queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Queueing {
    /// Torrents beyond the limits wait.
    On,
    /// Every torrent runs.
    Off,
}

/// One of qBittorrent's queue limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, Serialize)]
pub enum QueueLimit {
    /// No limit.
    #[display("no limit")]
    Unlimited,
    /// At most this many torrents.
    #[display("{_0}")]
    At(TorrentCount),
}

/// Whether qBittorrent counts torrents moving almost no data against its
/// limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum IdleTorrents {
    /// They count.
    Counted,
    /// They are left out, so idle seeds keep seeding.
    NotCounted,
}

/// How qBittorrent queues torrents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QueuePreferences {
    /// Whether torrents beyond the limits wait in a queue.
    pub queueing: Queueing,
    /// How many torrents may download at once.
    pub max_downloading: QueueLimit,
    /// How many torrents may be active at once.
    pub max_active: QueueLimit,
    /// How many torrents may seed at once.
    pub max_seeding: QueueLimit,
    /// Whether torrents moving almost no data count against the limits.
    pub idle: IdleTorrents,
}

impl QueuePreferences {
    /// The queue that keeps qBittorrent within these limits. Seeds count
    /// only while peers download from them, so idle seeds keep seeding.
    #[must_use]
    pub fn for_limits(limits: TorrentLimits) -> Self {
        let active = QueueLimit::At(TorrentCount::new(limits.active().into_inner()));
        Self {
            queueing: Queueing::On,
            max_downloading: QueueLimit::At(TorrentCount::new(limits.downloading().into_inner())),
            max_active: active,
            max_seeding: active,
            idle: IdleTorrents::NotCounted,
        }
    }
}

/// Why a qBittorrent request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum QbittorrentFailure {
    /// No connection, or no answer in time.
    Unreachable,
    /// qBittorrent answered with an unexpected HTTP status.
    Rejected {
        /// The HTTP status code.
        status: HttpStatus,
    },
    /// The answer could not be understood.
    Malformed,
}

/// A qBittorrent command or a result from its web UI API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum QbittorrentEvent {
    /// Start watching qBittorrent; sent when its web UI is configured.
    Enable,
    /// The web UI answered with its version and preferences.
    Connected {
        /// The application version.
        version: ReportedVersion,
        /// The current preferences.
        preferences: QbittorrentPreferences,
    },
    /// Connecting failed.
    ConnectFailed {
        /// Why.
        reason: QbittorrentFailure,
    },
    /// An incremental update arrived.
    Synced(SyncUpdate),
    /// Reading an update failed.
    SyncFailed {
        /// Why.
        reason: QbittorrentFailure,
    },
    /// Preferences were read.
    PreferencesRead(QbittorrentPreferences),
    /// Reading preferences failed.
    PreferencesReadFailed {
        /// Why.
        reason: QbittorrentFailure,
    },
    /// qBittorrent accepted a new listen port.
    ListenPortSet,
    /// Setting the listen port failed.
    ListenPortSetFailed {
        /// Why.
        reason: QbittorrentFailure,
    },
    /// qBittorrent accepted new queue settings.
    QueueSet,
    /// Setting the queue failed.
    QueueSetFailed {
        /// Why.
        reason: QbittorrentFailure,
    },
    /// qBittorrent accepted a registry command.
    CommandDone,
    /// A registry command failed.
    CommandFailed {
        /// Why.
        reason: QbittorrentFailure,
    },
}

/// The qBittorrent settings Windlass wants, where it knows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wanted {
    /// The forwarded port, for qBittorrent to listen on.
    pub listen_port: Option<Port>,
    /// The queue that keeps torrents within the owner's limits.
    pub queue: Option<QueuePreferences>,
}

/// The single request outstanding with a connected qBittorrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// Waiting for the poll timer.
    Waiting(TimerId),
    /// Reading an incremental update.
    Syncing,
    /// Reading preferences.
    ReadingPreferences,
    /// Setting the listen port.
    SettingListenPort(Port),
    /// Setting the queue.
    SettingQueue,
    /// Applying a registry command.
    Commanding,
    /// Choosing the next request; never visible outside a step.
    Deciding {
        /// Whether a fresh update was just merged.
        after_sync: bool,
    },
}

/// A connected qBittorrent and what Windlass knows about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The version qBittorrent reported.
    pub version: ReportedVersion,
    /// MAM's position on that version.
    pub approval: MamApproval,
    /// The latest preferences read.
    pub preferences: QbittorrentPreferences,
    /// Every torrent in qBittorrent, by hash.
    pub torrents: BTreeMap<TorrentHash, Torrent>,
    /// The request outstanding now.
    pub activity: Activity,
    position: SyncPosition,
    listed: bool,
    polls_since_preferences: Polls,
}

/// The core's view of qBittorrent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Qbittorrent {
    /// No web UI is configured.
    #[default]
    Disabled,
    /// Waiting for the web UI to answer.
    Connecting,
    /// The last request failed; connecting again when the timer expires.
    Unreachable {
        /// Why the last request failed.
        reason: QbittorrentFailure,
        /// The retry timer.
        retry: TimerId,
    },
    /// Connected and polling.
    Connected(Box<Session>),
}

impl Qbittorrent {
    pub(super) fn handle(
        &mut self,
        now: ClockReading,
        event: QbittorrentEvent,
        timers: &mut Timers,
    ) -> Vec<Action> {
        match (&mut *self, event) {
            (Self::Disabled, QbittorrentEvent::Enable) => {
                *self = Self::Connecting;
                vec![Action::ConnectQbittorrent]
            }
            (
                Self::Connecting,
                QbittorrentEvent::Connected {
                    version,
                    preferences,
                },
            ) => {
                let approval = version.mam_approval();
                *self = Self::Connected(Box::new(Session {
                    version,
                    approval,
                    preferences,
                    torrents: BTreeMap::new(),
                    activity: Activity::Syncing,
                    position: SyncPosition::default(),
                    listed: false,
                    polls_since_preferences: Polls::NONE,
                }));
                vec![Action::SyncQbittorrent {
                    position: SyncPosition::default(),
                }]
            }
            (Self::Connected(session), QbittorrentEvent::Synced(update))
                if session.activity == Activity::Syncing =>
            {
                session.apply(update);
                session.polls_since_preferences = session.polls_since_preferences.plus_one();
                session.activity = Activity::Deciding { after_sync: true };
                Vec::new()
            }
            (Self::Connected(session), QbittorrentEvent::CommandDone)
                if session.activity == Activity::Commanding =>
            {
                // Read the result of the command at once.
                session.activity = Activity::Deciding { after_sync: false };
                Vec::new()
            }
            (Self::Connected(session), QbittorrentEvent::PreferencesRead(preferences))
                if session.activity == Activity::ReadingPreferences =>
            {
                session.preferences = preferences;
                session.polls_since_preferences = Polls::NONE;
                session.wait(now, timers)
            }
            (Self::Connected(session), QbittorrentEvent::ListenPortSet)
                if matches!(session.activity, Activity::SettingListenPort(_)) =>
            {
                // Confirm the change rather than assume it.
                session.activity = Activity::ReadingPreferences;
                vec![Action::ReadQbittorrentPreferences]
            }
            (Self::Connected(session), QbittorrentEvent::QueueSet)
                if session.activity == Activity::SettingQueue =>
            {
                // Confirm the change rather than assume it.
                session.activity = Activity::ReadingPreferences;
                vec![Action::ReadQbittorrentPreferences]
            }
            (Self::Connecting, QbittorrentEvent::ConnectFailed { reason })
            | (
                Self::Connected(_),
                QbittorrentEvent::SyncFailed { reason }
                | QbittorrentEvent::PreferencesReadFailed { reason }
                | QbittorrentEvent::ListenPortSetFailed { reason }
                | QbittorrentEvent::QueueSetFailed { reason }
                | QbittorrentEvent::CommandFailed { reason },
            ) => {
                let (retry, deadline) = timers.schedule(now.monotonic, POLL_INTERVAL);
                *self = Self::Unreachable { reason, retry };
                vec![Action::StartTimer {
                    name: TimerName::QbittorrentPoll,
                    id: retry,
                    deadline,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Poll or reconnect when this timer is the current one.
    pub(super) fn timer_expired(&mut self, id: TimerId) -> Vec<Action> {
        match self {
            Self::Unreachable { retry, .. } if *retry == id => {
                *self = Self::Connecting;
                vec![Action::ConnectQbittorrent]
            }
            Self::Connected(session) if session.activity == Activity::Waiting(id) => {
                session.activity = Activity::Deciding { after_sync: false };
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The session, while it is choosing its next request, and whether a fresh
    /// update was just merged.
    #[must_use]
    pub fn deciding(&self) -> Option<(&Session, bool)> {
        match self {
            Self::Connected(session) => match session.activity {
                Activity::Deciding { after_sync } => Some((session, after_sync)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Choose the next request: the listen port and queue first, then an
    /// update, or after one a registry command, preferences, or the next poll.
    pub(super) fn decide(
        &mut self,
        now: ClockReading,
        timers: &mut Timers,
        wanted: Wanted,
        command: impl FnOnce(&Session) -> Option<TorrentCommand>,
    ) -> Vec<Action> {
        let Self::Connected(session) = self else {
            return Vec::new();
        };
        let Activity::Deciding { after_sync } = session.activity else {
            return Vec::new();
        };
        if let Some(actions) = session.set_preferences(wanted) {
            return actions;
        }
        if !after_sync {
            session.activity = Activity::Syncing;
            return vec![Action::SyncQbittorrent {
                position: session.position,
            }];
        }
        // Commands follow only from a fresh update.
        if let Some(command) = command(session) {
            session.activity = Activity::Commanding;
            return vec![Action::QbittorrentCommand { command }];
        }
        if session.polls_since_preferences >= POLLS_PER_PREFERENCES_READ {
            session.activity = Activity::ReadingPreferences;
            return vec![Action::ReadQbittorrentPreferences];
        }
        session.wait(now, timers)
    }

    /// Move qBittorrent to the wanted settings as soon as it is idle.
    pub(super) fn follow_preferences(&mut self, wanted: Wanted) -> Vec<Action> {
        match self {
            Self::Connected(session) if matches!(session.activity, Activity::Waiting(_)) => {
                session.set_preferences(wanted).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }
}

impl Session {
    /// Whether the torrent list is complete: the first full update has arrived.
    #[must_use]
    pub const fn listed(&self) -> bool {
        self.listed
    }

    fn apply(&mut self, update: SyncUpdate) {
        if update.scope == SyncScope::Full {
            self.torrents.clear();
            self.listed = true;
        }
        for (hash, changes) in update.torrents {
            self.torrents.entry(hash).or_default().apply(changes);
        }
        for hash in update.removed {
            self.torrents.remove(&hash);
        }
        self.position = update.position;
    }

    /// Change the first setting that differs from what Windlass wants: the
    /// listen port, then the queue. A queue qBittorrent does not report is left
    /// alone, since a change could not be confirmed.
    fn set_preferences(&mut self, wanted: Wanted) -> Option<Vec<Action>> {
        if let Some(port) = wanted
            .listen_port
            .filter(|port| self.preferences.listen_port != Some(*port))
        {
            self.activity = Activity::SettingListenPort(port);
            return Some(vec![Action::SetQbittorrentListenPort { port }]);
        }
        let queue = wanted.queue.filter(|queue| {
            self.preferences
                .queue
                .is_some_and(|current| current != *queue)
        })?;
        self.activity = Activity::SettingQueue;
        Some(vec![Action::SetQbittorrentQueue { queue }])
    }

    fn wait(&mut self, now: ClockReading, timers: &mut Timers) -> Vec<Action> {
        let (id, deadline) = timers.schedule(now.monotonic, POLL_INTERVAL);
        self.activity = Activity::Waiting(id);
        vec![Action::StartTimer {
            name: TimerName::QbittorrentPoll,
            id,
            deadline,
        }]
    }
}
