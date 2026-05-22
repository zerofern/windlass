use crate::AppState;
use axum::Router;

mod alerts;
mod debug;
mod download;
mod download_queue;
mod activity_log;
mod health;
mod stream;
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
        .merge(stream::router(state.clone()))
        .merge(torrents::router(state.clone()))
        .merge(debug::router(state))
}
