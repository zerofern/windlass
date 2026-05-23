use crate::{ActivityRow, DbError, DbPool};
use sqlx::Row;

/// Inserts an activity record.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn insert(
    pool: &DbPool,
    source: &str,
    action: &str,
    book_id: Option<i64>,
    detail: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query("INSERT INTO activity_log (source, action, book_id, detail) VALUES (?, ?, ?, ?)")
        .bind(source)
        .bind(action)
        .bind(book_id)
        .bind(detail)
        .execute(pool.inner())
        .await?;
    Ok(())
}

/// Returns the `limit` most recent activity records ordered by creation time descending.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn get_recent(pool: &DbPool, limit: i64) -> Result<Vec<ActivityRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, source, action, book_id, detail, created_at
         FROM activity_log ORDER BY created_at DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool.inner())
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ActivityRow {
            id: r.get("id"),
            source: r.get("source"),
            action: r.get("action"),
            book_id: r.get("book_id"),
            detail: r.get("detail"),
            created_at: r.get("created_at"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_pool;

    #[tokio::test]
    async fn insert_and_get_recent_roundtrip() {
        let (pool, _dir) = test_pool().await;
        insert(&pool, "shell", "SyncPort", None, Some("ok"))
            .await
            .unwrap();
        let rows = get_recent(&pool, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "shell");
        assert_eq!(rows[0].action, "SyncPort");
        assert_eq!(rows[0].detail, Some("ok".to_string()));
        assert!(rows[0].book_id.is_none());
    }

    #[tokio::test]
    async fn get_recent_respects_limit() {
        let (pool, _dir) = test_pool().await;
        for i in 0..5_i32 {
            insert(&pool, "s", &format!("a{i}"), None, None)
                .await
                .unwrap();
        }
        let rows = get_recent(&pool, 3).await.unwrap();
        assert_eq!(rows.len(), 3);
    }
}
