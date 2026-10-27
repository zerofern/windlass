use std::{error::Error, fmt};

use nutype::nutype;
use secrecy::{ExposeSecret, SecretString};
use serde::{Serialize, Serializer};

use super::{Action, Points};

/// The maximum number of history entries kept before the oldest are removed.
#[nutype(
    validate(greater_or_equal = 100, less_or_equal = 1_000_000),
    default = 10_000,
    derive(
        Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Display, Serialize
    )
)]
pub struct HistoryLimit(u32);

impl HistoryLimit {
    /// The number of entries to keep after trimming, leaving room below the limit.
    #[must_use]
    pub fn trim_target(self) -> EntryCount {
        EntryCount::new(u64::from(self.into_inner()) * 9 / 10)
    }

    /// Whether the given number of retained entries exceeds this limit.
    #[must_use]
    pub fn is_exceeded_by(self, retained: EntryCount) -> bool {
        retained.get() > u64::from(self.into_inner())
    }
}

/// Bonus points never spent: every purchase leaves at least this many.
#[nutype(
    validate(less_or_equal = 100_000_000),
    default = 20_000,
    derive(
        Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Display, Serialize
    )
)]
pub struct PointsReserve(u64);

impl PointsReserve {
    /// The reserve as points.
    #[must_use]
    pub fn points(self) -> Points {
        Points::new(self.into_inner())
    }
}

/// How many torrents may download at once.
#[nutype(
    validate(greater_or_equal = 1, less_or_equal = 100),
    default = 5,
    derive(
        Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Display, Serialize
    )
)]
pub struct DownloadingLimit(u32);

/// How many torrents may be active at once: downloading, or seeding while peers
/// download from them. Seeds nobody downloads from do not count.
#[nutype(
    validate(greater_or_equal = 1, less_or_equal = 1_000),
    default = 10,
    derive(
        Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Display, Serialize
    )
)]
pub struct ActiveLimit(u32);

/// How many torrents may download, and be active, at once. Downloading
/// torrents are active, so the active limit is never below the downloading one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TorrentLimits {
    downloading: DownloadingLimit,
    active: ActiveLimit,
}

/// Why submitted torrent limits were rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FewerActiveThanDownloading;

impl fmt::Display for FewerActiveThanDownloading {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("Downloading torrents are active too, so allow at least as many active.")
    }
}

impl Error for FewerActiveThanDownloading {}

impl TorrentLimits {
    /// Limits with at least as many active torrents as downloading ones.
    ///
    /// # Errors
    /// Returns an error when fewer torrents may be active than may download.
    pub fn new(
        downloading: DownloadingLimit,
        active: ActiveLimit,
    ) -> Result<Self, FewerActiveThanDownloading> {
        if active.into_inner() < downloading.into_inner() {
            return Err(FewerActiveThanDownloading);
        }
        Ok(Self {
            downloading,
            active,
        })
    }

    /// How many torrents may download at once.
    #[must_use]
    pub const fn downloading(self) -> DownloadingLimit {
        self.downloading
    }

    /// How many torrents may be active at once.
    #[must_use]
    pub const fn active(self) -> ActiveLimit {
        self.active
    }
}

/// A number of history entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
pub struct EntryCount(u64);

impl EntryCount {
    /// Wrap a count of entries.
    #[must_use]
    pub const fn new(count: u64) -> Self {
        Self(count)
    }

    /// The number of entries.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(super) const fn plus(self, count: u64) -> Self {
        Self(self.0.saturating_add(count))
    }

    pub(super) const fn minus(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

/// The MAM `mam_id` API cookie. Its value is never displayed or recorded.
#[derive(Clone)]
pub struct ApiCookie(SecretString);

/// Why a submitted API cookie was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidApiCookie {
    /// Nothing was submitted.
    Empty,
    /// The value contains whitespace or characters not allowed in a cookie value.
    InvalidCharacter,
}

impl fmt::Display for InvalidApiCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "Enter the mam_id value.",
            Self::InvalidCharacter => {
                "The value contains spaces or characters that cannot appear in a cookie."
            }
        })
    }
}

impl Error for InvalidApiCookie {}

impl ApiCookie {
    /// Validate a submitted cookie value, ignoring surrounding whitespace.
    ///
    /// # Errors
    /// Returns why the value cannot be a `mam_id` cookie value.
    pub fn parse(value: &str) -> Result<Self, InvalidApiCookie> {
        let value = value.trim();
        if value.is_empty() {
            return Err(InvalidApiCookie::Empty);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b';' | b',' | b'"' | b'\\'))
        {
            return Err(InvalidApiCookie::InvalidCharacter);
        }
        Ok(Self(SecretString::from(value)))
    }

    /// The cookie value, for the shell to store or send. Never display it.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for ApiCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiCookie([redacted])")
    }
}

impl PartialEq for ApiCookie {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret() == other.expose_secret()
    }
}

impl Eq for ApiCookie {}

impl Serialize for ApiCookie {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[redacted]")
    }
}

/// The ntfy topic notifications are published to. Anyone who knows it can read
/// them, so it is recorded only as `[redacted]`.
#[derive(Clone)]
pub struct NtfyTopic(SecretString);

/// Why a submitted ntfy topic was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidNtfyTopic {
    /// Nothing was submitted.
    Empty,
    /// The topic is longer than 64 characters or uses characters ntfy refuses.
    Invalid,
}

impl fmt::Display for InvalidNtfyTopic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "Enter a topic name.",
            Self::Invalid => "Use up to 64 letters, digits, hyphens, and underscores.",
        })
    }
}

impl Error for InvalidNtfyTopic {}

impl NtfyTopic {
    /// Validate a topic name, ignoring surrounding whitespace.
    ///
    /// # Errors
    /// Returns why the value cannot be an ntfy topic.
    pub fn parse(value: &str) -> Result<Self, InvalidNtfyTopic> {
        let value = value.trim();
        if value.is_empty() {
            return Err(InvalidNtfyTopic::Empty);
        }
        if value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(InvalidNtfyTopic::Invalid);
        }
        Ok(Self(SecretString::from(value)))
    }

    /// The topic, for the shell to store or publish to.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for NtfyTopic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NtfyTopic([redacted])")
    }
}

impl PartialEq for NtfyTopic {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret() == other.expose_secret()
    }
}

impl Eq for NtfyTopic {}

impl Serialize for NtfyTopic {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[redacted]")
    }
}

/// Whether a setting's latest requested value is durably saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingStatus {
    /// The requested value is saved.
    Saved,
    /// A save is outstanding.
    Saving,
    /// The last save failed; the previously saved value remains stored.
    NotSaved,
}

/// Storage activity for one setting, including the exact value being saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingPersistence<T> {
    /// No save is outstanding.
    Idle,
    /// The shell is saving this value.
    Pending(T),
    /// The last save failed.
    Failed,
}

/// A setting's saved value, the user's latest request, and storage activity.
///
/// Only one save is outstanding at a time. A newer request made during a save
/// is saved after that save completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting<T> {
    /// The value confirmed by a successful read or save.
    pub saved: T,
    /// The value most recently requested by the user.
    pub requested: T,
    /// The save in progress, or the outcome of the last one.
    pub persistence: SettingPersistence<T>,
}

impl<T: Clone + PartialEq> Setting<T> {
    const fn loaded(saved: T, requested: T) -> Self {
        Self {
            saved,
            requested,
            persistence: SettingPersistence::Idle,
        }
    }

    /// Whether the latest requested value is saved.
    #[must_use]
    pub const fn status(&self) -> SettingStatus {
        match self.persistence {
            SettingPersistence::Idle => SettingStatus::Saved,
            SettingPersistence::Pending(_) => SettingStatus::Saving,
            SettingPersistence::Failed => SettingStatus::NotSaved,
        }
    }

    fn request(&mut self, value: T) -> Option<T> {
        self.requested = value.clone();
        if matches!(self.persistence, SettingPersistence::Pending(_)) {
            return None;
        }
        if value == self.saved {
            self.persistence = SettingPersistence::Idle;
            return None;
        }
        self.persistence = SettingPersistence::Pending(value.clone());
        Some(value)
    }

    /// Saving a replacement failed: the saved value stays in use, unless the
    /// owner asked for another meanwhile.
    fn replacement_failed(&mut self) -> Option<T> {
        match &self.persistence {
            SettingPersistence::Pending(pending) if *pending == self.requested => {
                self.requested = self.saved.clone();
                self.persistence = SettingPersistence::Idle;
                None
            }
            _ => self.complete(false),
        }
    }

    fn complete(&mut self, succeeded: bool) -> Option<T> {
        let SettingPersistence::Pending(pending) = &self.persistence else {
            return None;
        };
        let pending = pending.clone();
        if succeeded {
            self.saved = pending.clone();
        }
        self.persistence = if succeeded {
            SettingPersistence::Idle
        } else {
            SettingPersistence::Failed
        };
        if pending == self.requested {
            None
        } else {
            self.request(self.requested.clone())
        }
    }
}

/// Settings edited from the dashboard and saved in `SQLite`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The maximum number of history entries kept.
    pub history_limit: Setting<HistoryLimit>,
    /// The MAM API cookie, if one is configured.
    pub api_cookie: Setting<Option<ApiCookie>>,
    /// The ntfy topic for notifications, if one is configured.
    pub ntfy_topic: Setting<Option<NtfyTopic>>,
    /// Bonus points every purchase leaves.
    pub points_reserve: Setting<PointsReserve>,
    /// How many torrents may download, and be active, at once.
    pub torrent_limits: Setting<TorrentLimits>,
}

impl Settings {
    /// The API cookie that may be used: saved, with no replacement pending or failed.
    ///
    /// A replacement must be saved before use, and a failed replacement blocks
    /// use of the old cookie.
    #[must_use]
    pub const fn usable_api_cookie(&self) -> Option<&ApiCookie> {
        match (&self.api_cookie.persistence, &self.api_cookie.saved) {
            (SettingPersistence::Idle, Some(cookie)) => Some(cookie),
            _ => None,
        }
    }
}

/// Whether settings have been loaded from storage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SettingsState {
    /// Storage has not been opened yet.
    #[default]
    Unread,
    /// Waiting for the shell to read saved settings.
    Reading,
    /// Settings are loaded and may be changed.
    Loaded(Settings),
    /// Saved settings could not be read; changes are refused.
    ReadFailed,
}

impl SettingsState {
    /// The loaded settings, if available.
    #[must_use]
    pub const fn loaded(&self) -> Option<&Settings> {
        match self {
            Self::Loaded(settings) => Some(settings),
            Self::Unread | Self::Reading | Self::ReadFailed => None,
        }
    }

    /// The saved points reserve, once settings are loaded.
    #[must_use]
    pub fn points_reserve(&self) -> Option<Points> {
        self.loaded()
            .map(|settings| settings.points_reserve.saved.points())
    }

    /// The saved torrent limits, once settings are loaded.
    #[must_use]
    pub fn torrent_limits(&self) -> Option<TorrentLimits> {
        self.loaded().map(|settings| settings.torrent_limits.saved)
    }

    /// The saved history limit, once settings are loaded.
    #[must_use]
    pub fn history_limit(&self) -> Option<HistoryLimit> {
        self.loaded().map(|settings| settings.history_limit.saved)
    }

    pub(super) fn start_reading(&mut self) -> Vec<Action> {
        *self = Self::Reading;
        vec![Action::ReadSettings]
    }
}

/// A settings command from the dashboard or a settings storage result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SettingsEvent {
    /// Saved settings were read; absent values use their defaults.
    Read {
        /// The saved history limit, if any.
        history_limit: Option<HistoryLimit>,
        /// The saved API cookie, if any.
        api_cookie: Option<ApiCookie>,
        /// The saved ntfy topic, if any.
        ntfy_topic: Option<NtfyTopic>,
        /// The saved points reserve, if any.
        points_reserve: Option<PointsReserve>,
        /// The saved torrent limits, if any.
        torrent_limits: Option<TorrentLimits>,
    },
    /// Saved settings could not be read.
    ReadFailed,
    /// The user changed the history limit.
    SetHistoryLimit {
        /// The requested limit.
        limit: HistoryLimit,
    },
    /// The history limit was saved.
    HistoryLimitWritten,
    /// Saving the history limit failed.
    HistoryLimitWriteFailed,
    /// The user supplied a replacement API cookie.
    SetApiCookie {
        /// The replacement cookie.
        cookie: ApiCookie,
    },
    /// The user removed the API cookie.
    ClearApiCookie,
    /// The API cookie change was saved.
    ApiCookieWritten,
    /// Saving the API cookie change failed.
    ApiCookieWriteFailed,
    /// The user set the ntfy topic.
    SetNtfyTopic {
        /// The topic.
        topic: NtfyTopic,
    },
    /// The user removed the ntfy topic, stopping notifications.
    ClearNtfyTopic,
    /// The ntfy topic change was saved.
    NtfyTopicWritten,
    /// Saving the ntfy topic change failed.
    NtfyTopicWriteFailed,
    /// The user set the points reserve.
    SetPointsReserve {
        /// The reserve.
        reserve: PointsReserve,
    },
    /// The points reserve was saved.
    PointsReserveWritten,
    /// Saving the points reserve failed.
    PointsReserveWriteFailed,
    /// The user set the torrent limits.
    SetTorrentLimits {
        /// The limits.
        limits: TorrentLimits,
    },
    /// The torrent limits were saved.
    TorrentLimitsWritten,
    /// Saving the torrent limits failed.
    TorrentLimitsWriteFailed,
    /// A cookie MAM issued to replace the saved one was saved.
    RotatedApiCookieWritten,
    /// Saving a cookie MAM issued failed; the saved one stays in use.
    RotatedApiCookieWriteFailed,
}

pub(super) fn handle(state: &mut SettingsState, event: SettingsEvent) -> Vec<Action> {
    let write_limit = |limit| Action::WriteHistoryLimit { limit };
    let write_cookie = |cookie| Action::WriteApiCookie { cookie };
    let write_topic = |topic| Action::WriteNtfyTopic { topic };
    let write_reserve = |reserve| Action::WritePointsReserve { reserve };
    let write_limits = |limits| Action::WriteTorrentLimits { limits };
    match event {
        SettingsEvent::Read {
            history_limit,
            api_cookie,
            ntfy_topic,
            points_reserve,
            torrent_limits,
        } => {
            if matches!(state, SettingsState::Reading) {
                let limit = history_limit.unwrap_or_default();
                let reserve = points_reserve.unwrap_or_default();
                let limits = torrent_limits.unwrap_or_default();
                *state = SettingsState::Loaded(Settings {
                    history_limit: Setting::loaded(limit, limit),
                    api_cookie: Setting::loaded(api_cookie.clone(), api_cookie),
                    ntfy_topic: Setting::loaded(ntfy_topic.clone(), ntfy_topic),
                    points_reserve: Setting::loaded(reserve, reserve),
                    torrent_limits: Setting::loaded(limits, limits),
                });
            }
            Vec::new()
        }
        SettingsEvent::ReadFailed => {
            if matches!(state, SettingsState::Reading) {
                *state = SettingsState::ReadFailed;
            }
            Vec::new()
        }
        SettingsEvent::SetHistoryLimit { limit } => change(state, |settings| {
            settings.history_limit.request(limit).map(write_limit)
        }),
        SettingsEvent::HistoryLimitWritten => change(state, |settings| {
            settings.history_limit.complete(true).map(write_limit)
        }),
        SettingsEvent::HistoryLimitWriteFailed => change(state, |settings| {
            settings.history_limit.complete(false).map(write_limit)
        }),
        SettingsEvent::SetApiCookie { cookie } => change(state, |settings| {
            settings.api_cookie.request(Some(cookie)).map(write_cookie)
        }),
        SettingsEvent::ClearApiCookie => change(state, |settings| {
            settings.api_cookie.request(None).map(write_cookie)
        }),
        SettingsEvent::ApiCookieWritten => change(state, |settings| {
            settings.api_cookie.complete(true).map(write_cookie)
        }),
        SettingsEvent::ApiCookieWriteFailed => change(state, |settings| {
            settings.api_cookie.complete(false).map(write_cookie)
        }),
        SettingsEvent::SetNtfyTopic { topic } => change(state, |settings| {
            settings.ntfy_topic.request(Some(topic)).map(write_topic)
        }),
        SettingsEvent::ClearNtfyTopic => change(state, |settings| {
            settings.ntfy_topic.request(None).map(write_topic)
        }),
        SettingsEvent::NtfyTopicWritten => change(state, |settings| {
            settings.ntfy_topic.complete(true).map(write_topic)
        }),
        SettingsEvent::NtfyTopicWriteFailed => change(state, |settings| {
            settings.ntfy_topic.complete(false).map(write_topic)
        }),
        SettingsEvent::SetPointsReserve { reserve } => change(state, |settings| {
            settings.points_reserve.request(reserve).map(write_reserve)
        }),
        SettingsEvent::PointsReserveWritten => change(state, |settings| {
            settings.points_reserve.complete(true).map(write_reserve)
        }),
        SettingsEvent::PointsReserveWriteFailed => change(state, |settings| {
            settings.points_reserve.complete(false).map(write_reserve)
        }),
        SettingsEvent::SetTorrentLimits { limits } => change(state, |settings| {
            settings.torrent_limits.request(limits).map(write_limits)
        }),
        SettingsEvent::TorrentLimitsWritten => change(state, |settings| {
            settings.torrent_limits.complete(true).map(write_limits)
        }),
        SettingsEvent::TorrentLimitsWriteFailed => change(state, |settings| {
            settings.torrent_limits.complete(false).map(write_limits)
        }),
        SettingsEvent::RotatedApiCookieWritten => change(state, |settings| {
            settings.api_cookie.complete(true).map(write_cookie)
        }),
        SettingsEvent::RotatedApiCookieWriteFailed => change(state, |settings| {
            settings.api_cookie.replacement_failed().map(write_cookie)
        }),
    }
}

/// Save a cookie MAM issued in place of the saved one. MAM issues one only on
/// a request sent with the saved cookie, which no change by the owner is
/// pending against.
pub(super) fn rotate(state: &mut SettingsState, cookie: ApiCookie) -> Vec<Action> {
    change(state, |settings| {
        settings
            .api_cookie
            .request(Some(cookie))
            .flatten()
            .map(|cookie| Action::WriteRotatedApiCookie { cookie })
    })
}

/// Apply a change to loaded settings; changes are refused until settings load.
fn change(
    state: &mut SettingsState,
    apply: impl FnOnce(&mut Settings) -> Option<Action>,
) -> Vec<Action> {
    match state {
        SettingsState::Loaded(settings) => apply(settings).into_iter().collect(),
        SettingsState::Unread | SettingsState::Reading | SettingsState::ReadFailed => Vec::new(),
    }
}
