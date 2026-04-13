use crate::AppState;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};

const DEFAULT_EVENT_LIMIT: i64 = 100;

#[derive(Deserialize)]
struct EventsQuery {
    limit: Option<i64>,
}

#[derive(Serialize)]
struct EventJson {
    id: i64,
    source: String,
    action: String,
    book_id: Option<i64>,
    detail: Option<String>,
    created_at: String,
}

/// Builds the router for event-log endpoints.
#[must_use = "pass to Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/events", get(get_events))
        .with_state(state)
}

async fn get_events(
    State(app): State<AppState>,
    Query(params): Query<EventsQuery>,
) -> Result<Json<Vec<EventJson>>, StatusCode> {
    let limit = params.limit.unwrap_or(DEFAULT_EVENT_LIMIT);
    let events = windlass_db::events::get_recent(&app.db_pool, limit)
        .await
        .map_err(|e| {
            tracing::warn!("Failed to fetch events: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(
        events
            .into_iter()
            .map(|r| EventJson {
                id: r.id,
                source: r.source,
                action: r.action,
                book_id: r.book_id,
                detail: r.detail,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn get_events_empty_db_returns_empty_array() {
        let (state, _dir) = crate::test_helpers::test_state().await;
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_events_limit_param_is_respected() {
        let (state, _dir) = crate::test_helpers::test_state().await;
        for i in 0..10_i32 {
            windlass_db::events::insert(&state.db_pool, "test", &format!("action{i}"), None, None)
                .await
                .unwrap();
        }
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/events?limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 5);
    }
}
