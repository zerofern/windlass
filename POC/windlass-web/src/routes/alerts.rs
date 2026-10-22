use crate::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;

#[derive(Serialize)]
struct AlertJson {
    id: i64,
    priority: String,
    title: String,
    body: String,
    read: bool,
    created_at: String,
}

/// Builds the router for alert endpoints.
#[must_use = "pass to Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/alerts", get(get_alerts))
        .route("/api/v1/alerts/{id}/read", post(post_mark_read))
        .with_state(state)
}

async fn get_alerts(State(app): State<AppState>) -> Result<Json<Vec<AlertJson>>, StatusCode> {
    windlass_db::alerts::get_all(&app.db_pool)
        .await
        .map(|rows| {
            Json(
                rows.into_iter()
                    .map(|r| AlertJson {
                        id: r.id,
                        priority: r.priority,
                        title: r.title,
                        body: r.body,
                        read: r.read,
                        created_at: r.created_at,
                    })
                    .collect(),
            )
        })
        .map_err(|e| {
            tracing::warn!("Failed to fetch alerts: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

async fn post_mark_read(State(app): State<AppState>, Path(id): Path<i64>) -> StatusCode {
    match windlass_db::alerts::mark_read(&app.db_pool, id).await {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::warn!("Failed to mark alert {id} as read: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
