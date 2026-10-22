use crate::AppState;
use axum::Router;

mod activity_log;
mod alerts;
mod download;
mod download_queue;
mod health;
mod observability;
mod torrents;

/// Combines all sub-routers into a single [`Router`].
#[must_use = "pass to axum::serve"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(alerts::router(state.clone()))
        .merge(download::router(state.clone()))
        .merge(download_queue::router(state.clone()))
        .merge(activity_log::router(state.clone()))
        .merge(health::router(state.clone()))
        .merge(torrents::router(state.clone()))
        .merge(observability::router(state))
}
