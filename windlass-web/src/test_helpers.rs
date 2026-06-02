use crate::AppState;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, mpsc};
use windlass_observability::ObservabilityController;

static TEST_SCHEMA_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) async fn test_state() -> AppState {
    test_state_with_observability(ObservabilityController::new()).await
}

pub(crate) async fn test_state_with_observability(
    observability: Arc<ObservabilityController>,
) -> AppState {
    let admin_url = std::env::var("DATABASE_URL").expect("DATABASE_URL required for web tests");
    let schema = format!(
        "windlass_web_test_{}_{}",
        std::process::id(),
        TEST_SCHEMA_ID.fetch_add(1, Ordering::Relaxed)
    );
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();

    let separator = if admin_url.contains('?') { '&' } else { '?' };
    let database_url = format!("{admin_url}{separator}options=-csearch_path%3D{schema}");
    let pool = windlass_db::DbPool::connect(&database_url).await.unwrap();
    pool.migrate().await.unwrap();
    let (event_tx, _rx) = mpsc::channel(1);
    let (obs_tx, _) = broadcast::channel(1);
    let (domain_command_tx, _domain_cmd_rx) = mpsc::unbounded_channel();
    AppState {
        event_tx,
        domain_command_tx,
        debug_ctrl: windlass_debug::DebugController::new(),
        observability,
        observations: obs_tx,
        chaos_url: None,
        db_pool: pool,
    }
}
