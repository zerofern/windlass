use axum::Router;
use crate::AppState;

mod health;
mod operator;

/// Combines all sub-routers into a single [`Router`].
#[must_use = "pass to axum::serve"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(health::router())
        .merge(operator::router(state))
}
