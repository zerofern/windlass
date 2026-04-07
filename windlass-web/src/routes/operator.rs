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
        .with_state(state)
}

async fn get_state(State(app): State<AppState>) -> Json<Value> {
    let state = app.state.load_full();
    Json(json!({
        "debug_mode": app.debug_ctrl.is_debug_mode(),
        "state": serde_json::to_value(&*state)
            .unwrap_or_else(|_| json!({"error": "serialization failed"})),
    }))
}

async fn post_reset(State(app): State<AppState>) -> StatusCode {
    let _ = app.event_tx.send(Event::ManualReset).await;
    StatusCode::ACCEPTED
}
