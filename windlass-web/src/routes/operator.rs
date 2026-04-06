use crate::AppState;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::{Value, json};
use windlass_core::events::Event;

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
        "frozen": app.debug_ctrl.is_frozen(),
        "state": serde_json::to_value(&*state)
            .unwrap_or_else(|_| json!({"error": "serialization failed"})),
    }))
}

async fn post_reset(State(app): State<AppState>) -> StatusCode {
    let _ = app.event_tx.send(Event::ManualReset).await;
    StatusCode::ACCEPTED
}

async fn post_freeze(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.freeze();
    StatusCode::OK
}

async fn post_unfreeze(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.unfreeze();
    StatusCode::OK
}
