#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod alerts;
pub mod books;
pub mod download_queue;
pub mod events;
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

/// Typed handle over the underlying `SQLite` connection pool.
///
/// WAL mode is mandatory: it allows concurrent readers alongside the single
/// writer, which is the correct model for Windlass — the shell writes (action
/// execution) and the web handlers read (API responses) concurrently.
/// sqlx serializes writers internally; no channel actor is needed.
#[derive(Clone)]
pub struct DbPool(sqlx::SqlitePool);

impl DbPool {
    /// Opens (or creates) the `SQLite` database at `path` in WAL mode.
    ///
    /// # Errors
    /// Returns `DbError::Connect` if the database cannot be opened.
    pub async fn connect(path: &str) -> Result<Self, DbError> {
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
        )
        .await
        .map_err(DbError::Connect)?;
        Ok(Self(pool))
    }

    /// Runs all pending migrations from the embedded `migrations/` directory.
    ///
    /// # Errors
    /// Returns `DbError::Migrate` if any migration fails.
    pub async fn migrate(&self) -> Result<(), DbError> {
        sqlx::migrate!()
            .run(&self.0)
            .await
            .map_err(DbError::Migrate)
    }

    pub(crate) const fn inner(&self) -> &sqlx::SqlitePool {
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
}

/// DB row representation of an event.
#[derive(Debug, Clone)]
pub struct EventRow {
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
    use tempfile::TempDir;

    pub(crate) async fn test_pool() -> (DbPool, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let pool = DbPool::connect(path.to_str().unwrap()).await.unwrap();
        pool.migrate().await.unwrap();
        (pool, dir)
    }

    #[tokio::test]
    async fn migrations_create_all_five_tables() {
        let (pool, _dir) = test_pool().await;
        // Use runtime query — sqlite_master is a system table not in the sqlx cache.
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx_%' ORDER BY name"
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
            tables.contains(&"events".to_string()),
            "events table missing"
        );
        assert!(
            tables.contains(&"alerts".to_string()),
            "alerts table missing"
        );
    }

    #[test]
    fn alert_priority_str_maps_all_variants() {
        assert_eq!(alert_priority_str(AlertPriority::Info), "info");
        assert_eq!(alert_priority_str(AlertPriority::Warning), "warning");
        assert_eq!(alert_priority_str(AlertPriority::Critical), "critical");
    }

    #[tokio::test]
    async fn connect_fails_on_bad_path() {
        let result = DbPool::connect("/nonexistent/path/db.db").await;
        assert!(result.is_err(), "expected connect to fail");
    }
}
