use windlass_db_core::{AlertId, DbCommand, DbEvent, DbFailure};

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
            DbCommand::RecordAlert(record) => match record_alert(&self.pool, record).await {
                Ok(id) => DbEvent::AlertRecorded { id },
                Err(error) => DbEvent::Failed(error),
            },
            other => DbEvent::Failed(DbFailure {
                operation: format!("{other:?}"),
                message: "DbCommand variant is not implemented by PostgresDbActor yet".to_string(),
                retryable: false,
            }),
        }
    }
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use windlass_db_core::{AlertRecord, DbCommand, DbEvent};
    use windlass_types::AlertPriority;

    use super::PostgresDbActor;
    use crate::{alerts, tests::test_pool};

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
}
