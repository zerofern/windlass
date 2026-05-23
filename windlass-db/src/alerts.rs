use crate::{AlertRow, DbError, DbPool, alert_priority_str};
use sqlx::Row;
use windlass_types::AlertPriority;

/// Inserts a new alert record.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn insert(
    pool: &DbPool,
    priority: AlertPriority,
    title: &str,
    body: &str,
) -> Result<(), DbError> {
    let p = alert_priority_str(priority);
    sqlx::query("INSERT INTO alerts (priority, title, body) VALUES (?, ?, ?)")
        .bind(p)
        .bind(title)
        .bind(body)
        .execute(pool.inner())
        .await?;
    Ok(())
}

/// Returns all alerts ordered by creation time descending, capped at 200.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn get_all(pool: &DbPool) -> Result<Vec<AlertRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, priority, title, body, read, created_at
         FROM alerts ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(pool.inner())
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AlertRow {
            id: r.get("id"),
            priority: r.get("priority"),
            title: r.get("title"),
            body: r.get("body"),
            read: r.get::<i64, _>("read") != 0,
            created_at: r.get("created_at"),
        })
        .collect())
}

/// Marks the alert with the given `id` as read.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn mark_read(pool: &DbPool, id: i64) -> Result<(), DbError> {
    sqlx::query("UPDATE alerts SET read = 1 WHERE id = ?")
        .bind(id)
        .execute(pool.inner())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_pool;
    use windlass_types::AlertPriority;

    #[tokio::test]
    async fn insert_and_get_all_roundtrip() {
        let (pool, _dir) = test_pool().await;
        insert(&pool, AlertPriority::Warning, "title", "body")
            .await
            .unwrap();
        let rows = get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].priority, "warning");
        assert_eq!(rows[0].title, "title");
        assert_eq!(rows[0].body, "body");
        assert!(!rows[0].read);
    }

    #[tokio::test]
    async fn mark_read_sets_flag() {
        let (pool, _dir) = test_pool().await;
        insert(&pool, AlertPriority::Info, "t", "b").await.unwrap();
        let rows = get_all(&pool).await.unwrap();
        let id = rows[0].id;
        mark_read(&pool, id).await.unwrap();
        let rows = get_all(&pool).await.unwrap();
        assert!(rows[0].read);
    }
}
