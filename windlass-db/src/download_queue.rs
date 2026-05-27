use crate::{DbError, DbPool, DownloadQueueRow};
use windlass_types::MamTorrentId;

/// Adds a new pending download queue entry for `mam_id` linked to `book_id`.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn enqueue(pool: &DbPool, mam_id: MamTorrentId, book_id: i64) -> Result<(), DbError> {
    let id = i64::try_from(mam_id.into_inner()).unwrap_or(i64::MAX);
    sqlx::query!(
        "INSERT INTO download_queue (mam_id, book_id, status) VALUES ($1, $2, 'pending')",
        id,
        book_id
    )
    .execute(pool.inner())
    .await?;
    Ok(())
}

/// Updates the status of the queue entry for `mam_id`.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn update_status(
    pool: &DbPool,
    mam_id: MamTorrentId,
    status: &str,
) -> Result<(), DbError> {
    let id = i64::try_from(mam_id.into_inner()).unwrap_or(i64::MAX);
    sqlx::query!(
        "UPDATE download_queue SET status = $1, updated_at = now() WHERE mam_id = $2",
        status,
        id
    )
    .execute(pool.inner())
    .await?;
    Ok(())
}

/// Marks the queue entry for `mam_id` as blacklisted.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn blacklist(pool: &DbPool, mam_id: MamTorrentId) -> Result<(), DbError> {
    update_status(pool, mam_id, "blacklisted").await
}

/// Returns all MAM IDs currently marked as blacklisted in the download queue.
/// Used at startup to populate `SystemState.blacklisted_mam_ids`.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn get_blacklisted_ids(pool: &DbPool) -> Result<Vec<MamTorrentId>, DbError> {
    let rows = sqlx::query!("SELECT mam_id FROM download_queue WHERE status = 'blacklisted'")
        .fetch_all(pool.inner())
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| u64::try_from(r.mam_id).ok())
        .filter_map(|id| MamTorrentId::try_new(id).ok())
        .collect())
}

/// Returns all download queue entries ordered by creation time descending.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn get_all(pool: &DbPool) -> Result<Vec<DownloadQueueRow>, DbError> {
    let rows = sqlx::query_as!(
        DownloadQueueRow,
        r#"
        SELECT id, book_id, mam_id, status,
            created_at::text AS "created_at!", updated_at::text AS "updated_at!"
        FROM download_queue ORDER BY created_at DESC
        "#
    )
    .fetch_all(pool.inner())
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_pool;
    use windlass_types::MamTorrentId;

    async fn make_book(pool: &DbPool) -> i64 {
        crate::books::upsert(pool, MamTorrentId::try_new(1).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn enqueue_and_get_roundtrip() {
        let pool = test_pool().await;
        let book_id = make_book(&pool).await;
        enqueue(&pool, MamTorrentId::try_new(100).unwrap(), book_id)
            .await
            .unwrap();
        let rows = get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mam_id, 100);
        assert_eq!(rows[0].status, "pending");
    }

    #[tokio::test]
    async fn update_status_changes_status() {
        let pool = test_pool().await;
        let book_id = make_book(&pool).await;
        enqueue(&pool, MamTorrentId::try_new(200).unwrap(), book_id)
            .await
            .unwrap();
        update_status(&pool, MamTorrentId::try_new(200).unwrap(), "downloading")
            .await
            .unwrap();
        let rows = get_all(&pool).await.unwrap();
        assert_eq!(rows[0].status, "downloading");
    }

    #[tokio::test]
    async fn blacklist_sets_blacklisted_status() {
        let pool = test_pool().await;
        let book_id = make_book(&pool).await;
        enqueue(&pool, MamTorrentId::try_new(300).unwrap(), book_id)
            .await
            .unwrap();
        blacklist(&pool, MamTorrentId::try_new(300).unwrap())
            .await
            .unwrap();
        let rows = get_all(&pool).await.unwrap();
        assert_eq!(rows[0].status, "blacklisted");
    }

    #[tokio::test]
    async fn get_blacklisted_ids_returns_blacklisted_only() {
        let pool = test_pool().await;
        let book_id = make_book(&pool).await;
        enqueue(&pool, MamTorrentId::try_new(400).unwrap(), book_id)
            .await
            .unwrap();
        enqueue(&pool, MamTorrentId::try_new(401).unwrap(), book_id)
            .await
            .unwrap();
        blacklist(&pool, MamTorrentId::try_new(400).unwrap())
            .await
            .unwrap();
        let ids = get_blacklisted_ids(&pool).await.unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], MamTorrentId::try_new(400).unwrap());
    }
}
