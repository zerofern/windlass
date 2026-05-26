#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::{AlertPriority, MamTorrentId, TorrentHash};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActivityId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlertId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BookId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DownloadId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbCommand {
    RecordActivity(ActivityRecord),
    RecordAlert(AlertRecord),
    SaveSystemSnapshot(SystemSnapshotRecord),
    UpsertTorrent(TorrentRecord),
    UpsertBook(BookRecord),
    EnqueueDownload(DownloadQueueRecord),
    MarkDownloadState(DownloadStateChange),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbEvent {
    ActivityRecorded { id: ActivityId },
    AlertRecorded { id: AlertId },
    SystemSnapshotSaved { id: SnapshotId },
    TorrentUpserted { hash: TorrentHash },
    BookUpserted { id: BookId },
    DownloadQueueUpdated { id: DownloadId },
    Failed(DbFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbFailure {
    pub operation: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivitySource {
    Shell,
    Domain,
    Qbit,
    Mam,
    Vpn,
    Web,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityRecord {
    pub at: DateTime<Utc>,
    pub source: ActivitySource,
    pub action: String,
    pub book_id: Option<BookId>,
    pub detail: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertRecord {
    pub at: DateTime<Utc>,
    pub priority: AlertPriority,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSnapshotRecord {
    pub at: DateTime<Utc>,
    pub state: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TorrentStateRecord {
    Downloading,
    Uploading,
    ForcedUpload,
    PausedDownloading,
    PausedUploading,
    StalledDownloading,
    StalledUploading,
    Checking,
    Error,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentRecord {
    pub hash: TorrentHash,
    pub book_id: Option<BookId>,
    pub mam_id: Option<MamTorrentId>,
    pub name: String,
    pub state: TorrentStateRecord,
    pub seeding_time_secs: i64,
    pub downloaded_bytes: i64,
    pub seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BookStatus {
    PendingMetadata,
    Queued,
    Downloading,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookRecord {
    pub id: Option<BookId>,
    pub mam_id: Option<MamTorrentId>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub status: BookStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Seeding,
    Satisfied,
    Complete,
    Failed,
    Blacklisted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadQueueRecord {
    pub book_id: Option<BookId>,
    pub mam_id: MamTorrentId,
    pub status: DownloadStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadStateChange {
    pub mam_id: MamTorrentId,
    pub status: DownloadStatus,
}

// ── DbMachine ─────────────────────────────────────────────────────────────────

/// Actions the DB shell executes on behalf of the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbAction {
    Execute(DbCommand),
}

/// Facts published by the DB machine through the topic fanout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbPublish {
    Succeeded { operation: String },
    Failed(DbFailure),
}

/// Topic discriminants for `DbPublish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbTopic {
    Results,
    Failures,
}

impl HasTopic<DbTopic> for DbPublish {
    fn topic(&self) -> DbTopic {
        match self {
            Self::Succeeded { .. } => DbTopic::Results,
            Self::Failed(_) => DbTopic::Failures,
        }
    }
}

/// Synchronous response returned by `DbMachine::handle_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbResponse {
    Accepted,
}

/// Stateless sans-I/O machine for database persistence.
///
/// Commands are dispatched to the DB shell, which executes them against
/// Postgres and sends the result back as a timed `DbEvent`. The machine
/// then publishes success or failure facts for subscribers (e.g. the
/// domain machine) to react to.
pub struct DbMachine;

impl Machine for DbMachine {
    type Config = ();
    type Event = DbEvent;
    type Action = DbAction;
    type Publish = DbPublish;
    type Topic = DbTopic;
    type Command = DbCommand;
    type Response = DbResponse;

    fn new(_config: (), _now: Instant) -> Self {
        Self
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        let publish = match event.inner {
            DbEvent::Failed(failure) => DbPublish::Failed(failure),
            DbEvent::ActivityRecorded { .. } => {
                DbPublish::Succeeded { operation: "RecordActivity".to_string() }
            }
            DbEvent::AlertRecorded { .. } => {
                DbPublish::Succeeded { operation: "RecordAlert".to_string() }
            }
            DbEvent::SystemSnapshotSaved { .. } => {
                DbPublish::Succeeded { operation: "SaveSystemSnapshot".to_string() }
            }
            DbEvent::TorrentUpserted { .. } => {
                DbPublish::Succeeded { operation: "UpsertTorrent".to_string() }
            }
            DbEvent::BookUpserted { .. } => {
                DbPublish::Succeeded { operation: "UpsertBook".to_string() }
            }
            DbEvent::DownloadQueueUpdated { .. } => {
                DbPublish::Succeeded { operation: "DownloadQueueUpdated".to_string() }
            }
        };
        Outcome {
            actions: Vec::new(),
            publish: vec![publish],
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        Self::outcome(vec![DbAction::Execute(cmd)], DbResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivityRecord, ActivitySource, DbCommand};
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn db_command_serializes_with_record_payload() {
        let cmd = DbCommand::RecordActivity(ActivityRecord {
            at: Utc::now(),
            source: ActivitySource::Domain,
            action: "sync-port".to_string(),
            book_id: None,
            detail: Some("ok".to_string()),
            metadata: json!({ "port": 51820 }),
        });

        let value = serde_json::to_value(cmd).expect("command should serialize");

        assert_eq!(value["RecordActivity"]["action"], "sync-port");
        assert_eq!(value["RecordActivity"]["metadata"]["port"], 51_820);
    }
}
