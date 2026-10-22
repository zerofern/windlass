use windlass_db_core::{
    ActivityId, ActivitySource, AlertId, BookId, BookStatus, DbCommand, DbEvent, DbFailure,
    DownloadId, DownloadStatus, SnapshotId, TorrentStateRecord,
};

use crate::{DbPool, alert_priority_str};

#[derive(Clone)]
pub struct PostgresDbActor {
    pool: DbPool,
}

impl PostgresDbActor {
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub async fn handle(&self, command: DbCommand) -> DbEvent {
        match command {
            DbCommand::RecordActivity(record) => match record_activity(&self.pool, record).await {
                Ok(id) => DbEvent::ActivityRecorded { id },
                Err(error) => DbEvent::Failed(error),
            },
            DbCommand::RecordAlert(record) => match record_alert(&self.pool, record).await {
                Ok(id) => DbEvent::AlertRecorded { id },
                Err(error) => DbEvent::Failed(error),
            },
            DbCommand::SaveSystemSnapshot(record) => {
                match save_system_snapshot(&self.pool, record).await {
                    Ok(id) => DbEvent::SystemSnapshotSaved { id },
                    Err(error) => DbEvent::Failed(error),
                }
            }
            DbCommand::UpsertTorrent(record) => {
                let hash = record.hash.clone();
                match upsert_torrent(&self.pool, record).await {
                    Ok(()) => DbEvent::TorrentUpserted { hash },
                    Err(error) => DbEvent::Failed(error),
                }
            }
            DbCommand::EnqueueDownload(record) => {
                match enqueue_download(&self.pool, record).await {
                    Ok(id) => DbEvent::DownloadQueueUpdated { id },
                    Err(error) => DbEvent::Failed(error),
                }
            }
            DbCommand::MarkDownloadState(change) => {
                match mark_download_state(&self.pool, change).await {
                    Ok(id) => DbEvent::DownloadQueueUpdated { id },
                    Err(error) => DbEvent::Failed(error),
                }
            }
            DbCommand::UpsertBook(record) => match upsert_book(&self.pool, record).await {
                Ok(id) => DbEvent::BookUpserted { id },
                Err(error) => DbEvent::Failed(error),
            },
        }
    }
}

async fn record_activity(
    pool: &DbPool,
    record: windlass_db_core::ActivityRecord,
) -> Result<ActivityId, DbFailure> {
    let source = activity_source_str(&record.source);
    let book_id = record.book_id.map(|id| id.0);
    let row = sqlx::query!(
        r#"
        INSERT INTO activity_log (source, action, book_id, detail, metadata, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#,
        source,
        record.action,
        book_id,
        record.detail,
        record.metadata,
        record.at
    )
    .fetch_one(pool.inner())
    .await
    .map_err(|e| DbFailure {
        operation: "RecordActivity".to_string(),
        message: e.to_string(),
        retryable: true,
    })?;
    Ok(ActivityId(row.id))
}

async fn record_alert(
    pool: &DbPool,
    record: windlass_db_core::AlertRecord,
) -> Result<AlertId, DbFailure> {
    let priority = alert_priority_str(record.priority);
    let row = sqlx::query!(
        r#"
        INSERT INTO alerts (priority, title, body, created_at)
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
        priority,
        record.title,
        record.body,
        record.at
    )
    .fetch_one(pool.inner())
    .await
    .map_err(|e| DbFailure {
        operation: "RecordAlert".to_string(),
        message: e.to_string(),
        retryable: true,
    })?;
    Ok(AlertId(row.id))
}

async fn save_system_snapshot(
    pool: &DbPool,
    record: windlass_db_core::SystemSnapshotRecord,
) -> Result<SnapshotId, DbFailure> {
    let row = sqlx::query!(
        r#"
        INSERT INTO system_snapshots (state, created_at)
        VALUES ($1, $2)
        RETURNING id
        "#,
        record.state,
        record.at
    )
    .fetch_one(pool.inner())
    .await
    .map_err(|e| DbFailure {
        operation: "SaveSystemSnapshot".to_string(),
        message: e.to_string(),
        retryable: true,
    })?;
    Ok(SnapshotId(row.id))
}

async fn upsert_torrent(
    pool: &DbPool,
    record: windlass_db_core::TorrentRecord,
) -> Result<(), DbFailure> {
    let state = torrent_state_str(&record.state);
    let book_id = record.book_id.map(|id| id.0);
    let mam_id = record
        .mam_id
        .map(|id| i64::try_from(id.into_inner()).unwrap_or(i64::MAX));
    sqlx::query!(
        r#"
        INSERT INTO torrents (hash, book_id, mam_id, name, state, seeding_time_secs,
            downloaded_bytes, seen_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT(hash) DO UPDATE SET
            book_id = COALESCE(excluded.book_id, torrents.book_id),
            mam_id = COALESCE(excluded.mam_id, torrents.mam_id),
            name = excluded.name,
            state = excluded.state,
            seeding_time_secs = excluded.seeding_time_secs,
            downloaded_bytes = excluded.downloaded_bytes,
            seen_at = excluded.seen_at
        "#,
        record.hash.0,
        book_id,
        mam_id,
        record.name,
        state,
        record.seeding_time_secs,
        record.downloaded_bytes,
        record.seen_at
    )
    .execute(pool.inner())
    .await
    .map_err(|e| DbFailure {
        operation: "UpsertTorrent".to_string(),
        message: e.to_string(),
        retryable: true,
    })?;
    Ok(())
}

async fn enqueue_download(
    pool: &DbPool,
    record: windlass_db_core::DownloadQueueRecord,
) -> Result<DownloadId, DbFailure> {
    let mam_id = i64::try_from(record.mam_id.into_inner()).unwrap_or(i64::MAX);
    let book_id = record.book_id.map(|id| id.0);
    let status = download_status_str(&record.status);
    let row = sqlx::query!(
        r#"
        INSERT INTO download_queue (book_id, mam_id, status)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
        book_id,
        mam_id,
        status
    )
    .fetch_one(pool.inner())
    .await
    .map_err(|e| DbFailure {
        operation: "EnqueueDownload".to_string(),
        message: e.to_string(),
        retryable: true,
    })?;
    Ok(DownloadId(row.id))
}

async fn upsert_book(
    pool: &DbPool,
    record: windlass_db_core::BookRecord,
) -> Result<BookId, DbFailure> {
    let mam_id = record
        .mam_id
        .map(|id| i64::try_from(id.into_inner()).unwrap_or(i64::MAX));
    let status = book_status_str(&record.status);
    let id = if let Some(id) = record.id {
        sqlx::query!(
            r#"
            INSERT INTO books (id, mam_id, title, author, status)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT(id) DO UPDATE SET
                mam_id = COALESCE(excluded.mam_id, books.mam_id),
                title = COALESCE(excluded.title, books.title),
                author = COALESCE(excluded.author, books.author),
                status = excluded.status
            RETURNING id
            "#,
            id.0,
            mam_id,
            record.title,
            record.author,
            status
        )
        .fetch_one(pool.inner())
        .await
        .map(|row| row.id)
    } else {
        sqlx::query!(
            r#"
            INSERT INTO books (mam_id, title, author, status)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT(mam_id) DO UPDATE SET
                title = COALESCE(excluded.title, books.title),
                author = COALESCE(excluded.author, books.author),
                status = excluded.status
            RETURNING id
            "#,
            mam_id,
            record.title,
            record.author,
            status
        )
        .fetch_one(pool.inner())
        .await
        .map(|row| row.id)
    }
    .map_err(|e| DbFailure {
        operation: "UpsertBook".to_string(),
        message: e.to_string(),
        retryable: true,
    })?;
    Ok(BookId(id))
}

async fn mark_download_state(
    pool: &DbPool,
    change: windlass_db_core::DownloadStateChange,
) -> Result<DownloadId, DbFailure> {
    let mam_id = i64::try_from(change.mam_id.into_inner()).unwrap_or(i64::MAX);
    let status = download_status_str(&change.status);
    if let Some(row) = sqlx::query!(
        r#"
        UPDATE download_queue
        SET status = $1, updated_at = now()
        WHERE mam_id = $2
        RETURNING id
        "#,
        status,
        mam_id
    )
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| DbFailure {
        operation: "MarkDownloadState".to_string(),
        message: e.to_string(),
        retryable: true,
    })? {
        return Ok(DownloadId(row.id));
    }

    let row = sqlx::query!(
        r#"
        INSERT INTO download_queue (mam_id, status)
        VALUES ($1, $2)
        RETURNING id
        "#,
        mam_id,
        status
    )
    .fetch_one(pool.inner())
    .await
    .map_err(|e| DbFailure {
        operation: "MarkDownloadState".to_string(),
        message: e.to_string(),
        retryable: true,
    })?;
    Ok(DownloadId(row.id))
}

const fn activity_source_str(source: &ActivitySource) -> &'static str {
    match source {
        ActivitySource::Shell => "shell",
        ActivitySource::Domain => "domain",
        ActivitySource::Qbit => "qbit",
        ActivitySource::Mam => "mam",
        ActivitySource::Vpn => "vpn",
        ActivitySource::Web => "web",
        ActivitySource::System => "system",
        ActivitySource::Download => "download",
    }
}

const fn torrent_state_str(state: &TorrentStateRecord) -> &str {
    match state {
        TorrentStateRecord::Downloading => "downloading",
        TorrentStateRecord::Uploading => "uploading",
        TorrentStateRecord::ForcedUpload => "forcedUP",
        TorrentStateRecord::PausedDownloading => "pausedDL",
        TorrentStateRecord::PausedUploading => "pausedUP",
        TorrentStateRecord::StalledDownloading => "stalledDL",
        TorrentStateRecord::StalledUploading => "stalledUP",
        TorrentStateRecord::Checking => "checking",
        TorrentStateRecord::Error => "error",
        // qBit emits ~12 states that don't map to the 9 explicit
        // variants (metaDL, queuedDL/UP, checkingDL/UP/ResumeData,
        // allocating, moving, missingFiles, forcedDL, unknown).  The
        // `torrents_state_valid` CHECK constraint (migration 0002)
        // covers these under the catch-all `'other'`; writing the raw
        // qBit state string violates the CHECK and silently drops the
        // upsert.
        TorrentStateRecord::Unknown(_) => "other",
    }
}

const fn download_status_str(status: &DownloadStatus) -> &'static str {
    match status {
        DownloadStatus::Pending => "pending",
        DownloadStatus::Downloading => "downloading",
        DownloadStatus::Seeding => "seeding",
        DownloadStatus::Satisfied => "satisfied",
        DownloadStatus::Complete => "complete",
        DownloadStatus::Failed => "failed",
        DownloadStatus::Blacklisted => "blacklisted",
    }
}

const fn book_status_str(status: &BookStatus) -> &'static str {
    match status {
        BookStatus::PendingMetadata => "pending_metadata",
        BookStatus::Queued => "queued",
        BookStatus::Downloading => "downloading",
        BookStatus::Complete => "complete",
        BookStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use windlass_db_core::{
        ActivityRecord, ActivitySource, AlertRecord, BookRecord, BookStatus, DbCommand, DbEvent,
        DownloadQueueRecord, DownloadStateChange, DownloadStatus, SystemSnapshotRecord,
        TorrentRecord, TorrentStateRecord,
    };
    use windlass_types::{AlertPriority, MamTorrentId, TorrentHash};

    use super::PostgresDbActor;
    use crate::{activity_log, alerts, books, download_queue, tests::test_pool, torrents};

    #[tokio::test]
    async fn handle_record_activity_persists_activity() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::RecordActivity(ActivityRecord {
                at: Utc::now(),
                source: ActivitySource::Domain,
                action: "sync-port".to_string(),
                book_id: None,
                detail: Some("ok".to_string()),
                metadata: json!({ "port": 51820 }),
            }))
            .await;

        assert!(matches!(event, DbEvent::ActivityRecorded { .. }));
        let rows = activity_log::get_recent(&pool, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "domain");
        assert_eq!(rows[0].action, "sync-port");
        assert_eq!(rows[0].detail, Some("ok".to_string()));
    }

    #[tokio::test]
    async fn handle_record_alert_persists_alert() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::RecordAlert(AlertRecord {
                at: Utc::now(),
                priority: AlertPriority::Warning,
                title: "title".to_string(),
                body: "body".to_string(),
            }))
            .await;

        assert!(matches!(event, DbEvent::AlertRecorded { .. }));
        let rows = alerts::get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "title");
    }

    #[tokio::test]
    async fn handle_save_system_snapshot_persists_snapshot() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::SaveSystemSnapshot(SystemSnapshotRecord {
                at: Utc::now(),
                state: json!({ "vpn": "ready" }),
            }))
            .await;

        assert!(matches!(event, DbEvent::SystemSnapshotSaved { .. }));
        let saved = sqlx::query_scalar!("SELECT state FROM system_snapshots LIMIT 1")
            .fetch_one(pool.inner())
            .await
            .unwrap();
        assert_eq!(saved, json!({ "vpn": "ready" }));
    }

    #[tokio::test]
    async fn handle_upsert_torrent_persists_torrent() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::UpsertTorrent(TorrentRecord {
                hash: TorrentHash("abc123".to_string()),
                book_id: None,
                mam_id: Some(MamTorrentId::try_new(42).unwrap()),
                name: "test".to_string(),
                state: TorrentStateRecord::ForcedUpload,
                seeding_time_secs: 120,
                downloaded_bytes: 1_024,
                seen_at: Utc::now(),
            }))
            .await;

        assert!(matches!(event, DbEvent::TorrentUpserted { .. }));
        let rows = torrents::get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "abc123");
        assert_eq!(rows[0].state, "forcedUP");
    }

    /// Regression: qBit emits ~12 transient states (`metaDL`,
    /// `queuedDL`, `checkingDL`, `allocating`, etc.) that don't map to
    /// the 9 explicit `TorrentStateRecord` variants and end up as
    /// `Unknown(raw_string)`.  Writing the raw qBit string violates
    /// the `torrents_state_valid` CHECK constraint (migration 0002),
    /// so `Unknown(_)` must lower to the catch-all `'other'`.
    #[tokio::test]
    async fn handle_upsert_torrent_unknown_state_maps_to_other() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::UpsertTorrent(TorrentRecord {
                hash: TorrentHash("metaDL-test".to_string()),
                book_id: None,
                mam_id: None,
                name: "transient-state-torrent".to_string(),
                state: TorrentStateRecord::Unknown("metaDL".to_string()),
                seeding_time_secs: 0,
                downloaded_bytes: 0,
                seen_at: Utc::now(),
            }))
            .await;

        assert!(
            matches!(event, DbEvent::TorrentUpserted { .. }),
            "upsert should succeed; got {event:?}"
        );
        let rows = torrents::get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, "other");
    }

    #[tokio::test]
    async fn handle_upsert_book_persists_book() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::UpsertBook(BookRecord {
                id: None,
                mam_id: Some(MamTorrentId::try_new(7).unwrap()),
                title: Some("Title".to_string()),
                author: Some("Author".to_string()),
                status: BookStatus::Queued,
            }))
            .await;

        assert!(matches!(event, DbEvent::BookUpserted { .. }));
        let row = books::get_by_mam_id(&pool, MamTorrentId::try_new(7).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.title, Some("Title".to_string()));
        assert_eq!(row.author, Some("Author".to_string()));
        assert_eq!(row.status, "queued");
    }

    #[tokio::test]
    async fn handle_enqueue_download_persists_queue_row() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::EnqueueDownload(DownloadQueueRecord {
                book_id: None,
                mam_id: MamTorrentId::try_new(123).unwrap(),
                status: DownloadStatus::Pending,
            }))
            .await;

        assert!(matches!(event, DbEvent::DownloadQueueUpdated { .. }));
        let rows = download_queue::get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mam_id, 123);
        assert_eq!(rows[0].status, "pending");
    }

    #[tokio::test]
    async fn handle_mark_download_state_updates_or_inserts_queue_row() {
        let pool = test_pool().await;
        let actor = PostgresDbActor::new(pool.clone());

        let event = actor
            .handle(DbCommand::MarkDownloadState(DownloadStateChange {
                mam_id: MamTorrentId::try_new(99).unwrap(),
                status: DownloadStatus::Blacklisted,
            }))
            .await;

        assert!(matches!(event, DbEvent::DownloadQueueUpdated { .. }));
        let rows = download_queue::get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mam_id, 99);
        assert_eq!(rows[0].status, "blacklisted");
    }
}
