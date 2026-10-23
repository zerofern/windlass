use std::{error::Error, fmt};

use nutype::nutype;
use secrecy::{ExposeSecret, SecretString};
use serde::{Serialize, Serializer};

use super::Action;

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
}

pub(super) fn handle(state: &mut SettingsState, event: SettingsEvent) -> Vec<Action> {
    let write_limit = |limit| Action::WriteHistoryLimit { limit };
    let write_cookie = |cookie| Action::WriteApiCookie { cookie };
    match event {
        SettingsEvent::Read {
            history_limit,
            api_cookie,
        } => {
            if matches!(state, SettingsState::Reading) {
                let limit = history_limit.unwrap_or_default();
                *state = SettingsState::Loaded(Settings {
                    history_limit: Setting::loaded(limit, limit),
                    api_cookie: Setting::loaded(api_cookie.clone(), api_cookie),
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
    }
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
