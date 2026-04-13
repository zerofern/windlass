use crate::AppState;
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Serialize;
use std::collections::HashMap;

const HNR_REQUIRED_SECS: i64 = 72 * 3600;
const HNR_REQUIRED_HOURS: i64 = 72;

#[derive(Serialize)]
struct TorrentJson {
    hash: String,
    name: String,
    title: Option<String>,
    mam_id: Option<i64>,
    state: String,
    seeding_time_secs: i64,
    downloaded_bytes: i64,
    hnr_satisfied: bool,
    hnr_hours_remaining: i64,
    added_at: String,
    seen_at: String,
}

/// Builds the router for torrent-monitor endpoints.
#[must_use = "pass to Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/torrents", get(get_torrents))
        .with_state(state)
}

async fn get_torrents(State(app): State<AppState>) -> Result<Json<Vec<TorrentJson>>, StatusCode> {
    let torrents = windlass_db::torrents::get_all(&app.db_pool)
        .await
        .map_err(|e| {
            tracing::warn!("Failed to fetch torrents: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let books = windlass_db::books::get_all(&app.db_pool)
        .await
        .map_err(|e| {
            tracing::warn!("Failed to fetch books for torrents: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let book_map: HashMap<i64, Option<String>> =
        books.into_iter().map(|b| (b.id, b.title)).collect();

    let json = torrents
        .into_iter()
        .map(|t| {
            let title = t
                .book_id
                .and_then(|id| book_map.get(&id))
                .cloned()
                .flatten();
            let s = t.seeding_time_secs;
            let hnr_satisfied = s >= HNR_REQUIRED_SECS;
            let hnr_hours_remaining = if hnr_satisfied {
                0
            } else {
                HNR_REQUIRED_HOURS - s / 3600
            };
            TorrentJson {
                hash: t.hash,
                name: t.name,
                title,
                mam_id: t.mam_id,
                state: t.state,
                seeding_time_secs: s,
                downloaded_bytes: t.downloaded_bytes,
                hnr_satisfied,
                hnr_hours_remaining,
                added_at: t.added_at,
                seen_at: t.seen_at,
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
    async fn get_torrents_empty_db_returns_empty_array() {
        let (state, _dir) = crate::test_helpers::test_state().await;
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
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
