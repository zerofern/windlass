//! Plain-language descriptions of events and actions for the history log.

use std::time::SystemTime;

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::core::{
    Action, Event, HistoryEvent, MamPauseEvent, MamRequestMode, MappingFailure,
    PortForwardingEvent, QbittorrentEvent, QbittorrentFailure, SettingsEvent, StartupEvent,
    StartupMode, TimerName,
};

/// History categories, used for filtering.
pub(super) const CATEGORIES: [(&str, &str); 6] = [
    ("startup", "Startup"),
    ("mam", "MAM requests"),
    ("settings", "Settings"),
    ("history", "History"),
    ("vpn", "VPN"),
    ("qbittorrent", "qBittorrent"),
];

pub(super) const fn category(event: &Event) -> &'static str {
    match event {
        Event::Startup(_) => "startup",
        Event::MamPause(_) => "mam",
        Event::Settings(_) => "settings",
        Event::History(_) => "history",
        Event::PortForwarding(_)
        | Event::TimerExpired {
            name: TimerName::PortLease,
            ..
        } => "vpn",
        Event::Qbittorrent(_)
        | Event::TimerExpired {
            name: TimerName::QbittorrentPoll,
            ..
        } => "qbittorrent",
    }
}

/// Whether an entry is periodic work that succeeded, hidden from history by default.
pub(super) const fn is_routine(event: &Event) -> bool {
    match event {
        Event::TimerExpired { .. }
        | Event::PortForwarding(PortForwardingEvent::Mapped { .. })
        | Event::Qbittorrent(QbittorrentEvent::PreferencesRead(_)) => true,
        Event::Qbittorrent(QbittorrentEvent::Synced(update)) => {
            !update.full && update.torrents.is_empty() && update.removed.is_empty()
        }
        _ => false,
    }
}

const fn timer(name: TimerName) -> &'static str {
    match name {
        TimerName::QbittorrentPoll => "qBittorrent poll",
        TimerName::PortLease => "forwarded-port lease",
    }
}

fn mapping_failure(reason: MappingFailure) -> String {
    match reason {
        MappingFailure::NoResponse => String::from("the gateway did not answer"),
        MappingFailure::Refused { result_code } => {
            format!("the gateway refused with NAT-PMP result {result_code}")
        }
        MappingFailure::Malformed => String::from("the gateway's answer was not valid"),
    }
}

pub(super) fn qbittorrent_failure(reason: QbittorrentFailure) -> String {
    match reason {
        QbittorrentFailure::Unreachable => String::from("qBittorrent did not answer"),
        QbittorrentFailure::Rejected { status } => {
            format!("qBittorrent answered with HTTP status {status}")
        }
        QbittorrentFailure::Malformed => String::from("qBittorrent's answer was not understood"),
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
        Event::TimerExpired { name, .. } => format!("Timer expired: {}", timer(*name)),
        Event::PortForwarding(event) => port_forwarding(*event),
        Event::Qbittorrent(event) => qbittorrent(event),
    }
}

fn port_forwarding(event: PortForwardingEvent) -> String {
    match event {
        PortForwardingEvent::Enable => String::from("Port forwarding started"),
        PortForwardingEvent::Mapped { tcp, udp, lifetime } => format!(
            "Forwarded ports granted: TCP {}, UDP {}, for {} seconds",
            tcp.get(),
            udp.get(),
            lifetime.as_secs()
        ),
        PortForwardingEvent::MappingFailed { reason } => {
            format!("Port forwarding failed: {}", mapping_failure(reason))
        }
    }
}

fn qbittorrent(event: &QbittorrentEvent) -> String {
    match event {
        QbittorrentEvent::Enable => String::from("Connecting to qBittorrent"),
        QbittorrentEvent::Connected { version, .. } => {
            format!("Connected to qBittorrent {version}")
        }
        QbittorrentEvent::ConnectFailed { reason } => {
            format!("Could not connect: {}", qbittorrent_failure(*reason))
        }
        QbittorrentEvent::Synced(update) => format!(
            "qBittorrent update: {} torrents changed, {} removed{}",
            update.torrents.len(),
            update.removed.len(),
            if update.full { " (full list)" } else { "" }
        ),
        QbittorrentEvent::SyncFailed { reason } => {
            format!(
                "Reading qBittorrent failed: {}",
                qbittorrent_failure(*reason)
            )
        }
        QbittorrentEvent::PreferencesRead(preferences) => format!(
            "qBittorrent preferences read: listen port {}, interface {}",
            preferences
                .listen_port
                .map_or_else(|| String::from("none"), |port| port.get().to_string()),
            preferences.interface.as_deref().unwrap_or("any")
        ),
        QbittorrentEvent::PreferencesReadFailed { reason } => format!(
            "Reading qBittorrent preferences failed: {}",
            qbittorrent_failure(*reason)
        ),
        QbittorrentEvent::ListenPortSet => String::from("qBittorrent accepted the new listen port"),
        QbittorrentEvent::ListenPortSetFailed { reason } => format!(
            "Setting qBittorrent's listen port failed: {}",
            qbittorrent_failure(*reason)
        ),
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
        Action::StartTimer { name, deadline, .. } => format!(
            "Start timer: {} in {:.1} seconds",
            timer(*name),
            deadline.after.as_secs_f64()
        ),
        Action::RequestPortMapping { lifetime } => {
            format!("Request forwarded ports for {} seconds", lifetime.as_secs())
        }
        Action::ConnectQbittorrent => String::from("Connect to qBittorrent"),
        Action::SyncQbittorrent { position } => {
            format!("Read qBittorrent changes since update {}", position.0)
        }
        Action::ReadQbittorrentPreferences => String::from("Read qBittorrent preferences"),
        Action::SetQbittorrentListenPort { port } => {
            format!("Set qBittorrent's listen port to {}", port.get())
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

    const PORT: crate::core::Port = match crate::core::Port::new(41000) {
        Some(port) => port,
        None => unreachable!(),
    };

    /// Schedule timers through the core, for their identities.
    fn timer_actions() -> Vec<Action> {
        use std::time::Instant;

        use crate::core::{Core, MappingFailure, PortForwardingEvent};

        let mut core = Core::new();
        let clock = crate::core::ClockReading {
            monotonic: Instant::now(),
            wall: SystemTime::UNIX_EPOCH,
        };
        let mut actions = core.handle(clock, Event::PortForwarding(PortForwardingEvent::Enable));
        actions.extend(core.handle(
            clock,
            Event::PortForwarding(PortForwardingEvent::MappingFailed {
                reason: MappingFailure::NoResponse,
            }),
        ));
        actions
    }

    fn check(entries: &[(Event, &str, bool)]) {
        for (entry, expected_category, routine) in entries {
            assert!(!event(entry).is_empty());
            assert_eq!(category(entry), *expected_category, "{entry:?}");
            assert_eq!(super::is_routine(entry), *routine, "{entry:?}");
        }
    }

    #[test]
    fn vpn_events_are_described_and_classified() -> Result<(), &'static str> {
        use std::time::Duration;

        use crate::core::{MappingFailure, PortForwardingEvent, TimerName};

        let id = timer_actions()
            .iter()
            .find_map(|action| match action {
                Action::StartTimer { id, .. } => Some(*id),
                _ => None,
            })
            .ok_or("no timer")?;
        let mut entries = vec![
            (
                Event::TimerExpired {
                    name: TimerName::PortLease,
                    id,
                },
                "vpn",
                true,
            ),
            (
                Event::TimerExpired {
                    name: TimerName::QbittorrentPoll,
                    id,
                },
                "qbittorrent",
                true,
            ),
            (
                Event::PortForwarding(PortForwardingEvent::Enable),
                "vpn",
                false,
            ),
            (
                Event::PortForwarding(PortForwardingEvent::Mapped {
                    tcp: PORT,
                    udp: PORT,
                    lifetime: Duration::from_mins(1),
                }),
                "vpn",
                true,
            ),
        ];
        for reason in [
            MappingFailure::NoResponse,
            MappingFailure::Refused { result_code: 2 },
            MappingFailure::Malformed,
        ] {
            entries.push((
                Event::PortForwarding(PortForwardingEvent::MappingFailed { reason }),
                "vpn",
                false,
            ));
        }
        check(&entries);
        Ok(())
    }

    #[test]
    fn qbittorrent_events_are_described_and_classified() {
        use crate::core::{
            QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences, SyncUpdate,
        };

        let qbittorrent = |event, routine| (Event::Qbittorrent(event), "qbittorrent", routine);
        let mut entries = vec![
            qbittorrent(QbittorrentEvent::Enable, false),
            qbittorrent(
                QbittorrentEvent::Connected {
                    version: String::from("v5.2.2"),
                    preferences: QbittorrentPreferences::default(),
                },
                false,
            ),
            qbittorrent(QbittorrentEvent::Synced(SyncUpdate::default()), true),
            qbittorrent(
                QbittorrentEvent::Synced(SyncUpdate {
                    full: true,
                    ..SyncUpdate::default()
                }),
                false,
            ),
            qbittorrent(
                QbittorrentEvent::PreferencesRead(QbittorrentPreferences {
                    listen_port: Some(PORT),
                    interface: Some(String::from("wg0")),
                }),
                true,
            ),
            qbittorrent(
                QbittorrentEvent::PreferencesRead(QbittorrentPreferences::default()),
                true,
            ),
            qbittorrent(QbittorrentEvent::ListenPortSet, false),
        ];
        for reason in [
            QbittorrentFailure::Unreachable,
            QbittorrentFailure::Rejected { status: 403 },
            QbittorrentFailure::Malformed,
        ] {
            for failure in [
                QbittorrentEvent::ConnectFailed { reason },
                QbittorrentEvent::SyncFailed { reason },
                QbittorrentEvent::PreferencesReadFailed { reason },
                QbittorrentEvent::ListenPortSetFailed { reason },
            ] {
                entries.push(qbittorrent(failure, false));
            }
        }
        check(&entries);
    }

    #[test]
    fn integration_actions_are_described() {
        use std::time::Duration;

        use crate::core::SyncPosition;

        for entry in [
            Action::RequestPortMapping {
                lifetime: Duration::from_mins(1),
            },
            Action::ConnectQbittorrent,
            Action::SyncQbittorrent {
                position: SyncPosition(3),
            },
            Action::ReadQbittorrentPreferences,
            Action::SetQbittorrentListenPort { port: PORT },
        ]
        .iter()
        .chain(&timer_actions())
        {
            assert!(!action(entry).is_empty());
        }
    }
}
