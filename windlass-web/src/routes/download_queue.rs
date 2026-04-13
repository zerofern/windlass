use crate::AppState;
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
struct DownloadQueueJson {
    id: i64,
    mam_id: i64,
    title: Option<String>,
    status: String,
    created_at: String,
    updated_at: String,
}

/// Builds the router for download-queue endpoints.
#[must_use = "pass to Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/download-queue", get(get_download_queue))
        .with_state(state)
}

async fn get_download_queue(
    State(app): State<AppState>,
) -> Result<Json<Vec<DownloadQueueJson>>, StatusCode> {
    let queue = windlass_db::download_queue::get_all(&app.db_pool)
        .await
        .map_err(|e| {
            tracing::warn!("Failed to fetch download queue: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let books = windlass_db::books::get_all(&app.db_pool)
        .await
        .map_err(|e| {
            tracing::warn!("Failed to fetch books for queue: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let book_map: HashMap<i64, Option<String>> =
        books.into_iter().map(|b| (b.id, b.title)).collect();

    let json = queue
        .into_iter()
        .map(|r| {
            let title = r
                .book_id
                .and_then(|id| book_map.get(&id))
                .cloned()
                .flatten();
            DownloadQueueJson {
                id: r.id,
                mam_id: r.mam_id,
                title,
                status: r.status,
                created_at: r.created_at,
                updated_at: r.updated_at,
            }
        })
        .collect();

    Ok(Json(json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn get_download_queue_empty_db_returns_empty_array() {
        let (state, _dir) = crate::test_helpers::test_state().await;
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/download-queue")
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
}
