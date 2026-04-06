use std::collections::HashSet;
use std::sync::Arc;
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use crate::wiremock_admin::WireMockAdmin;
use crate::scenarios;

#[derive(Clone)]
pub struct ChaosState {
    pub qbit:   WireMockAdmin,
    pub mam:    WireMockAdmin,
    pub gotify: WireMockAdmin,
    /// Currently active scenario IDs (empty = happy path).
    pub active: Arc<RwLock<HashSet<String>>>,
}

pub async fn run(qbit_admin: &str, mam_admin: &str, gotify_admin: &str) -> anyhow::Result<()> {
    let state = Arc::new(ChaosState {
        qbit:   WireMockAdmin::new(qbit_admin),
        mam:    WireMockAdmin::new(mam_admin),
        gotify: WireMockAdmin::new(gotify_admin),
        active: Arc::new(RwLock::new(HashSet::new())),
    });

    apply_happy_path(&state).await?;
    tracing::info!("Chaos controller: happy-path stubs loaded");

    let app = Router::new()
        .route("/scenario/{name}", post(scenario_handler))
        .route("/reset", post(reset_handler))
        .route("/active", get(active_handler))
        .route("/health", get(|| async { axum::http::StatusCode::OK }))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9000").await?;
    tracing::info!("Chaos controller listening on :9000");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn apply_happy_path(state: &ChaosState) -> anyhow::Result<()> {
    state.qbit.set_mappings(scenarios::happy_path_qbit()).await?;
    state.mam.set_mappings(scenarios::happy_path_mam()).await?;
    state.gotify.set_mappings(scenarios::happy_path_gotify()).await?;
    state.qbit.reset_requests().await?;
    state.mam.reset_requests().await?;
    state.gotify.reset_requests().await?;
    Ok(())
}

async fn active_handler(State(s): State<Arc<ChaosState>>) -> Json<Value> {
    let active = s.active.read().await;
    Json(json!({ "active": active.iter().collect::<Vec<_>>() }))
}

async fn reset_handler(State(s): State<Arc<ChaosState>>) -> axum::http::StatusCode {
    match apply_happy_path(&s).await {
        Ok(()) => {
            s.active.write().await.clear();
            tracing::info!("Chaos: reset to happy-path");
            axum::http::StatusCode::OK
        }
        Err(e) => {
            tracing::error!("Chaos reset failed: {e}");
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn scenario_handler(
    State(s): State<Arc<ChaosState>>,
    Path(name): Path<String>,
) -> (axum::http::StatusCode, Json<Value>) {
    let result = match name.as_str() {
        "qbit-auth-fail" => s.qbit.set_mappings(scenarios::qbit_auth_fail()).await,
        "mam-rate-limit" => s.mam.set_mappings(scenarios::mam_rate_limit()).await,
        _ => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown scenario: {name}")})),
            );
        }
    };
    match result {
        Ok(()) => {
            s.active.write().await.insert(name.clone());
            tracing::info!("Chaos: applied scenario '{name}'");
            (axum::http::StatusCode::OK, Json(json!({"scenario": name})))
        }
        Err(e) => {
            tracing::error!("Chaos scenario '{name}' failed: {e}");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        }
    }
}
