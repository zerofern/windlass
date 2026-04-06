use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

/// Builds the router for liveness-probe endpoints.
#[must_use = "pass to axum::serve or Router::merge"]
pub fn router() -> Router {
    Router::new().route("/api/v1/health", get(handler))
}

async fn handler() -> Json<Value> {
    Json(json!({"status": "ok"}))
}
