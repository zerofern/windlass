use crate::AppState;
use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};

/// Builds the router for liveness-probe and configuration endpoints.
#[must_use = "pass to axum::serve or Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health_handler))
        .route("/api/v1/config", get(config_handler))
        .route("/api/v1/operator/state", get(operator_state_handler))
        .with_state(state)
}

async fn health_handler() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn config_handler(State(app): State<AppState>) -> Json<Value> {
    Json(json!({ "chaos_url": app.chaos_url }))
}

async fn operator_state_handler(State(app): State<AppState>) -> Json<Value> {
    let snapshot = app.debug_ctrl.snapshot.load_full();
    Json(json!({ "state": snapshot.latest_state.clone() }))
}
