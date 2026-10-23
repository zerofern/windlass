use std::{error::Error, fs::OpenOptions, io, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, types::Type};

use crate::core::{Action, Event, MamPauseEvent, MamRequestMode, StartupEvent, StartupMode};

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
    };
    result.map_err(|error| FailedAction {
        event: failure,
        detail: error.to_string(),
    })
}

fn connection(path: &Path) -> StorageResult<Connection> {
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
            connection(path)?.execute_batch(
                "CREATE TABLE mam_configuration (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    request_mode TEXT NOT NULL CHECK (request_mode IN ('paused', 'enabled'))
                );",
            )?;
        }
        StartupMode::Resume => {
            connection(path)?.prepare("SELECT request_mode FROM mam_configuration LIMIT 0")?;
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
                Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid MAM request preference",
                )),
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
