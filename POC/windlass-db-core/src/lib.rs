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
    /// §36 step 5: manual-download activity entries
    /// (`download_blocked` / `torrent_added` / `torrent_add_failed`).
    Download,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    // The DB machine is stateless: actions are passed verbatim to the
    // shell and the machine carries no fields. The snapshot is `()`.
    type StateSnapshot = ();

    fn new(_config: (), _now: Instant) -> Self {
        Self
    }

    fn handle(
        &mut self,
        _now: Instant,
        _wall_now: chrono::DateTime<chrono::Utc>,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        let publishes = match event.inner {
            DbEvent::Failed(failure) => DbPublish::Failed(failure),
            DbEvent::ActivityRecorded { .. } => DbPublish::Succeeded {
                operation: "RecordActivity".to_string(),
            },
            DbEvent::AlertRecorded { .. } => DbPublish::Succeeded {
                operation: "RecordAlert".to_string(),
            },
            DbEvent::SystemSnapshotSaved { .. } => DbPublish::Succeeded {
                operation: "SaveSystemSnapshot".to_string(),
            },
            DbEvent::TorrentUpserted { .. } => DbPublish::Succeeded {
                operation: "UpsertTorrent".to_string(),
            },
            DbEvent::BookUpserted { .. } => DbPublish::Succeeded {
                operation: "UpsertBook".to_string(),
            },
            DbEvent::DownloadQueueUpdated { .. } => DbPublish::Succeeded {
                operation: "DownloadQueueUpdated".to_string(),
            },
        };
        Outcome {
            actions: Vec::new(),
            publishes: vec![publishes],
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        _wall_now: chrono::DateTime<chrono::Utc>,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        Self::outcome(vec![DbAction::Execute(cmd)], DbResponse::Accepted)
    }

    fn state_snapshot(&self) -> Self::StateSnapshot {}
}

#[cfg(test)]
mod tests {
    use super::{ActivityRecord, ActivitySource, DbCommand, DbMachine};
    use chrono::Utc;
    use serde_json::json;
    use std::time::Instant;
    use windlass_machine::Machine;

    #[test]
    fn db_machine_state_snapshot_is_unit() {
        // §37b: DbMachine is stateless — the snapshot is `()` and
        // serializes to JSON `null`.
        let machine = DbMachine::new((), Instant::now());
        let value =
            serde_json::to_value(machine.state_snapshot()).expect("snapshot should serialize");
        assert!(value.is_null());
    }

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

#[cfg(test)]
mod prop_tests {
    use std::time::Instant;

    use chrono::Utc;
    use proptest::prelude::*;
    use serde_json::json;
    use windlass_machine::{ExternalCause, Machine, Timed};
    use windlass_types::{AlertPriority, MamTorrentId, TorrentHash};

    use crate::{
        ActivityId, ActivityRecord, ActivitySource, AlertId, AlertRecord, BookId, BookRecord,
        BookStatus, DbAction, DbCommand, DbEvent, DbFailure, DbMachine, DbPublish, DbResponse,
        DownloadId, DownloadQueueRecord, DownloadStateChange, DownloadStatus, SnapshotId,
        SystemSnapshotRecord, TorrentRecord, TorrentStateRecord,
    };

    fn any_torrent_hash() -> impl Strategy<Value = TorrentHash> {
        "[a-f0-9]{40}".prop_map(TorrentHash)
    }

    fn any_mam_id() -> impl Strategy<Value = MamTorrentId> {
        (1u64..=1_000_000).prop_map(|n| MamTorrentId::try_new(n).unwrap())
    }

    fn any_db_event() -> impl Strategy<Value = DbEvent> {
        prop_oneof![
            any::<i64>().prop_map(|id| DbEvent::ActivityRecorded { id: ActivityId(id) }),
            any::<i64>().prop_map(|id| DbEvent::AlertRecorded { id: AlertId(id) }),
            any::<i64>().prop_map(|id| DbEvent::SystemSnapshotSaved { id: SnapshotId(id) }),
            any_torrent_hash().prop_map(|hash| DbEvent::TorrentUpserted { hash }),
            any::<i64>().prop_map(|id| DbEvent::BookUpserted { id: BookId(id) }),
            any::<i64>().prop_map(|id| DbEvent::DownloadQueueUpdated { id: DownloadId(id) }),
            (any::<String>(), any::<String>(), any::<bool>()).prop_map(
                |(operation, message, retryable)| DbEvent::Failed(DbFailure {
                    operation,
                    message,
                    retryable,
                })
            ),
        ]
    }

    fn any_alert_priority() -> impl Strategy<Value = AlertPriority> {
        prop_oneof![
            Just(AlertPriority::Info),
            Just(AlertPriority::Warning),
            Just(AlertPriority::Critical),
        ]
    }

    fn any_db_command() -> impl Strategy<Value = DbCommand> {
        prop_oneof![
            any::<String>().prop_map(|action| DbCommand::RecordActivity(ActivityRecord {
                at: Utc::now(),
                source: ActivitySource::System,
                action,
                book_id: None,
                detail: None,
                metadata: json!({}),
            })),
            (any_alert_priority(), any::<String>(), any::<String>()).prop_map(
                |(priority, title, body)| DbCommand::RecordAlert(AlertRecord {
                    at: Utc::now(),
                    priority,
                    title,
                    body,
                })
            ),
            Just(DbCommand::SaveSystemSnapshot(SystemSnapshotRecord {
                at: Utc::now(),
                state: json!({}),
            })),
            any_torrent_hash().prop_map(|hash| DbCommand::UpsertTorrent(TorrentRecord {
                hash,
                book_id: None,
                mam_id: None,
                name: "t".to_string(),
                state: TorrentStateRecord::Downloading,
                seeding_time_secs: 0,
                downloaded_bytes: 0,
                seen_at: Utc::now(),
            })),
            Just(DbCommand::UpsertBook(BookRecord {
                id: None,
                mam_id: None,
                title: None,
                author: None,
                status: BookStatus::Queued,
            })),
            any_mam_id().prop_map(|mam_id| DbCommand::EnqueueDownload(DownloadQueueRecord {
                book_id: None,
                mam_id,
                status: DownloadStatus::Pending,
            })),
            any_mam_id().prop_map(|mam_id| DbCommand::MarkDownloadState(DownloadStateChange {
                mam_id,
                status: DownloadStatus::Complete,
            })),
        ]
    }

    proptest! {
        // DB-2 + DB-3 (Guarantee F): every event yields exactly one publish and
        // NO action — so a DB failure can never trigger more DB work (no recursion).
        #[test]
        fn event_publishes_once_and_emits_no_action(event in any_db_event()) {
            let mut machine = DbMachine::new((), Instant::now());
            let is_failure = matches!(event, DbEvent::Failed(_));

            let out = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));

            prop_assert!(out.actions.is_empty());
            prop_assert_eq!(out.publishes.len(), 1);
            match &out.publishes[0] {
                DbPublish::Failed(_) => prop_assert!(is_failure),
                DbPublish::Succeeded { .. } => prop_assert!(!is_failure),
            }
        }

        // DB-1: every command yields exactly one Execute(cmd), no publish, Accepted.
        #[test]
        fn command_executes_verbatim(command in any_db_command()) {
            let mut machine = DbMachine::new((), Instant::now());

            let out = machine.handle_command(Instant::now(), chrono::Utc::now(), command.clone());

            prop_assert!(out.publishes.is_empty());
            prop_assert_eq!(out.response, DbResponse::Accepted);
            prop_assert_eq!(out.actions, vec![DbAction::Execute(command)]);
        }
    }
}
