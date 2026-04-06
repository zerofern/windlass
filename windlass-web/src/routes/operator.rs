use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use windlass_core::events::Event;
use crate::AppState;

/// Builds the router for operator-control endpoints.
#[must_use = "pass to axum::serve or Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/operator/state", get(get_state))
        .route("/api/v1/operator/reset", post(post_reset))
        .route("/api/v1/operator/freeze", post(post_freeze))
        .route("/api/v1/operator/unfreeze", post(post_unfreeze))
        .with_state(state)
}

async fn get_state(State(app): State<AppState>) -> Json<Value> {
    let state = app.state.read().await;
    Json(json!({
        "frozen": app.debug_gate.is_frozen(),
        "state": serde_json::to_value(&*state)
            .unwrap_or_else(|_| json!({"error": "serialization failed"})),
    }))
}

async fn post_reset(State(app): State<AppState>) -> StatusCode {
    let _ = app.event_tx.send(Event::ManualReset).await;
    StatusCode::ACCEPTED
}

async fn post_freeze(State(app): State<AppState>) -> StatusCode {
    app.debug_gate.freeze();
    StatusCode::OK
}

async fn post_unfreeze(State(app): State<AppState>) -> StatusCode {
    app.debug_gate.unfreeze();
    StatusCode::OK
}
