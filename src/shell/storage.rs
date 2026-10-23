use std::{
    error::Error,
    fs::OpenOptions,
    io,
    path::Path,
    time::{Duration, SystemTime},
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params, types::Type};

use crate::core::{
    ApiCookie, ByteSize, Desired, Event, HistoryLimit, Identity, MamPauseEvent, MamRequestMode,
    MamTorrentId, Observation, Origin, Progress, Ratio, RecordId, RegistryEvent, SettingsEvent,
    StartupEvent, StartupMode, Torrent, TorrentHash, TorrentRecord, TorrentState,
};

use super::qbittorrent::torrent_state;

/// The schema version written by this build. Version 1 held only the MAM
/// preference; version 2 added settings and history; version 3 marks routine
/// history entries; version 4 adds the torrent registry and VPN state.
const SCHEMA_VERSION: i64 = 4;

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

const SCHEMA_V3: &str = "ALTER TABLE history ADD COLUMN routine INTEGER NOT NULL DEFAULT 0;";

/// Registry rows hold intent, then the latest observation, which is present
/// when `last_seen` is set. Times are Unix seconds.
const SCHEMA_V4: &str = "CREATE TABLE torrents (
    id INTEGER PRIMARY KEY,
    mam_id INTEGER,
    hash TEXT UNIQUE,
    origin TEXT NOT NULL CHECK (origin IN ('bookmark', 'redownload', 'adopted')),
    desired TEXT NOT NULL CHECK (desired IN ('queued', 'active', 'removed')),
    save_path TEXT,
    deletion_allowed INTEGER NOT NULL,
    name TEXT,
    state TEXT,
    progress_permille INTEGER,
    size INTEGER,
    total_size INTEGER,
    downloaded INTEGER,
    uploaded INTEGER,
    ratio_thousandths INTEGER,
    seeding_seconds INTEGER,
    added_at INTEGER,
    completed_at INTEGER,
    content_path TEXT,
    observed_save_path TEXT,
    last_seen INTEGER,
    CHECK (mam_id IS NOT NULL OR hash IS NOT NULL)
);
CREATE TABLE vpn_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    last_switch_server TEXT NOT NULL,
    last_switch_at INTEGER NOT NULL
);";

type StorageResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug)]
pub(super) struct FailedAction {
    pub event: Event,
    pub detail: String,
}

/// The core actions that read or write durable control state.
#[derive(Debug)]
pub(super) enum Operation {
    OpenStorage(StartupMode),
    ReadMamRequestMode,
    WriteMamRequestMode(MamRequestMode),
    DeleteMamRequestMode,
    ReadSettings,
    WriteHistoryLimit(HistoryLimit),
    WriteApiCookie(Option<ApiCookie>),
    ReadTorrentRegistry,
    SaveTorrents(Vec<TorrentRecord>),
}

pub(super) fn execute(path: &Path, operation: Operation) -> Result<Event, FailedAction> {
    let (result, failure) = match operation {
        Operation::OpenStorage(mode) => (
            open(path, mode),
            Event::Startup(StartupEvent::StorageOpenFailed),
        ),
        Operation::ReadMamRequestMode => (read(path), Event::MamPause(MamPauseEvent::ReadFailed)),
        Operation::WriteMamRequestMode(mode) => (
            write(path, mode),
            Event::MamPause(MamPauseEvent::WriteFailed),
        ),
        Operation::DeleteMamRequestMode => {
            (delete(path), Event::MamPause(MamPauseEvent::DeleteFailed))
        }
        Operation::ReadSettings => (
            read_settings(path),
            Event::Settings(SettingsEvent::ReadFailed),
        ),
        Operation::WriteHistoryLimit(limit) => (
            write_history_limit(path, limit),
            Event::Settings(SettingsEvent::HistoryLimitWriteFailed),
        ),
        Operation::WriteApiCookie(cookie) => (
            write_api_cookie(path, cookie.as_ref()),
            Event::Settings(SettingsEvent::ApiCookieWriteFailed),
        ),
        Operation::ReadTorrentRegistry => (
            read_registry(path),
            Event::Registry(RegistryEvent::ReadFailed),
        ),
        Operation::SaveTorrents(records) => (
            save_torrents(path, &records),
            Event::Registry(RegistryEvent::SaveFailed),
        ),
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
                "BEGIN; {SCHEMA_V1} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))?;
        }
        StartupMode::Resume => {
            let connection = connection(path)?;
            connection.prepare("SELECT request_mode FROM mam_configuration LIMIT 0")?;
            let version: i64 =
                connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
            let upgrade = match version {
                0 | 1 => {
                    connection.pragma_update(None, "journal_mode", "WAL")?;
                    Some(format!("{SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4}"))
                }
                2 => Some(format!("{SCHEMA_V3} {SCHEMA_V4}")),
                3 => Some(String::from(SCHEMA_V4)),
                SCHEMA_VERSION => None,
                _ => return Err(invalid_data("storage was written by a newer Windlass").into()),
            };
            if let Some(upgrade) = upgrade {
                connection.execute_batch(&format!(
                    "BEGIN; {upgrade} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
                ))?;
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

const TORRENT_COLUMNS: &str = "id, mam_id, hash, origin, desired, save_path, deletion_allowed, \
    name, state, progress_permille, size, total_size, downloaded, uploaded, ratio_thousandths, \
    seeding_seconds, added_at, completed_at, content_path, observed_save_path, last_seen";

fn read_registry(path: &Path) -> StorageResult<Event> {
    let connection = connection(path)?;
    let mut statement = connection.prepare(&format!(
        "SELECT {TORRENT_COLUMNS} FROM torrents ORDER BY id"
    ))?;
    let records = statement
        .query_map([], |row| Ok(torrent_record(row)))?
        .map(|record| record?)
        .collect::<StorageResult<Vec<_>>>()?;
    Ok(Event::Registry(RegistryEvent::Read(records)))
}

fn torrent_record(row: &Row<'_>) -> StorageResult<TorrentRecord> {
    let id: i64 = row.get(0)?;
    let id = u64::try_from(id)
        .ok()
        .and_then(RecordId::new)
        .ok_or_else(|| invalid_data("invalid torrent row id"))?;
    let mam: Option<i64> = row.get(1)?;
    let mam = mam
        .map(|mam| {
            u64::try_from(mam)
                .ok()
                .and_then(MamTorrentId::new)
                .ok_or_else(|| invalid_data("invalid MAM torrent ID"))
        })
        .transpose()?;
    let hash: Option<String> = row.get(2)?;
    let hash = hash
        .map(|hash| TorrentHash::parse(&hash).ok_or_else(|| invalid_data("invalid info hash")))
        .transpose()?;
    let identity =
        Identity::new(mam, hash).ok_or_else(|| invalid_data("torrent row without identity"))?;
    let origin = match row.get::<_, String>(3)?.as_str() {
        "bookmark" => Origin::Bookmark,
        "redownload" => Origin::Redownload,
        "adopted" => Origin::Adopted,
        _ => return Err(invalid_data("invalid torrent origin").into()),
    };
    let desired = match row.get::<_, String>(4)?.as_str() {
        "queued" => Desired::Queued,
        "active" => Desired::Active,
        "removed" => Desired::Removed,
        _ => return Err(invalid_data("invalid desired torrent state").into()),
    };
    let last_seen: Option<i64> = row.get(20)?;
    let observation = last_seen
        .map(|last_seen| observation(row, last_seen))
        .transpose()?;
    Ok(TorrentRecord {
        id,
        identity,
        origin,
        desired,
        save_path: row.get(5)?,
        deletion_allowed: row.get(6)?,
        observation,
    })
}

fn observation(row: &Row<'_>, last_seen: i64) -> StorageResult<Observation> {
    let count = |column| -> StorageResult<u64> {
        let count: i64 = row.get(column)?;
        Ok(u64::try_from(count)?)
    };
    let time = |column| -> StorageResult<Option<SystemTime>> {
        let seconds: Option<i64> = row.get(column)?;
        seconds.map(time_from_seconds).transpose()
    };
    let state: String = row.get(8)?;
    let progress = u16::try_from(count(9)?)
        .ok()
        .and_then(Progress::from_permille)
        .ok_or_else(|| invalid_data("invalid progress"))?;
    Ok(Observation {
        torrent: Torrent {
            name: row.get(7)?,
            state: torrent_state(&state),
            progress,
            size: ByteSize(count(10)?),
            total_size: ByteSize(count(11)?),
            downloaded: ByteSize(count(12)?),
            uploaded: ByteSize(count(13)?),
            ratio: Ratio(count(14)?),
            seeding_time: Duration::from_secs(count(15)?),
            added_at: time(16)?,
            completed_at: time(17)?,
            content_path: row.get(18)?,
            save_path: row.get(19)?,
        },
        last_seen: time_from_seconds(last_seen)?,
    })
}

fn save_torrents(path: &Path, records: &[TorrentRecord]) -> StorageResult<Event> {
    let mut connection = connection(path)?;
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare(&format!(
            "INSERT OR REPLACE INTO torrents({TORRENT_COLUMNS}) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
             ?19, ?20, ?21)"
        ))?;
        for record in records {
            let observed = record.observation.as_ref();
            let torrent = observed.map(|observation| &observation.torrent);
            let count = |count: fn(&Torrent) -> u64| torrent.map(|torrent| integer(count(torrent)));
            let time =
                |time: fn(&Torrent) -> Option<SystemTime>| torrent.and_then(time).map(seconds);
            statement.execute(params![
                integer(record.id.get()),
                record.identity.mam_id().map(|mam| integer(mam.get())),
                record.identity.hash().map(TorrentHash::as_str),
                match record.origin {
                    Origin::Bookmark => "bookmark",
                    Origin::Redownload => "redownload",
                    Origin::Adopted => "adopted",
                },
                match record.desired {
                    Desired::Queued => "queued",
                    Desired::Active => "active",
                    Desired::Removed => "removed",
                },
                record.save_path,
                record.deletion_allowed,
                torrent.map(|torrent| torrent.name.as_str()),
                torrent.map(|torrent| state_name(torrent.state)),
                torrent.map(|torrent| torrent.progress.permille()),
                count(|torrent| torrent.size.0),
                count(|torrent| torrent.total_size.0),
                count(|torrent| torrent.downloaded.0),
                count(|torrent| torrent.uploaded.0),
                count(|torrent| torrent.ratio.0),
                count(|torrent| torrent.seeding_time.as_secs()),
                time(|torrent| torrent.added_at),
                time(|torrent| torrent.completed_at),
                torrent.and_then(|torrent| torrent.content_path.as_deref()),
                torrent.and_then(|torrent| torrent.save_path.as_deref()),
                observed.map(|observation| seconds(observation.last_seen)),
            ])?;
        }
    }
    transaction.commit()?;
    Ok(Event::Registry(RegistryEvent::Saved))
}

/// A qBittorrent state name that reads back as the same state.
const fn state_name(state: TorrentState) -> &'static str {
    match state {
        TorrentState::FetchingMetadata => "metaDL",
        TorrentState::Downloading => "downloading",
        TorrentState::DownloadStalled => "stalledDL",
        TorrentState::DownloadQueued => "queuedDL",
        TorrentState::Seeding => "uploading",
        TorrentState::SeedStalled => "stalledUP",
        TorrentState::SeedQueued => "queuedUP",
        TorrentState::Stopped { complete: true } => "stoppedUP",
        TorrentState::Stopped { complete: false } => "stoppedDL",
        TorrentState::Checking => "checkingDL",
        TorrentState::Moving => "moving",
        TorrentState::MissingFiles => "missingFiles",
        TorrentState::Error => "error",
        TorrentState::Unknown => "unknown",
    }
}

/// `SQLite` integers are signed; counts beyond `i64::MAX`, far past any real
/// size or identity, are saved as the maximum.
fn integer(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Unix seconds; a clock set before 1970 is saved as 1970.
fn seconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| integer(since.as_secs()))
}

fn time_from_seconds(seconds: i64) -> StorageResult<SystemTime> {
    u64::try_from(seconds)
        .ok()
        .and_then(|seconds| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .ok_or_else(|| invalid_data("time out of range").into())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use crate::core::{
        ByteSize, Desired, Event, Identity, MamTorrentId, Observation, Origin, Progress, Ratio,
        RecordId, RegistryEvent, StartupMode, Torrent, TorrentHash, TorrentRecord, TorrentState,
    };

    use super::{Operation, execute, state_name, torrent_state};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Compile-time constants: their unreachable branches are not run code.
    macro_rules! constant {
        ($name:ident: $type:ty = $value:expr) => {
            const $name: $type = match $value {
                Some(value) => value,
                None => unreachable!(),
            };
        };
    }
    constant!(FIRST: RecordId = RecordId::new(1));
    constant!(SECOND: RecordId = RecordId::new(2));
    constant!(THIRD: RecordId = RecordId::new(3));
    constant!(MAM: MamTorrentId = MamTorrentId::new(77));
    constant!(DONE: Progress = Progress::from_permille(1000));

    fn records() -> Result<Vec<TorrentRecord>, &'static str> {
        let (Some(hash), Some(v2_hash)) = (
            TorrentHash::parse(&"a".repeat(40)),
            TorrentHash::parse(&"b".repeat(64)),
        ) else {
            return Err("invalid hash");
        };
        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        let observed = Observation {
            torrent: Torrent {
                name: String::from("A Synthetic Audiobook"),
                state: TorrentState::Stopped { complete: true },
                progress: DONE,
                size: ByteSize(10),
                total_size: ByteSize(12),
                downloaded: ByteSize(10),
                uploaded: ByteSize(30),
                ratio: Ratio(3000),
                seeding_time: Duration::from_hours(1),
                added_at: Some(at(1_700_000_000)),
                completed_at: Some(at(1_700_000_600)),
                content_path: Some(String::from("/srv/books/A Synthetic Audiobook")),
                save_path: Some(String::from("/srv/books")),
            },
            last_seen: at(1_700_001_000),
        };
        let row = |id, identity, origin, desired, observation| TorrentRecord {
            id,
            identity,
            origin,
            desired,
            save_path: Some(String::from("/srv/books")),
            deletion_allowed: id == SECOND,
            observation,
        };
        Ok(vec![
            row(
                FIRST,
                Identity::Client(hash),
                Origin::Adopted,
                Desired::Active,
                Some(observed),
            ),
            row(
                SECOND,
                Identity::Mam(MAM),
                Origin::Bookmark,
                Desired::Queued,
                None,
            ),
            row(
                THIRD,
                Identity::Linked {
                    mam: MAM,
                    hash: v2_hash,
                },
                Origin::Redownload,
                Desired::Removed,
                None,
            ),
        ])
    }

    #[test]
    fn registry_rows_round_trip() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("windlass.sqlite");
        let _ = execute(&path, Operation::OpenStorage(StartupMode::Initialise));
        let rows = records()?;
        assert_eq!(
            execute(&path, Operation::SaveTorrents(rows.clone())).ok(),
            Some(Event::Registry(RegistryEvent::Saved))
        );
        // Saving again replaces rows rather than adding them.
        let _ = execute(&path, Operation::SaveTorrents(rows.clone()));
        assert_eq!(
            execute(&path, Operation::ReadTorrentRegistry).ok(),
            Some(Event::Registry(RegistryEvent::Read(rows)))
        );
        Ok(())
    }

    #[test]
    fn invalid_registry_rows_fail_the_read() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("windlass.sqlite");
        let _ = execute(&path, Operation::OpenStorage(StartupMode::Initialise));
        let _ = execute(&path, Operation::SaveTorrents(records()?));
        let connection = rusqlite::Connection::open(&path)?;
        // Keep the valid rows to restore before each corruption.
        connection.execute_batch("CREATE TABLE pristine AS SELECT * FROM torrents;")?;
        for corruption in [
            "id = 0 WHERE id = 2",
            "mam_id = 0 WHERE id = 2",
            "mam_id = 'x' WHERE id = 2",
            "mam_id = NULL WHERE id = 2",
            "hash = 'xyz' WHERE id = 1",
            "hash = x'00' WHERE id = 1",
            "origin = 'other' WHERE id = 1",
            "origin = x'00' WHERE id = 1",
            "desired = 'other' WHERE id = 1",
            "desired = x'00' WHERE id = 1",
            "save_path = x'00' WHERE id = 1",
            "deletion_allowed = 'x' WHERE id = 1",
            "last_seen = 'x' WHERE id = 1",
            "last_seen = -1 WHERE id = 1",
            "name = x'00' WHERE id = 1",
            "state = x'00' WHERE id = 1",
            "progress_permille = 1001 WHERE id = 1",
            "progress_permille = 'x' WHERE id = 1",
            "size = -1 WHERE id = 1",
            "total_size = -1 WHERE id = 1",
            "downloaded = -1 WHERE id = 1",
            "uploaded = -1 WHERE id = 1",
            "ratio_thousandths = -1 WHERE id = 1",
            "seeding_seconds = -1 WHERE id = 1",
            "added_at = -1 WHERE id = 1",
            "added_at = 'x' WHERE id = 1",
            "completed_at = -1 WHERE id = 1",
            "content_path = x'00' WHERE id = 1",
            "observed_save_path = x'00' WHERE id = 1",
        ] {
            // Bypass the schema's own checks, as a damaged file would.
            connection.execute_batch(&format!(
                "DELETE FROM torrents; INSERT INTO torrents SELECT * FROM pristine; \
                 PRAGMA ignore_check_constraints = ON; UPDATE torrents SET {corruption};"
            ))?;
            let failure = execute(&path, Operation::ReadTorrentRegistry).err();
            assert_eq!(
                failure.map(|failure| failure.event),
                Some(Event::Registry(RegistryEvent::ReadFailed)),
                "{corruption}"
            );
        }
        Ok(())
    }

    #[test]
    fn state_names_read_back_as_the_same_state() {
        for state in [
            TorrentState::FetchingMetadata,
            TorrentState::Downloading,
            TorrentState::DownloadStalled,
            TorrentState::DownloadQueued,
            TorrentState::Seeding,
            TorrentState::SeedStalled,
            TorrentState::SeedQueued,
            TorrentState::Stopped { complete: true },
            TorrentState::Stopped { complete: false },
            TorrentState::Checking,
            TorrentState::Moving,
            TorrentState::MissingFiles,
            TorrentState::Error,
            TorrentState::Unknown,
        ] {
            assert_eq!(torrent_state(state_name(state)), state);
        }
    }

    #[test]
    fn out_of_range_values_are_saved_at_their_limits() {
        assert_eq!(
            super::seconds(SystemTime::UNIX_EPOCH - Duration::from_secs(1)),
            0
        );
        assert_eq!(super::integer(u64::MAX), i64::MAX);
    }

    #[test]
    fn missing_registry_storage_fails() -> TestResult {
        let directory = tempfile::tempdir()?;
        let missing = directory.path().join("missing.sqlite");
        let path = directory.path().join("windlass.sqlite");
        let _ = execute(&path, Operation::OpenStorage(StartupMode::Initialise));
        rusqlite::Connection::open(&path)?.execute_batch("DROP TABLE torrents;")?;
        for path in [&missing, &path] {
            let failure = execute(path, Operation::ReadTorrentRegistry).err();
            assert_eq!(
                failure.map(|failure| failure.event),
                Some(Event::Registry(RegistryEvent::ReadFailed))
            );
            let failure = execute(path, Operation::SaveTorrents(records()?)).err();
            assert_eq!(
                failure.map(|failure| failure.event),
                Some(Event::Registry(RegistryEvent::SaveFailed))
            );
        }
        Ok(())
    }
}
