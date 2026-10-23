//! Record every handled event and its actions in processing order, and read
//! pages of that history for the dashboard.

use std::{
    error::Error,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use rusqlite::{Connection, params_from_iter, types::Value};
use serde::Serialize;
use tokio::{
    sync::mpsc,
    task::{JoinError, JoinHandle},
};

use crate::core::{Action, EntryCount, Event, HistoryEvent, TimedEvent};

use super::{Input, WorkerCompletion, describe, read_clock, storage};

/// How many entries one history page shows.
pub(super) const PAGE_SIZE: u32 = 50;

/// Entries held in memory until the core opens history storage at startup.
const BUFFER_LIMIT: usize = 1_000;

/// One handled event and the actions the core returned for it.
#[derive(Debug)]
pub(super) struct HistoryEntry {
    pub(super) occurred_at: SystemTime,
    pub(super) processed_at: SystemTime,
    pub(super) event: Event,
    pub(super) actions: Vec<Action>,
}

#[derive(Debug)]
pub(super) enum Command {
    Record(HistoryEntry),
    Open,
    Trim(EntryCount),
}

/// A single writer keeps insertion order equal to processing order.
#[derive(Debug)]
pub(super) struct HistoryWriter {
    commands: mpsc::UnboundedSender<Command>,
    worker: JoinHandle<()>,
}

impl HistoryWriter {
    pub(super) fn start(database: PathBuf, results: mpsc::Sender<Input>) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let worker = tokio::task::spawn_blocking(move || run(&database, receiver, &results));
        Self { commands, worker }
    }

    pub(super) fn send(&self, command: Command) {
        // The writer only stops after this handle is dropped.
        let _ = self.commands.send(command);
    }

    /// Write everything already sent, then stop.
    pub(super) async fn finish(self) -> Result<(), JoinError> {
        drop(self.commands);
        self.worker.await
    }
}

enum Store {
    Buffering,
    Open(Connection),
    Unavailable,
}

impl Store {
    const fn connection(&self) -> Option<&Connection> {
        match self {
            Self::Open(connection) => Some(connection),
            Self::Buffering | Self::Unavailable => None,
        }
    }
}

struct Lost {
    count: u64,
    from: SystemTime,
    until: SystemTime,
}

fn run(
    database: &Path,
    mut commands: mpsc::UnboundedReceiver<Command>,
    results: &mpsc::Sender<Input>,
) {
    let mut store = Store::Buffering;
    let mut buffer = Vec::new();
    let mut lost: Option<Lost> = None;
    while let Some(command) = commands.blocking_recv() {
        match command {
            Command::Record(entry) => {
                if let Some(connection) = store.connection() {
                    record(connection, &entry, &mut lost, results);
                } else if matches!(store, Store::Buffering) && buffer.len() < BUFFER_LIMIT {
                    buffer.push(entry);
                } else {
                    lose(&mut lost, entry.processed_at);
                }
            }
            Command::Open => {
                let opened = open(database).and_then(|connection| {
                    for entry in std::mem::take(&mut buffer) {
                        record(&connection, &entry, &mut lost, results);
                    }
                    let retained = count(&connection)?;
                    Ok((connection, retained))
                });
                match opened {
                    Ok((connection, retained)) => {
                        store = Store::Open(connection);
                        complete(results, HistoryEvent::Opened { retained }, None);
                    }
                    Err(error) => {
                        store = Store::Unavailable;
                        complete(results, HistoryEvent::OpenFailed, Some(error.to_string()));
                    }
                }
            }
            Command::Trim(keep) => {
                let trimmed = store
                    .connection()
                    .ok_or("history storage is not open")
                    .map_err(HistoryError::from)
                    .and_then(|connection| trim(connection, keep));
                match trimmed {
                    Ok(retained) => complete(results, HistoryEvent::Trimmed { retained }, None),
                    Err(error) => {
                        complete(results, HistoryEvent::TrimFailed, Some(error.to_string()));
                    }
                }
            }
        }
    }
}

fn complete(results: &mpsc::Sender<Input>, event: HistoryEvent, storage_error: Option<String>) {
    let completion = WorkerCompletion {
        event: TimedEvent {
            occurred_at: read_clock(),
            event: Event::History(event),
        },
        storage_error,
    };
    // The runner can already have stopped; its final state no longer needs this.
    let _ = results.blocking_send(Input::Completed(Ok(completion)));
}

const fn lose(lost: &mut Option<Lost>, processed_at: SystemTime) {
    match lost {
        Some(lost) => {
            lost.count += 1;
            lost.until = processed_at;
        }
        None => {
            *lost = Some(Lost {
                count: 1,
                from: processed_at,
                until: processed_at,
            });
        }
    }
}

fn record(
    connection: &Connection,
    entry: &HistoryEntry,
    lost: &mut Option<Lost>,
    results: &mpsc::Sender<Input>,
) {
    if insert(connection, entry).is_err() {
        lose(lost, entry.processed_at);
        return;
    }
    if let Some(Lost { count, from, until }) = lost.take() {
        let event = TimedEvent {
            occurred_at: read_clock(),
            event: Event::History(HistoryEvent::EntriesLost {
                count: EntryCount::new(count),
                from,
                until,
            }),
        };
        let _ = results.blocking_send(Input::Event(event));
    }
}

type HistoryError = Box<dyn Error + Send + Sync>;
type HistoryResult<T> = Result<T, HistoryError>;

fn open(database: &Path) -> HistoryResult<Connection> {
    let connection = storage::connection(database)?;
    // History is information, not control state: in WAL mode this keeps the
    // database consistent while letting a power loss drop the newest entries.
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.prepare("SELECT id FROM history LIMIT 0")?;
    Ok(connection)
}

fn count(connection: &Connection) -> HistoryResult<EntryCount> {
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))?;
    Ok(EntryCount::new(u64::try_from(count)?))
}

fn trim(connection: &Connection, keep: EntryCount) -> HistoryResult<EntryCount> {
    connection.execute(
        "DELETE FROM history WHERE id <= (SELECT id FROM history ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [i64::try_from(keep.get())?],
    )?;
    count(connection)
}

fn millis(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A recorded action: its description and structured data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub(super) struct ActionRecord {
    pub(super) summary: String,
    pub(super) detail: serde_json::Value,
}

fn insert(connection: &Connection, entry: &HistoryEntry) -> HistoryResult<()> {
    let actions = entry
        .actions
        .iter()
        .map(|action| {
            Ok(ActionRecord {
                summary: describe::action(action),
                detail: serde_json::to_value(action)?,
            })
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    connection.execute(
        "INSERT INTO history(occurred_at_ms, processed_at_ms, category, summary, event, actions)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            millis(entry.occurred_at),
            millis(entry.processed_at),
            describe::category(&entry.event),
            describe::event(&entry.event),
            serde_json::to_string(&entry.event)?,
            serde_json::to_string(&actions)?,
        ],
    )?;
    Ok(())
}

/// Which history entries to show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct HistoryQuery {
    /// Only entries of this category.
    pub(super) category: Option<&'static str>,
    /// Only entries processed at or after this time.
    pub(super) from: Option<SystemTime>,
    /// Only entries processed at or before this time.
    pub(super) until: Option<SystemTime>,
    /// Which page to show.
    pub(super) page: Page,
}

/// A position in history, newest first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Page {
    /// The newest entries.
    #[default]
    Newest,
    /// Entries older than this entry.
    Before(i64),
    /// Entries newer than this entry.
    After(i64),
}

/// One stored history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HistoryRow {
    pub(super) id: i64,
    pub(super) occurred_at: SystemTime,
    pub(super) processed_at: SystemTime,
    pub(super) category: String,
    pub(super) summary: String,
    pub(super) event: String,
    pub(super) actions: Vec<ActionRecord>,
}

/// A page of entries, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct HistoryPage {
    pub(super) rows: Vec<HistoryRow>,
    pub(super) has_newer: bool,
    pub(super) has_older: bool,
}

fn time_from_millis(millis: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(millis).unwrap_or(0))
}

/// Read one page of history matching the query.
pub(super) fn read_page(database: &Path, query: &HistoryQuery) -> HistoryResult<HistoryPage> {
    let connection = storage::connection(database)?;
    let mut conditions = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    if let Some(category) = query.category {
        conditions.push("category = ?");
        values.push(Value::Text(category.to_owned()));
    }
    if let Some(from) = query.from {
        conditions.push("processed_at_ms >= ?");
        values.push(Value::Integer(millis(from)));
    }
    if let Some(until) = query.until {
        conditions.push("processed_at_ms <= ?");
        values.push(Value::Integer(millis(until)));
    }
    let filter = |extra: Option<&str>| {
        let mut all = conditions.clone();
        all.extend(extra);
        if all.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", all.join(" AND "))
        }
    };
    let exists = |condition: &str, id: i64| -> HistoryResult<bool> {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM history {})",
            filter(Some(condition))
        );
        let mut parameters = values.clone();
        parameters.push(Value::Integer(id));
        Ok(connection.query_row(&sql, params_from_iter(parameters), |row| row.get(0))?)
    };
    let (condition, order, anchor) = match query.page {
        Page::Newest => (None, "DESC", None),
        Page::Before(id) => (Some("id < ?"), "DESC", Some(id)),
        Page::After(id) => (Some("id > ?"), "ASC", Some(id)),
    };
    let sql = format!(
        "SELECT id, occurred_at_ms, processed_at_ms, category, summary, event, actions
         FROM history {} ORDER BY id {order} LIMIT {PAGE_SIZE}",
        filter(condition)
    );
    let mut parameters = values.clone();
    parameters.extend(anchor.map(Value::Integer));
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement
        .query_map(params_from_iter(parameters), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .map(|row| {
            let (id, occurred, processed, category, summary, event, actions) = row?;
            Ok(HistoryRow {
                id,
                occurred_at: time_from_millis(occurred),
                processed_at: time_from_millis(processed),
                category,
                summary,
                event,
                actions: serde_json::from_str(&actions)?,
            })
        })
        .collect::<HistoryResult<Vec<_>>>()?;
    if order == "ASC" {
        rows.reverse();
    }
    let (has_newer, has_older) = match (rows.first(), rows.last()) {
        (Some(newest), Some(oldest)) => {
            (exists("id > ?", newest.id)?, exists("id < ?", oldest.id)?)
        }
        _ => (false, false),
    };
    Ok(HistoryPage {
        rows,
        has_newer,
        has_older,
    })
}
