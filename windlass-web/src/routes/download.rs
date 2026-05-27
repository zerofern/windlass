use crate::AppState;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use chrono::Utc;
use serde::Deserialize;
use windlass_core::events::Event;
use windlass_types::MamTorrentId;

#[derive(Deserialize)]
struct AddDownloadBody {
    /// Full MAM URL (`https://www.myanonamouse.net/t/12345`) or numeric ID.
    mam_url: Option<String>,
    /// Numeric MAM torrent ID (alternative to `mam_url`).
    mam_id: Option<u64>,
}

/// Builds the router for manual-download endpoints.
#[must_use = "pass to Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/download/add", post(post_add_download))
        .with_state(state)
}

async fn post_add_download(
    State(app): State<AppState>,
    Json(body): Json<AddDownloadBody>,
) -> StatusCode {
    let mam_id = resolve_mam_id(&body);
    let Some(mam_id) = mam_id else {
        return StatusCode::BAD_REQUEST;
    };

    let event = Event::ManualDownloadRequested {
        at: Utc::now(),
        mam_id,
    };
    if app.event_tx.send(event).await.is_err() {
        tracing::warn!("Event channel closed — could not queue ManualDownloadRequested");
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::ACCEPTED
}

fn resolve_mam_id(body: &AddDownloadBody) -> Option<MamTorrentId> {
    if let Some(ref url) = body.mam_url {
        return MamTorrentId::from_url_or_id(url);
    }
    body.mam_id.and_then(|id| MamTorrentId::try_new(id).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_mam_id_from_url() {
        let body = AddDownloadBody {
            mam_url: Some("https://www.myanonamouse.net/t/99".into()),
            mam_id: None,
        };
        assert_eq!(
            resolve_mam_id(&body),
            Some(MamTorrentId::try_new(99).unwrap())
        );
    }

    #[test]
    fn resolve_mam_id_from_numeric_field() {
        let body = AddDownloadBody {
            mam_url: None,
            mam_id: Some(42),
        };
        assert_eq!(
            resolve_mam_id(&body),
            Some(MamTorrentId::try_new(42).unwrap())
        );
    }

    #[test]
    fn resolve_mam_id_rejects_zero() {
        let body = AddDownloadBody {
            mam_url: None,
            mam_id: Some(0),
        };
        assert_eq!(resolve_mam_id(&body), None);
    }

    #[test]
    fn resolve_mam_id_rejects_both_absent() {
        let body = AddDownloadBody {
            mam_url: None,
            mam_id: None,
        };
        assert_eq!(resolve_mam_id(&body), None);
    }

    #[test]
    fn resolve_mam_id_url_takes_precedence_over_numeric() {
        let body = AddDownloadBody {
            mam_url: Some("https://www.myanonamouse.net/t/77".into()),
            mam_id: Some(99),
        };
        assert_eq!(
            resolve_mam_id(&body),
            Some(MamTorrentId::try_new(77).unwrap())
        );
    }
}
