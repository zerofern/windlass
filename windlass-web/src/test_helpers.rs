use crate::AppState;
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};

pub(crate) async fn test_state() -> (AppState, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pool = windlass_db::DbPool::connect(db_path.to_str().unwrap())
        .await
        .unwrap();
    pool.migrate().await.unwrap();
    let (event_tx, _rx) = mpsc::channel(1);
    let (obs_tx, _) = broadcast::channel(1);
    let state = AppState {
        event_tx,
        debug_ctrl: windlass_debug::DebugController::new(),
        observations: obs_tx,
        chaos_url: None,
        db_pool: pool,
    };
    (state, dir)
}
