use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, SystemTime},
};

use serde::Serialize;

use super::{Action, ClockReading, Port, TimerId, TimerName, timer::Timers};

/// How often qBittorrent's state is read.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Preferences are re-read after this many polls, to notice outside changes.
const POLLS_PER_PREFERENCES_READ: u8 = 6;

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

    const fn new(major: u32, minor: u32, patch: u32) -> Self {
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
    Stopped {
        /// Whether all selected content was downloaded.
        complete: bool,
    },
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
    /// Progress from a fraction between 0 and 1; out-of-range values are clamped.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped to 0..=1000 before conversion"
    )]
    pub fn from_fraction(fraction: f64) -> Self {
        Self((fraction.clamp(0.0, 1.0) * 1000.0).round() as u16)
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
pub struct ByteSize(pub u64);

/// An upload ratio in thousandths, such as 1500 for 1.5.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Ratio(pub u64);

impl Ratio {
    /// A ratio from qBittorrent's floating-point value; negative values are zero.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped to be non-negative, and ratios far beyond u64 are not real"
    )]
    pub fn from_float(ratio: f64) -> Self {
        Self((ratio.max(0.0) * 1000.0).round() as u64)
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
    /// The torrent's display name.
    name: String,
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
    content_path: Option<String>,
    /// The directory the content is saved under.
    save_path: Option<String>,
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
}

/// qBittorrent's position in its stream of incremental updates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SyncPosition(pub u64);

/// One incremental update of qBittorrent's torrent list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncUpdate {
    /// The position to request next.
    pub position: SyncPosition,
    /// Whether this update replaces the whole list.
    pub full: bool,
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
    pub interface: Option<String>,
}

/// Why a qBittorrent request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum QbittorrentFailure {
    /// No connection, or no answer in time.
    Unreachable,
    /// qBittorrent answered with an unexpected HTTP status.
    Rejected {
        /// The HTTP status code.
        status: u16,
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
        /// The application version, such as `v5.2.2`.
        version: String,
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
    /// qBittorrent accepted a registry command.
    CommandDone,
    /// A registry command failed.
    CommandFailed {
        /// Why.
        reason: QbittorrentFailure,
    },
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
    pub version: String,
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
    polls_since_preferences: u8,
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
                let approval = ClientVersion::parse(&version)
                    .map_or(MamApproval::NotAllowed, ClientVersion::mam_approval);
                *self = Self::Connected(Box::new(Session {
                    version,
                    approval,
                    preferences,
                    torrents: BTreeMap::new(),
                    activity: Activity::Syncing,
                    position: SyncPosition::default(),
                    listed: false,
                    polls_since_preferences: 0,
                }));
                vec![Action::SyncQbittorrent {
                    position: SyncPosition::default(),
                }]
            }
            (Self::Connected(session), QbittorrentEvent::Synced(update))
                if session.activity == Activity::Syncing =>
            {
                session.apply(update);
                session.polls_since_preferences = session.polls_since_preferences.saturating_add(1);
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
                session.polls_since_preferences = 0;
                session.wait(now, timers)
            }
            (Self::Connected(session), QbittorrentEvent::ListenPortSet)
                if matches!(session.activity, Activity::SettingListenPort(_)) =>
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

    /// Choose the next request: the listen port first, then an update, or after
    /// one a registry command, preferences, or the next poll.
    pub(super) fn decide(
        &mut self,
        now: ClockReading,
        timers: &mut Timers,
        forwarded_port: Option<Port>,
        command: impl FnOnce(&Session) -> Option<TorrentCommand>,
    ) -> Vec<Action> {
        let Self::Connected(session) = self else {
            return Vec::new();
        };
        let Activity::Deciding { after_sync } = session.activity else {
            return Vec::new();
        };
        if let Some(actions) = session.set_listen_port(forwarded_port) {
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

    /// Move qBittorrent to the forwarded port as soon as it is idle.
    pub(super) fn follow_forwarded_port(&mut self, forwarded_port: Option<Port>) -> Vec<Action> {
        match self {
            Self::Connected(session) if matches!(session.activity, Activity::Waiting(_)) => {
                session.set_listen_port(forwarded_port).unwrap_or_default()
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
        if update.full {
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

    fn set_listen_port(&mut self, forwarded_port: Option<Port>) -> Option<Vec<Action>> {
        let port = forwarded_port.filter(|port| self.preferences.listen_port != Some(*port))?;
        self.activity = Activity::SettingListenPort(port);
        Some(vec![Action::SetQbittorrentListenPort { port }])
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
