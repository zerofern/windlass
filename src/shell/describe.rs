//! Plain-language descriptions of events and actions for the history log.

use std::time::SystemTime;

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::core::{
    Action, Event, HistoryEvent, MamPauseEvent, MamRequestMode, SettingsEvent, StartupEvent,
    StartupMode,
};

/// History categories, used for filtering.
pub(super) const CATEGORIES: [(&str, &str); 4] = [
    ("startup", "Startup"),
    ("mam", "MAM requests"),
    ("settings", "Settings"),
    ("history", "History"),
];

pub(super) const fn category(event: &Event) -> &'static str {
    match event {
        Event::Startup(_) => "startup",
        Event::MamPause(_) => "mam",
        Event::Settings(_) => "settings",
        Event::History(_) => "history",
    }
}

const fn startup_mode(mode: StartupMode) -> &'static str {
    match mode {
        StartupMode::Initialise => "initialise new storage",
        StartupMode::Resume => "resume existing storage",
    }
}

const fn request_mode(mode: MamRequestMode) -> &'static str {
    match mode {
        MamRequestMode::Paused => "paused",
        MamRequestMode::Enabled => "running",
    }
}

/// Format wall time as UTC RFC 3339; the page converts it to local time.
///
/// Times before 1970 or after 9999 cannot be formatted and are left empty.
pub(super) fn timestamp(time: SystemTime) -> String {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| time::Duration::try_from(since).ok())
        .and_then(|since| OffsetDateTime::UNIX_EPOCH.checked_add(since))
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_default()
}

pub(super) fn event(event: &Event) -> String {
    match event {
        Event::Startup(event) => match event {
            StartupEvent::Start { mode } => format!("Startup requested ({})", startup_mode(*mode)),
            StartupEvent::StorageOpened => String::from("Storage opened"),
            StartupEvent::StorageOpenFailed => String::from("Storage could not be opened"),
        },
        Event::MamPause(event) => mam_pause(*event),
        Event::Settings(event) => settings(event),
        Event::History(event) => history(event),
    }
}

fn mam_pause(event: MamPauseEvent) -> String {
    String::from(match event {
        MamPauseEvent::Set {
            mode: MamRequestMode::Paused,
        } => "Pause of MAM requests requested",
        MamPauseEvent::Set {
            mode: MamRequestMode::Enabled,
        } => "Resume of MAM requests requested",
        MamPauseEvent::Delete => "Removal of the saved MAM request choice requested",
        MamPauseEvent::Read { mode: None } => {
            "MAM request choice read: none saved, so requests are paused"
        }
        MamPauseEvent::Read {
            mode: Some(MamRequestMode::Paused),
        } => "MAM request choice read: paused",
        MamPauseEvent::Read {
            mode: Some(MamRequestMode::Enabled),
        } => "MAM request choice read: running",
        MamPauseEvent::ReadFailed => "MAM request choice could not be read",
        MamPauseEvent::Written => "MAM request choice saved",
        MamPauseEvent::WriteFailed => "MAM request choice could not be saved",
        MamPauseEvent::Deleted => "Saved MAM request choice removed",
        MamPauseEvent::DeleteFailed => "Saved MAM request choice could not be removed",
    })
}

fn settings(event: &SettingsEvent) -> String {
    match event {
        SettingsEvent::Read {
            history_limit,
            api_cookie,
        } => format!(
            "Settings read: history limit {}, API cookie {}",
            history_limit.unwrap_or_default(),
            if api_cookie.is_some() {
                "configured"
            } else {
                "not configured"
            }
        ),
        SettingsEvent::ReadFailed => String::from("Settings could not be read"),
        SettingsEvent::SetHistoryLimit { limit } => {
            format!("History limit change to {limit} entries requested")
        }
        SettingsEvent::HistoryLimitWritten => String::from("History limit saved"),
        SettingsEvent::HistoryLimitWriteFailed => String::from("History limit could not be saved"),
        SettingsEvent::SetApiCookie { .. } => {
            String::from("API cookie replaced from the dashboard (value not recorded)")
        }
        SettingsEvent::ClearApiCookie => String::from("API cookie removal requested"),
        SettingsEvent::ApiCookieWritten => String::from("API cookie change saved"),
        SettingsEvent::ApiCookieWriteFailed => String::from("API cookie change could not be saved"),
    }
}

fn history(event: &HistoryEvent) -> String {
    match event {
        HistoryEvent::Opened { retained } => {
            format!("History opened with {} entries", retained.get())
        }
        HistoryEvent::OpenFailed => String::from("History could not be opened"),
        HistoryEvent::Trimmed { retained } => {
            format!("Oldest history removed; {} entries kept", retained.get())
        }
        HistoryEvent::TrimFailed => String::from("Oldest history could not be removed"),
        HistoryEvent::EntriesLost { count, from, until } => format!(
            "History could not be saved: {} entries are missing, processed from {} to {}",
            count.get(),
            timestamp(*from),
            timestamp(*until)
        ),
    }
}

pub(super) fn action(action: &Action) -> String {
    match action {
        Action::OpenStorage { mode } => format!("Open storage ({})", startup_mode(*mode)),
        Action::ReadMamRequestMode => String::from("Read the saved MAM request choice"),
        Action::WriteMamRequestMode { mode } => {
            format!("Save MAM request choice: {}", request_mode(*mode))
        }
        Action::DeleteMamRequestMode => String::from("Remove the saved MAM request choice"),
        Action::ReadSettings => String::from("Read saved settings"),
        Action::WriteHistoryLimit { limit } => format!("Save history limit: {limit} entries"),
        Action::WriteApiCookie { cookie: Some(_) } => {
            String::from("Save API cookie (value not recorded)")
        }
        Action::WriteApiCookie { cookie: None } => String::from("Remove the saved API cookie"),
        Action::OpenHistory => String::from("Open history storage"),
        Action::TrimHistory { keep } => {
            format!("Remove oldest history, keeping the newest {}", keep.get())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use crate::core::{
        Action, ApiCookie, EntryCount, Event, HistoryEvent, HistoryLimit, MamPauseEvent,
        MamRequestMode, SettingsEvent, StartupEvent, StartupMode,
    };

    use super::{action, category, event};

    #[test]
    fn every_event_and_action_is_described_without_secrets() {
        let cookie = ApiCookie::parse("secret-value").ok();
        let limit = HistoryLimit::default();
        let events = [
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Resume,
            }),
            Event::Startup(StartupEvent::StorageOpened),
            Event::Startup(StartupEvent::StorageOpenFailed),
            Event::MamPause(MamPauseEvent::Set {
                mode: MamRequestMode::Enabled,
            }),
            Event::MamPause(MamPauseEvent::Delete),
            Event::MamPause(MamPauseEvent::Read {
                mode: Some(MamRequestMode::Paused),
            }),
            Event::MamPause(MamPauseEvent::Read {
                mode: Some(MamRequestMode::Enabled),
            }),
            Event::MamPause(MamPauseEvent::ReadFailed),
            Event::MamPause(MamPauseEvent::WriteFailed),
            Event::MamPause(MamPauseEvent::Deleted),
            Event::MamPause(MamPauseEvent::DeleteFailed),
            Event::Settings(SettingsEvent::Read {
                history_limit: Some(limit),
                api_cookie: cookie.clone(),
            }),
            Event::Settings(SettingsEvent::ReadFailed),
            Event::Settings(SettingsEvent::HistoryLimitWritten),
            Event::Settings(SettingsEvent::HistoryLimitWriteFailed),
            Event::Settings(SettingsEvent::ClearApiCookie),
            Event::Settings(SettingsEvent::ApiCookieWriteFailed),
            Event::History(HistoryEvent::OpenFailed),
            Event::History(HistoryEvent::TrimFailed),
            Event::History(HistoryEvent::EntriesLost {
                count: EntryCount::new(2),
                from: SystemTime::UNIX_EPOCH,
                until: SystemTime::UNIX_EPOCH,
            }),
        ];
        let mut descriptions: Vec<String> = events.iter().map(event).collect();
        assert!(descriptions.contains(&String::from("Storage could not be opened")));
        assert!(
            descriptions
                .iter()
                .any(|text| text.contains("1970-01-01T00:00:00Z"))
        );
        assert_eq!(category(&events[0]), "startup");
        let actions = [
            Action::ReadMamRequestMode,
            Action::WriteMamRequestMode {
                mode: MamRequestMode::Paused,
            },
            Action::WriteMamRequestMode {
                mode: MamRequestMode::Enabled,
            },
            Action::DeleteMamRequestMode,
            Action::ReadSettings,
            Action::WriteHistoryLimit { limit },
            Action::WriteApiCookie { cookie },
            Action::WriteApiCookie { cookie: None },
            Action::OpenHistory,
        ];
        descriptions.extend(actions.iter().map(action));
        for description in &descriptions {
            assert!(!description.is_empty());
            assert!(!description.contains("secret-value"), "{description}");
        }
        let far_future = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(u64::MAX / 4);
        assert_eq!(super::timestamp(far_future), "");
        let unique: std::collections::BTreeSet<_> = descriptions.iter().collect();
        assert_eq!(
            unique.len(),
            descriptions.len(),
            "descriptions are distinct"
        );
    }
}
