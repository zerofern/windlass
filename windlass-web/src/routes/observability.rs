//! `/api/v1/observability/*` HTTP routes.
//!
//! Provides the SSE stream the React `/observability` page consumes
//! plus the REST endpoints for pause/resume/step and breakpoint
//! management.
//!
//! All endpoints route through [`crate::AppState::observability`],
//! which is constructed once in `main` and threaded into every
//! [`windlass_machine::ServiceRuntime::spawn`] call.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{
    Sse,
    sse::{Event as SseEvent, KeepAlive},
};
use axum::routing::{get, post};
use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use tokio_stream::wrappers::BroadcastStream;
use windlass_machine::CoreId;
use windlass_observability::{Breakpoint, SseMessage};

use crate::AppState;

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/api/v1/observability/stream", get(stream_handler))
        .route("/api/v1/observability/pause/{core}", post(pause))
        .route("/api/v1/observability/pause_all", post(pause_all))
        .route("/api/v1/observability/resume/{core}", post(resume))
        .route("/api/v1/observability/resume_all", post(resume_all))
        .route("/api/v1/observability/step/{core}", post(step))
        .route("/api/v1/observability/step_all", post(step_all))
        .route("/api/v1/observability/breakpoints", get(list_breakpoints))
        .route(
            "/api/v1/observability/breakpoints/{kind}/{value}",
            post(add_breakpoint).delete(remove_breakpoint),
        )
        .with_state(state)
}

// ── SSE stream ────────────────────────────────────────────────────────────────

async fn stream_handler(
    State(app): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    // Subscribe *before* taking the Hello snapshot so any message
    // emitted between the two is delivered after the snapshot
    // (potentially as a redundant Step/HttpExchange — harmless).
    let rx = app.observability.subscribe();
    let hello = app.observability.hello().await;
    let initial = stream::iter([SseMessage::Hello(hello)])
        .map(|msg| sse_event(&msg))
        .map(Ok);

    let live = BroadcastStream::new(rx).filter_map(|msg| async move {
        msg.ok()
            .map(|m| Ok::<_, std::convert::Infallible>(sse_event(&m)))
    });

    Sse::new(initial.chain(live)).keep_alive(KeepAlive::default())
}

fn sse_event(msg: &SseMessage) -> SseEvent {
    let json = serde_json::to_string(msg).unwrap_or_default();
    SseEvent::default().event("observability").data(json)
}

// ── Pause / step controls ─────────────────────────────────────────────────────

async fn pause(
    State(app): State<AppState>,
    Path(core): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_core(&core)?;
    app.observability.pause(id);
    Ok(StatusCode::NO_CONTENT)
}

async fn pause_all(State(app): State<AppState>) -> StatusCode {
    app.observability.pause_all();
    StatusCode::NO_CONTENT
}

async fn resume(
    State(app): State<AppState>,
    Path(core): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_core(&core)?;
    app.observability.resume(id);
    Ok(StatusCode::NO_CONTENT)
}

async fn resume_all(State(app): State<AppState>) -> StatusCode {
    app.observability.resume_all();
    StatusCode::NO_CONTENT
}

async fn step(
    State(app): State<AppState>,
    Path(core): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_core(&core)?;
    app.observability.step(id);
    Ok(StatusCode::NO_CONTENT)
}

async fn step_all(State(app): State<AppState>) -> StatusCode {
    app.observability.step_all();
    StatusCode::NO_CONTENT
}

// ── Breakpoint management ─────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum BreakpointKind {
    Event,
    Action,
    Publish,
    Http,
}

impl BreakpointKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "event" => Some(Self::Event),
            "action" => Some(Self::Action),
            "publish" => Some(Self::Publish),
            "http" => Some(Self::Http),
            _ => None,
        }
    }
}

async fn list_breakpoints(State(app): State<AppState>) -> Json<Vec<Breakpoint>> {
    Json(app.observability.active_breakpoints())
}

async fn add_breakpoint(
    State(app): State<AppState>,
    Path((kind, value)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let Some(kind) = BreakpointKind::parse(&kind) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "unknown breakpoint kind; expected one of: event, action, publish, http".into(),
        ));
    };
    match kind {
        BreakpointKind::Event => app.observability.add_event_breakpoint(value),
        BreakpointKind::Action => app.observability.add_action_breakpoint(value),
        BreakpointKind::Publish => app.observability.add_publish_breakpoint(value),
        BreakpointKind::Http => app.observability.add_http_breakpoint(value),
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_breakpoint(
    State(app): State<AppState>,
    Path((kind, value)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let Some(kind) = BreakpointKind::parse(&kind) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "unknown breakpoint kind; expected one of: event, action, publish, http".into(),
        ));
    };
    match kind {
        BreakpointKind::Event => app.observability.remove_event_breakpoint(&value),
        BreakpointKind::Action => app.observability.remove_action_breakpoint(&value),
        BreakpointKind::Publish => app.observability.remove_publish_breakpoint(&value),
        BreakpointKind::Http => app.observability.remove_http_breakpoint(&value),
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_core(s: &str) -> Result<CoreId, (StatusCode, String)> {
    match s {
        "vpn" => Ok(CoreId::Vpn),
        "qbit" => Ok(CoreId::Qbit),
        "mam" => Ok(CoreId::Mam),
        "db" => Ok(CoreId::Db),
        "disk" => Ok(CoreId::Disk),
        "docker" => Ok(CoreId::Docker),
        "domain" => Ok(CoreId::Domain),
        _ => Err((
            StatusCode::NOT_FOUND,
            format!(
                "unknown core '{s}'; expected one of: vpn, qbit, mam, db, disk, docker, domain"
            ),
        )),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;
    use windlass_observability::ObservabilityController;

    #[tokio::test]
    async fn pause_resume_round_trip() {
        let observability = ObservabilityController::new();
        let state = crate::test_helpers::test_state_with_observability(observability.clone()).await;
        let app = router(state);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/observability/pause/mam")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(observability.is_paused(CoreId::Mam));

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/observability/resume/mam")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(!observability.is_paused(CoreId::Mam));
    }

    #[tokio::test]
    async fn unknown_core_is_404() {
        let observability = ObservabilityController::new();
        let state = crate::test_helpers::test_state_with_observability(observability).await;
        let app = router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/observability/pause/bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn breakpoint_add_list_remove() {
        let observability = ObservabilityController::new();
        let state = crate::test_helpers::test_state_with_observability(observability.clone()).await;
        let app = router(state);

        // Add
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/observability/breakpoints/event/StatusFetched")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        // List
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/observability/breakpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let bps: Vec<Breakpoint> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(bps.len(), 1);

        // Remove
        let res = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/observability/breakpoints/event/StatusFetched")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(observability.active_breakpoints().is_empty());
    }
}
