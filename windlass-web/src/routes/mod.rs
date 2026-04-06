use crate::AppState;
use axum::Router;

mod debug;
mod health;
mod operator;
mod stream;

/// Combines all sub-routers into a single [`Router`].
#[must_use = "pass to axum::serve"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(health::router(state.clone()))
        .merge(operator::router(state.clone()))
        .merge(stream::router(state.clone()))
        .merge(debug::router(state))
}
