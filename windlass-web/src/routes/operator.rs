use crate::AppState;
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde_json::{Value, json};

/// Builds the router for operator-control endpoints.
#[must_use = "pass to axum::serve or Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/operator/state", get(get_state))
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
