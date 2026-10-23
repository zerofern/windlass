use std::{error::Error, fs::OpenOptions, io, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, types::Type};

use crate::core::{
    Action, ApiCookie, Event, HistoryEvent, HistoryLimit, MamPauseEvent, MamRequestMode,
    SettingsEvent, StartupEvent, StartupMode,
};

/// The schema version written by this build. Version 1 held only the MAM preference.
const SCHEMA_VERSION: i64 = 2;

const SCHEMA_V1: &str = "CREATE TABLE mam_configuration (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    request_mode TEXT NOT NULL CHECK (request_mode IN ('paused', 'enabled'))
);";

const SCHEMA_V2: &str = "CREATE TABLE settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    history_limit INTEGER,
    api_cookie TEXT
);
CREATE TABLE history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at_ms INTEGER NOT NULL,
    processed_at_ms INTEGER NOT NULL,
    category TEXT NOT NULL,
    summary TEXT NOT NULL,
    event TEXT NOT NULL,
    actions TEXT NOT NULL
);
CREATE INDEX history_processed_at ON history(processed_at_ms);";

type StorageResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug)]
pub(super) struct FailedAction {
    pub event: Event,
    pub detail: String,
}

pub(super) fn execute(path: &Path, action: Action) -> Result<Event, FailedAction> {
    let (result, failure) = match action {
        Action::OpenStorage { mode } => (
            open(path, mode),
            Event::Startup(StartupEvent::StorageOpenFailed),
        ),
        Action::ReadMamRequestMode => (read(path), Event::MamPause(MamPauseEvent::ReadFailed)),
        Action::WriteMamRequestMode { mode } => (
            write(path, mode),
            Event::MamPause(MamPauseEvent::WriteFailed),
        ),
        Action::DeleteMamRequestMode => {
            (delete(path), Event::MamPause(MamPauseEvent::DeleteFailed))
        }
        Action::ReadSettings => (
            read_settings(path),
            Event::Settings(SettingsEvent::ReadFailed),
        ),
        Action::WriteHistoryLimit { limit } => (
            write_history_limit(path, limit),
            Event::Settings(SettingsEvent::HistoryLimitWriteFailed),
        ),
        Action::WriteApiCookie { cookie } => (
            write_api_cookie(path, cookie.as_ref()),
            Event::Settings(SettingsEvent::ApiCookieWriteFailed),
        ),
        // The shell routes these to the history writer, which keeps entries in order.
        Action::OpenHistory => (Err(misrouted()), Event::History(HistoryEvent::OpenFailed)),
        Action::TrimHistory { .. } => (Err(misrouted()), Event::History(HistoryEvent::TrimFailed)),
    };
    result.map_err(|error| FailedAction {
        event: failure,
        detail: error.to_string(),
    })
}

pub(super) fn connection(path: &Path) -> StorageResult<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(connection)
}

fn open(path: &Path, mode: StartupMode) -> StorageResult<Event> {
    match mode {
        StartupMode::Initialise => {
            // Exclusive creation prevents a setup command from overwriting an instance.
            OpenOptions::new().write(true).create_new(true).open(path)?;
            let connection = connection(path)?;
            connection.pragma_update(None, "journal_mode", "WAL")?;
            connection.execute_batch(&format!(
                "BEGIN; {SCHEMA_V1} {SCHEMA_V2} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))?;
        }
        StartupMode::Resume => {
            let connection = connection(path)?;
            connection.prepare("SELECT request_mode FROM mam_configuration LIMIT 0")?;
            let version: i64 =
                connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
            match version {
                0 | 1 => {
                    connection.pragma_update(None, "journal_mode", "WAL")?;
                    connection.execute_batch(&format!(
                        "BEGIN; {SCHEMA_V2} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
                    ))?;
                }
                SCHEMA_VERSION => {}
                _ => return Err(invalid_data("storage was written by a newer Windlass").into()),
            }
        }
    }
    Ok(Event::Startup(StartupEvent::StorageOpened))
}

fn read(path: &Path) -> StorageResult<Event> {
    let stored: Option<String> = connection(path)?
        .query_row(
            "SELECT request_mode FROM mam_configuration WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let mode = match stored.as_deref() {
        None => None,
        Some("paused") => Some(MamRequestMode::Paused),
        Some("enabled") => Some(MamRequestMode::Enabled),
        Some(_) => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                Type::Text,
                Box::new(invalid_data("invalid MAM request preference")),
            )
            .into());
        }
    };
    Ok(Event::MamPause(MamPauseEvent::Read { mode }))
}

fn write(path: &Path, mode: MamRequestMode) -> StorageResult<Event> {
    let stored = match mode {
        MamRequestMode::Paused => "paused",
        MamRequestMode::Enabled => "enabled",
    };
    connection(path)?.execute(
        "INSERT INTO mam_configuration(singleton, request_mode) VALUES (1, ?1)
         ON CONFLICT(singleton) DO UPDATE SET request_mode = excluded.request_mode",
        [stored],
    )?;
    Ok(Event::MamPause(MamPauseEvent::Written))
}

fn delete(path: &Path) -> StorageResult<Event> {
    connection(path)?.execute("DELETE FROM mam_configuration WHERE singleton = 1", [])?;
    Ok(Event::MamPause(MamPauseEvent::Deleted))
}

fn misrouted() -> Box<dyn Error + Send + Sync> {
    io::Error::other("history actions run on the history writer").into()
}

fn invalid_data(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}

fn read_settings(path: &Path) -> StorageResult<Event> {
    let stored: Option<(Option<i64>, Option<String>)> = connection(path)?
        .query_row(
            "SELECT history_limit, api_cookie FROM settings WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (limit, cookie) = stored.unwrap_or_default();
    let history_limit = limit
        .map(|limit| {
            u32::try_from(limit)
                .ok()
                .and_then(|limit| HistoryLimit::try_new(limit).ok())
                .ok_or_else(|| invalid_data("invalid history limit"))
        })
        .transpose()?;
    let api_cookie = cookie
        .map(|cookie| ApiCookie::parse(&cookie).map_err(|_| invalid_data("invalid API cookie")))
        .transpose()?;
    Ok(Event::Settings(SettingsEvent::Read {
        history_limit,
        api_cookie,
    }))
}

fn write_history_limit(path: &Path, limit: HistoryLimit) -> StorageResult<Event> {
    connection(path)?.execute(
        "INSERT INTO settings(singleton, history_limit) VALUES (1, ?1)
         ON CONFLICT(singleton) DO UPDATE SET history_limit = excluded.history_limit",
        [i64::from(limit.into_inner())],
    )?;
    Ok(Event::Settings(SettingsEvent::HistoryLimitWritten))
}

fn write_api_cookie(path: &Path, cookie: Option<&ApiCookie>) -> StorageResult<Event> {
    connection(path)?.execute(
        "INSERT INTO settings(singleton, api_cookie) VALUES (1, ?1)
         ON CONFLICT(singleton) DO UPDATE SET api_cookie = excluded.api_cookie",
        [cookie.map(ApiCookie::expose_secret)],
    )?;
    Ok(Event::Settings(SettingsEvent::ApiCookieWritten))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::core::{Action, EntryCount, Event, HistoryEvent};

    use super::execute;

    #[test]
    fn history_actions_are_not_storage_operations() {
        let path = Path::new("unused.sqlite");
        for (action, failure) in [
            (Action::OpenHistory, HistoryEvent::OpenFailed),
            (
                Action::TrimHistory {
                    keep: EntryCount::new(1),
                },
                HistoryEvent::TrimFailed,
            ),
        ] {
            let error = execute(path, action).err().map(|failed| failed.event);
            assert_eq!(error, Some(Event::History(failure)));
        }
        assert!(!path.exists());
    }
}
