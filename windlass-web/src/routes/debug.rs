use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use windlass_debug::DebugState;

use crate::AppState;

const EVENT_VARIANTS: &[&str] = &[
    "Init",
    "ManualReset",
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
        .route("/api/v1/debug/step/event", post(post_step_event))
        .route("/api/v1/debug/step/action", post(post_step_action))
        .with_state(state)
}

async fn get_debug_state(State(app): State<AppState>) -> Json<DebugState> {
    Json(app.debug_ctrl.debug_state())
}

async fn post_enable(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.enable_debug(app.observations.clone());
    StatusCode::OK
}

async fn post_disable(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.disable_debug();
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

/// Releases one step permit — the event loop will process the next queued event.
async fn post_step_event(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.release_event_step();
    StatusCode::OK
}

/// Releases one step permit — the event loop will dispatch the next queued action.
async fn post_step_action(State(app): State<AppState>) -> StatusCode {
    app.debug_ctrl.release_action_step();
    StatusCode::OK
}
