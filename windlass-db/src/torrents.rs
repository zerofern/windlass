use crate::{DbError, DbPool, TorrentRow};

/// Inserts or updates a torrent record keyed by `hash`.
///
/// `book_id` and `mam_id` are coalesced with existing values so a later
/// upsert without those fields does not clear them.
///
/// # Errors
/// Returns `DbError` if the database query fails.
pub async fn upsert(pool: &DbPool, r: &TorrentRow) -> Result<(), DbError> {
    sqlx::query!(
        r#"
        INSERT INTO torrents (hash, book_id, mam_id, name, state, seeding_time_secs,
            downloaded_bytes, seen_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::timestamptz)
        ON CONFLICT(hash) DO UPDATE SET
            book_id = COALESCE(excluded.book_id, torrents.book_id),
            mam_id = COALESCE(excluded.mam_id, torrents.mam_id),
            name = excluded.name,
            state = excluded.state,
            seeding_time_secs = excluded.seeding_time_secs,
            downloaded_bytes = excluded.downloaded_bytes,
            seen_at = excluded.seen_at
        "#,
        r.hash,
        r.book_id,
        r.mam_id,
        r.name,
        r.state,
        r.seeding_time_secs,
        r.downloaded_bytes,
        r.seen_at
    )
    .execute(pool.inner())
    .await?;
    Ok(())
}

/// Returns all torrents ordered by insertion time descending.
///
/// # Errors
/// Returns `DbError` if the database query fails.
///
pub async fn get_all(pool: &DbPool) -> Result<Vec<TorrentRow>, DbError> {
    let rows = sqlx::query_as!(
        TorrentRow,
        r#"
        SELECT hash, book_id, mam_id, name, state, seeding_time_secs,
            downloaded_bytes, seen_at::text AS "seen_at!", added_at::text AS "added_at!"
        FROM torrents ORDER BY added_at DESC
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

    fn sample_row(hash: &str) -> TorrentRow {
        TorrentRow {
            hash: hash.to_string(),
            book_id: None,
            mam_id: None,
            name: "Test Torrent".to_string(),
            state: "downloading".to_string(),
            seeding_time_secs: 0,
            downloaded_bytes: 0,
            seen_at: "2024-01-01T00:00:00Z".to_string(),
            added_at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn upsert_and_get_all_roundtrip() {
        let pool = test_pool().await;
        upsert(&pool, &sample_row("abc123")).await.unwrap();
        let rows = get_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "abc123");
        assert_eq!(rows[0].state, "downloading");
    }

    #[tokio::test]
    async fn upsert_preserves_book_id_on_conflict() {
        let pool = test_pool().await;
        // Create a valid book first to satisfy the foreign key constraint.
        let book_id = crate::books::upsert(&pool, MamTorrentId(42)).await.unwrap();

        let mut r = sample_row("def456");
        r.book_id = Some(book_id);
        upsert(&pool, &r).await.unwrap();

        // Second upsert without book_id — COALESCE should keep the original.
        let r2 = sample_row("def456");
        upsert(&pool, &r2).await.unwrap();

        let rows = get_all(&pool).await.unwrap();
        assert_eq!(rows[0].book_id, Some(book_id));
    }
}
