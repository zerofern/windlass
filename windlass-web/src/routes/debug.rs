use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{
        Sse,
        sse::{Event as SseEvent, KeepAlive},
    },
    routing::{delete, get, post, put},
};
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;
use windlass_core::Observation;
use windlass_debug::DebugState;

use crate::AppState;

const EVENT_VARIANTS: &[&str] = &[
    "Init",
    "DockerGluetunDied",
    "DockerGluetunHealthy",
    "PortFileReadResult",
    "QbitAuthSuccess",
    "QbitAuthFailed",
    "QbitConnectionRefused",
    "QbitApiError",
    "QbitPortSyncSuccess",
    "QbitPortSyncFailed",
    "MamUpdateSuccess",
    "MamAsnMismatch",
    "MamStatusObserved",
    "DiskSpaceObserved",
    "NewTorrentsObserved",
    "LogsDumped",
    "Wakeup",
    "MamRateLimitViolation",
];

const ACTION_VARIANTS: &[&str] = &[
    "ScheduleWakeup",
    "ReadPortFiles",
    "FetchAndDumpAllLogs",
    "StopDependentContainers",
    "StartDependentContainers",
    "RestartGluetun",
    "AuthenticateQbit",
    "SyncQbitPort",
    "UpdateMam",
    "CheckMamConnectability",
    "CheckDiskSpace",
    "CheckNewTorrents",
    "SendGotifyAlert",
];

/// Builds the router for all debug-mode endpoints.
#[must_use = "pass to Router::merge"]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/debug", get(get_debug_state))
        .route("/api/v1/debug/stream", get(debug_stream_handler))
        .route("/api/v1/debug/enable", post(post_enable))
        .route("/api/v1/debug/disable", post(post_disable))
        .route("/api/v1/debug/events", get(get_event_variants))
        .route("/api/v1/debug/actions", get(get_action_variants))
        .route(
            "/api/v1/debug/breakpoints/event/{variant}",
            post(post_event_breakpoint).delete(delete_event_breakpoint),
        )
        .route(
            "/api/v1/debug/breakpoints/action/{variant}",
            post(post_action_breakpoint).delete(delete_action_breakpoint),
        )
        .route("/api/v1/debug/step", post(post_step))
        .route("/api/v1/debug/skip", post(post_skip))
        // Queue manipulation
        .route("/api/v1/debug/queue", post(post_inject))
        .route("/api/v1/debug/queue/order", put(put_queue_order))
        .route(
            "/api/v1/debug/queue/{id}",
            delete(delete_queue_event).put(put_queue_event),
        )
        .with_state(state)
}

async fn get_debug_state(State(app): State<AppState>) -> Json<DebugState> {
    Json((*app.debug_ctrl.snapshot.load_full()).clone())
}

async fn post_enable(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.enable_debug();
    let _ = app.observations.send(Observation::DebugModeChanged(true));
    StatusCode::OK
}

async fn post_disable(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.disable_debug();
    let _ = app.observations.send(Observation::DebugModeChanged(false));
    StatusCode::OK
}

async fn get_event_variants() -> Json<&'static [&'static str]> {
    Json(EVENT_VARIANTS)
}

async fn get_action_variants() -> Json<&'static [&'static str]> {
    Json(ACTION_VARIANTS)
}

async fn post_event_breakpoint(
    State(app): State<AppState>,
    Path(variant): Path<String>,
) -> StatusCode {
    app.debug_ctrl.add_event_breakpoint(variant);
    StatusCode::OK
}

async fn delete_event_breakpoint(
    State(app): State<AppState>,
    Path(variant): Path<String>,
) -> StatusCode {
    app.debug_ctrl.remove_event_breakpoint(&variant);
    StatusCode::OK
}

async fn post_action_breakpoint(
    State(app): State<AppState>,
    Path(variant): Path<String>,
) -> StatusCode {
    app.debug_ctrl.add_action_breakpoint(variant);
    StatusCode::OK
}

async fn delete_action_breakpoint(
    State(app): State<AppState>,
    Path(variant): Path<String>,
) -> StatusCode {
    app.debug_ctrl.remove_action_breakpoint(&variant);
    StatusCode::OK
}

/// Releases one step permit — the currently-paused event or action will execute.
async fn post_step(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.release_step();
    StatusCode::OK
}

/// Skips the currently-paused event or action without executing it.
async fn post_skip(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.request_skip();
    StatusCode::OK
}

// ── Queue manipulation ────────────────────────────────────────────────────────

/// Removes a queued event by ID. No-op if the ID is not in the queue.
async fn delete_queue_event(State(app): State<AppState>, Path(id): Path<Uuid>) -> StatusCode {
    let _ = app
        .debug_ctrl
        .cmd_tx
        .send(windlass_debug::DebugCommand::RemoveQueuedEvent(id))
        .await;
    StatusCode::OK
}

#[derive(Deserialize)]
struct EditPayload {
    payload: Value,
}

/// Replaces the payload of a queued event. Returns 400 if the payload does not
/// deserialise to a valid `Event`.
async fn put_queue_event(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<EditPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = app
        .debug_ctrl
        .cmd_tx
        .send(windlass_debug::DebugCommand::EditQueuedEvent(
            id,
            body.payload,
            reply_tx,
        ))
        .await;
    match reply_rx.await {
        Ok(Ok(())) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "channel closed"})),
        ),
    }
}

#[derive(Deserialize)]
struct InjectBody {
    payload: Value,
    position: Option<usize>,
    at: Option<chrono::DateTime<Utc>>,
}

/// Injects a new event into the queue. `payload` must be a valid serialised
/// `Event`. `position` defaults to the end of the queue. `at` defaults to now.
async fn post_inject(
    State(app): State<AppState>,
    Json(body): Json<InjectBody>,
) -> (StatusCode, Json<serde_json::Value>) {
    let at = body.at.unwrap_or_else(Utc::now);
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = app
        .debug_ctrl
        .cmd_tx
        .send(windlass_debug::DebugCommand::InjectEvent {
            payload: body.payload,
            position: body.position,
            at,
            reply: reply_tx,
        })
        .await;
    match reply_rx.await {
        Ok(Ok(id)) => (StatusCode::CREATED, Json(serde_json::json!({"id": id}))),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "channel closed"})),
        ),
    }
}

#[derive(Deserialize)]
struct OrderBody {
    ids: Vec<Uuid>,
}

/// Reorders the queue. `ids` must list every queued event ID in the desired order.
async fn put_queue_order(
    State(app): State<AppState>,
    Json(body): Json<OrderBody>,
) -> (StatusCode, Json<serde_json::Value>) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = app
        .debug_ctrl
        .cmd_tx
        .send(windlass_debug::DebugCommand::ReorderQueue(
            body.ids, reply_tx,
        ))
        .await;
    match reply_rx.await {
        Ok(Ok(())) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "channel closed"})),
        ),
    }
}

/// SSE stream for the debug page.
///
/// Pushes the full [`DebugState`] snapshot as a `"snapshot"` event immediately
/// on connect, then again on every change. The `seq` field in the snapshot
/// lets the frontend discard events that arrived before its initial GET.
///
/// Only publishes while debug mode is active (the main loop's `publish()` is a
/// no-op when debug mode is off, so the stream goes silent but stays connected).
async fn debug_stream_handler(
    State(app): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    // Push the current snapshot immediately so the client has a baseline.
    let initial = {
        let state = app.debug_ctrl.snapshot.load_full();
        let json = serde_json::to_string(&*state).unwrap_or_default();
        stream::once(async move {
            Ok::<_, std::convert::Infallible>(SseEvent::default().event("snapshot").data(json))
        })
    };

    // Subscribe to future snapshot changes via notify_tx.
    let rx = app.debug_ctrl.notify_tx.subscribe();
    let ctrl = app.debug_ctrl.clone();
    let live = BroadcastStream::new(rx).filter_map(move |_msg| {
        let ctrl = ctrl.clone();
        async move {
            // Load the latest snapshot regardless of which seq triggered this.
            // Lagged notifications are fine — we always serve fresh data.
            let state = ctrl.snapshot.load_full();
            let json = serde_json::to_string(&*state).unwrap_or_default();
            Some(Ok(SseEvent::default().event("snapshot").data(json)))
        }
    });

    Sse::new(initial.chain(live)).keep_alive(KeepAlive::default())
}
