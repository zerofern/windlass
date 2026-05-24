#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod activity_log;
pub mod actor;
pub mod alerts;
pub mod books;
pub mod download_queue;
pub mod torrents;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database connection failed: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),
    #[error("query failed: {0}")]
    Query(#[from] sqlx::Error),
}

/// Typed handle over the underlying Postgres connection pool.
#[derive(Clone)]
pub struct DbPool(sqlx::PgPool);

impl DbPool {
    /// Opens a Postgres connection pool.
    ///
    /// # Errors
    /// Returns `DbError::Connect` if the database cannot be opened.
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        sqlx::PgPool::connect(database_url)
            .await
            .map(Self)
            .map_err(DbError::Connect)
    }

    /// Runs all pending Postgres migrations.
    ///
    /// # Errors
    /// Returns `DbError::Migrate` if any migration fails.
    pub async fn migrate(&self) -> Result<(), DbError> {
        sqlx::migrate!("./postgres/migrations")
            .run(&self.0)
            .await
            .map_err(DbError::Migrate)
    }

    pub(crate) const fn inner(&self) -> &sqlx::PgPool {
        &self.0
    }
}

use windlass_types::AlertPriority;

/// DB row representation of an alert.
#[derive(Debug, Clone)]
pub struct AlertRow {
    pub id: i64,
    pub priority: String,
    pub title: String,
    pub body: String,
    pub read: bool,
    pub created_at: String,
}

/// DB row representation of a torrent.
#[derive(Debug, Clone)]
pub struct TorrentRow {
    pub hash: String,
    pub book_id: Option<i64>,
    pub mam_id: Option<i64>,
    pub name: String,
    pub state: String,
    pub seeding_time_secs: i64,
    pub downloaded_bytes: i64,
    pub seen_at: String,
    pub added_at: String,
}

/// DB row representation of an activity record.
#[derive(Debug, Clone)]
pub struct ActivityRow {
    pub id: i64,
    pub source: String,
    pub action: String,
    pub book_id: Option<i64>,
    pub detail: Option<String>,
    pub created_at: String,
}

/// DB row representation of a book.
#[derive(Debug, Clone)]
pub struct BookRow {
    pub id: i64,
    pub mam_id: Option<i64>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub status: String,
    pub created_at: String,
}

/// DB row representation of a download queue entry.
#[derive(Debug, Clone)]
pub struct DownloadQueueRow {
    pub id: i64,
    pub book_id: Option<i64>,
    pub mam_id: i64,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Converts `AlertPriority` to the string stored in the DB.
#[must_use]
pub const fn alert_priority_str(p: AlertPriority) -> &'static str {
    match p {
        AlertPriority::Info => "info",
        AlertPriority::Warning => "warning",
        AlertPriority::Critical => "critical",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SCHEMA_ID: AtomicU64 = AtomicU64::new(0);

    pub async fn test_pool() -> DbPool {
        let admin_url = std::env::var("DATABASE_URL").expect("DATABASE_URL required for DB tests");
        let schema = format!(
            "windlass_test_{}_{}",
            std::process::id(),
            TEST_SCHEMA_ID.fetch_add(1, Ordering::Relaxed)
        );
        let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
        let quoted_schema = format!(r#""{schema}""#);
        sqlx::query(&format!("CREATE SCHEMA {quoted_schema}"))
            .execute(&admin)
            .await
            .unwrap();

        let separator = if admin_url.contains('?') { '&' } else { '?' };
        let db_url = format!("{admin_url}{separator}options=-csearch_path%3D{schema}");
        let pool = DbPool::connect(&db_url).await.unwrap();
        pool.migrate().await.unwrap();
        pool
    }

    #[tokio::test]
    async fn migrations_create_all_tables() {
        let pool = test_pool().await;
        let tables: Vec<String> = sqlx::query_scalar!(
            r#"
            SELECT table_name AS "table_name!"
            FROM information_schema.tables
            WHERE table_schema = current_schema()
            ORDER BY table_name
            "#
        )
        .fetch_all(pool.inner())
        .await
        .unwrap();
        assert!(tables.contains(&"books".to_string()), "books table missing");
        assert!(
            tables.contains(&"torrents".to_string()),
            "torrents table missing"
        );
        assert!(
            tables.contains(&"download_queue".to_string()),
            "download_queue table missing"
        );
        assert!(
            tables.contains(&"activity_log".to_string()),
            "activity_log table missing"
        );
        assert!(
            tables.contains(&"alerts".to_string()),
            "alerts table missing"
        );
        assert!(
            tables.contains(&"system_snapshots".to_string()),
            "system_snapshots table missing"
        );
    }

    #[test]
    fn alert_priority_str_maps_all_variants() {
        assert_eq!(alert_priority_str(AlertPriority::Info), "info");
        assert_eq!(alert_priority_str(AlertPriority::Warning), "warning");
        assert_eq!(alert_priority_str(AlertPriority::Critical), "critical");
    }
}
