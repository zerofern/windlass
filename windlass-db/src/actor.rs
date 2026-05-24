use windlass_db_core::{
    ActivityId, ActivitySource, AlertId, DbCommand, DbEvent, DbFailure, SnapshotId,
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
            other => DbEvent::Failed(DbFailure {
                operation: format!("{other:?}"),
                message: "DbCommand variant is not implemented by PostgresDbActor yet".to_string(),
                retryable: false,
            }),
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

const fn activity_source_str(source: &ActivitySource) -> &'static str {
    match source {
        ActivitySource::Shell => "shell",
        ActivitySource::Domain => "domain",
        ActivitySource::Qbit => "qbit",
        ActivitySource::Mam => "mam",
        ActivitySource::Vpn => "vpn",
        ActivitySource::Web => "web",
        ActivitySource::System => "system",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use windlass_db_core::{
        ActivityRecord, ActivitySource, AlertRecord, DbCommand, DbEvent, SystemSnapshotRecord,
    };
    use windlass_types::AlertPriority;

    use super::PostgresDbActor;
    use crate::{activity_log, alerts, tests::test_pool};

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
}
