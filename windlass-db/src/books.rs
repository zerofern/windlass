use crate::{BookRow, DbError, DbPool};
use windlass_types::MamTorrentId;

/// Upserts a book record by `mam_id`. Returns the book's primary key `id`.
///
/// # Errors
/// Returns `DbError` if the database query fails.
///
/// # Panics
/// Panics if the book row is missing after a successful insert — impossible in
/// practice because the upsert always affects exactly one row.
pub async fn upsert(pool: &DbPool, mam_id: MamTorrentId) -> Result<i64, DbError> {
    let mam = i64::try_from(mam_id.0).unwrap_or(i64::MAX);
    sqlx::query!(
        "INSERT INTO books (mam_id) VALUES (?) ON CONFLICT(mam_id) DO UPDATE SET mam_id = excluded.mam_id",
        mam
    )
    .execute(pool.inner())
    .await?;
    let id = sqlx::query_scalar!("SELECT id FROM books WHERE mam_id = ?", mam)
        .fetch_one(pool.inner())
        .await?;
    // id is INTEGER PRIMARY KEY AUTOINCREMENT — never null after successful INSERT
    Ok(id.expect("book id is always set after upsert"))
}

/// Returns the book with the given `mam_id`, if it exists.
///
/// # Errors
/// Returns `DbError` if the database query fails.
///
/// # Panics
/// Panics if the `id` column is NULL — impossible for `INTEGER PRIMARY KEY`.
pub async fn get_by_mam_id(
    pool: &DbPool,
    mam_id: MamTorrentId,
) -> Result<Option<BookRow>, DbError> {
    let id = i64::try_from(mam_id.0).unwrap_or(i64::MAX);
    let row = sqlx::query!(
        "SELECT id, mam_id, title, author, status, created_at FROM books WHERE mam_id = ?",
        id
    )
    .fetch_optional(pool.inner())
    .await?;
    Ok(row.map(|r| BookRow {
        // id is INTEGER PRIMARY KEY — never null when a row exists
        id: r.id.expect("book id is always non-null"),
        mam_id: r.mam_id,
        title: r.title,
        author: r.author,
        status: r.status,
        created_at: r.created_at,
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
}
