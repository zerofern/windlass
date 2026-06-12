use crate::AppState;
use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

/// Builds the router for the liveness-probe endpoint.
#[must_use = "pass to axum::serve or Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health_handler))
        .with_state(state)
}

async fn health_handler() -> Json<Value> {
    Json(json!({"status": "ok"}))
}
