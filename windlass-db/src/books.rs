use crate::{BookRow, DbError, DbPool};
use sqlx::Row;
use windlass_types::MamTorrentId;

/// Upserts a book record by `mam_id`. Returns the book's primary key `id`.
///
/// # Errors
/// Returns `DbError` if the database query fails.
///
pub async fn upsert(pool: &DbPool, mam_id: MamTorrentId) -> Result<i64, DbError> {
    let mam = i64::try_from(mam_id.0).unwrap_or(i64::MAX);
    sqlx::query(
        "INSERT INTO books (mam_id) VALUES (?) ON CONFLICT(mam_id) DO UPDATE SET mam_id = excluded.mam_id",
    )
    .bind(mam)
    .execute(pool.inner())
    .await?;
    let id = sqlx::query_scalar::<_, i64>("SELECT id FROM books WHERE mam_id = ?")
        .bind(mam)
        .fetch_one(pool.inner())
        .await?;
    Ok(id)
}

/// Returns all books ordered by creation time descending.
///
/// # Errors
/// Returns `DbError` if the database query fails.
///
pub async fn get_all(pool: &DbPool) -> Result<Vec<BookRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, mam_id, title, author, status, created_at FROM books ORDER BY created_at DESC",
    )
    .fetch_all(pool.inner())
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| BookRow {
            id: r.get("id"),
            mam_id: r.get("mam_id"),
            title: r.get("title"),
            author: r.get("author"),
            status: r.get("status"),
            created_at: r.get("created_at"),
        })
        .collect())
}

///
/// # Errors
/// Returns `DbError` if the database query fails.
///
pub async fn get_by_mam_id(
    pool: &DbPool,
    mam_id: MamTorrentId,
) -> Result<Option<BookRow>, DbError> {
    let id = i64::try_from(mam_id.0).unwrap_or(i64::MAX);
    let row = sqlx::query(
        "SELECT id, mam_id, title, author, status, created_at FROM books WHERE mam_id = ?",
    )
    .bind(id)
    .fetch_optional(pool.inner())
    .await?;
    Ok(row.map(|r| BookRow {
        id: r.get("id"),
        mam_id: r.get("mam_id"),
        title: r.get("title"),
        author: r.get("author"),
        status: r.get("status"),
        created_at: r.get("created_at"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_pool;
    use windlass_types::MamTorrentId;

    #[tokio::test]
    async fn upsert_returns_id_and_is_idempotent() {
        let (pool, _dir) = test_pool().await;
        let mam_id = MamTorrentId(42);
        let id1 = upsert(&pool, mam_id).await.unwrap();
        let id2 = upsert(&pool, mam_id).await.unwrap();
        assert!(id1 > 0);
        assert_eq!(id1, id2, "upsert same mam_id should return same book id");
    }

    #[tokio::test]
    async fn get_by_mam_id_returns_none_when_missing() {
        let (pool, _dir) = test_pool().await;
        let row = get_by_mam_id(&pool, MamTorrentId(999)).await.unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn get_by_mam_id_returns_row_after_upsert() {
        let (pool, _dir) = test_pool().await;
        let mam_id = MamTorrentId(7);
        upsert(&pool, mam_id).await.unwrap();
        let row = get_by_mam_id(&pool, mam_id).await.unwrap().unwrap();
        assert_eq!(row.mam_id, Some(7));
        assert_eq!(row.status, "pending_metadata");
    }

    #[tokio::test]
    async fn get_all_returns_all_books() {
        let (pool, _dir) = test_pool().await;
        upsert(&pool, MamTorrentId(1)).await.unwrap();
        upsert(&pool, MamTorrentId(2)).await.unwrap();
        let rows = get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 2);
        let mam_ids: Vec<Option<i64>> = rows.iter().map(|r| r.mam_id).collect();
        assert!(mam_ids.contains(&Some(1)));
        assert!(mam_ids.contains(&Some(2)));
    }
}
