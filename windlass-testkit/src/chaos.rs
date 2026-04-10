use crate::scenarios;
use crate::wiremock_admin::WireMockAdmin;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

// ── GluetunClient ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct GluetunClient {
    client: reqwest::Client,
    base: String,
}

impl GluetunClient {
    pub fn new(base: &str) -> Self {
        Self { client: reqwest::Client::new(), base: base.to_owned() }
    }

    pub async fn set(&self, ip: &str, port: u16) -> anyhow::Result<()> {
        self.client
            .post(format!("{}/set", self.base))
            .json(&json!({ "ip": ip, "port": port }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn clear_port(&self) -> anyhow::Result<()> {
        self.client
            .post(format!("{}/clear-port", self.base))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

// ── VpnFileState ──────────────────────────────────────────────────────────────

/// Tracks the last known good VPN ip/port so `health/up` can restore them.
pub struct VpnFileState {
    pub ip: String,
    pub port: u16,
    pub healthy: bool,
}

impl Default for VpnFileState {
    fn default() -> Self {
        Self { ip: "10.8.0.1".to_owned(), port: 51820, healthy: true }
    }
}

// ── ChaosState ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ChaosState {
    pub qbit: WireMockAdmin,
    pub mam: WireMockAdmin,
    pub gotify: WireMockAdmin,
    pub gluetun: GluetunClient,
    /// Currently active fault scenario IDs (empty = happy path).
    pub active: Arc<RwLock<HashSet<String>>>,
    /// Last written VPN file values + whether the port file is currently valid.
    pub vpn: Arc<RwLock<VpnFileState>>,
}

pub async fn run(
    qbit_admin: &str,
    mam_admin: &str,
    gotify_admin: &str,
    gluetun_control: &str,
) -> anyhow::Result<()> {
    let state = Arc::new(ChaosState {
        qbit: WireMockAdmin::new(qbit_admin),
        mam: WireMockAdmin::new(mam_admin),
        gotify: WireMockAdmin::new(gotify_admin),
        gluetun: GluetunClient::new(gluetun_control),
        active: Arc::new(RwLock::new(HashSet::new())),
        vpn: Arc::new(RwLock::new(VpnFileState::default())),
    });

    apply_happy_path(&state).await?;
    tracing::info!("Chaos controller: happy-path stubs loaded");

    let app = Router::new()
        // Fault scenarios
        .route("/scenario/{name}", post(scenario_handler))
        .route("/reset", post(reset_handler))
        .route("/active", get(active_handler))
        // Gluetun controls
        .route("/gluetun/state", get(gluetun_state_handler))
        .route("/gluetun/set-files", post(gluetun_set_files_handler))
        .route("/gluetun/health/down", post(gluetun_health_down_handler))
        .route("/gluetun/health/up", post(gluetun_health_up_handler))
        // Misc
        .route("/health", get(|| async { StatusCode::OK }))
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

// ── Fault scenario handlers ───────────────────────────────────────────────────

async fn active_handler(State(s): State<Arc<ChaosState>>) -> Json<Value> {
    let active = s.active.read().await;
    Json(json!({ "active": active.iter().collect::<Vec<_>>() }))
}

async fn reset_handler(State(s): State<Arc<ChaosState>>) -> StatusCode {
    match apply_happy_path(&s).await {
        Ok(()) => {
            s.active.write().await.clear();
            tracing::info!("Chaos: reset to happy-path");
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!("Chaos reset failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn scenario_handler(
    State(s): State<Arc<ChaosState>>,
    Path(name): Path<String>,
) -> (StatusCode, Json<Value>) {
    let result = match name.as_str() {
        "qbit-auth-fail"          => s.qbit.set_mappings(scenarios::qbit_auth_fail()).await,
        "qbit-connection-refused" => s.qbit.set_mappings(scenarios::qbit_connection_refused()).await,
        "mam-rate-limit"          => s.mam.set_mappings(scenarios::mam_rate_limit()).await,
        "mam-not-connectable"     => s.mam.set_mappings(scenarios::mam_not_connectable()).await,
        "mam-asn-mismatch"        => s.mam.set_mappings(scenarios::mam_asn_mismatch()).await,
        "gotify-down"             => s.gotify.set_mappings(scenarios::gotify_down()).await,
        _ => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": format!("unknown scenario: {name}")})));
        }
    };
    match result {
        Ok(()) => {
            s.active.write().await.insert(name.clone());
            tracing::info!("Chaos: applied scenario '{name}'");
            (StatusCode::OK, Json(json!({"scenario": name})))
        }
        Err(e) => {
            tracing::error!("Chaos scenario '{name}' failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
        }
    }
}

// ── Gluetun handlers ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GluetunStateResponse {
    ip: String,
    port: u16,
    healthy: bool,
}

async fn gluetun_state_handler(State(s): State<Arc<ChaosState>>) -> Json<GluetunStateResponse> {
    let vpn = s.vpn.read().await;
    Json(GluetunStateResponse { ip: vpn.ip.clone(), port: vpn.port, healthy: vpn.healthy })
}

#[derive(Deserialize)]
struct SetFilesBody {
    ip: String,
    port: u16,
}

/// Updates the VPN ip/port files. Triggers `PortFileReadResult` via the file
/// watcher. Also restores a healthy state if the port file was previously cleared.
async fn gluetun_set_files_handler(
    State(s): State<Arc<ChaosState>>,
    Json(body): Json<SetFilesBody>,
) -> (StatusCode, Json<Value>) {
    match s.gluetun.set(&body.ip, body.port).await {
        Ok(()) => {
            let mut vpn = s.vpn.write().await;
            vpn.ip = body.ip.clone();
            vpn.port = body.port;
            vpn.healthy = true;
            tracing::info!("Chaos/gluetun: set VPN files ip={} port={}", body.ip, body.port);
            (StatusCode::OK, Json(json!({"ip": body.ip, "port": body.port})))
        }
        Err(e) => {
            tracing::error!("Chaos/gluetun: set-files failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
        }
    }
}

/// Clears the VPN port file so the Docker healthcheck fails.
/// Triggers `DockerGluetunDied` via the Docker event watcher.
async fn gluetun_health_down_handler(State(s): State<Arc<ChaosState>>) -> (StatusCode, Json<Value>) {
    match s.gluetun.clear_port().await {
        Ok(()) => {
            s.vpn.write().await.healthy = false;
            tracing::info!("Chaos/gluetun: port file cleared (health → down)");
            (StatusCode::OK, Json(json!({"healthy": false})))
        }
        Err(e) => {
            tracing::error!("Chaos/gluetun: health/down failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
        }
    }
}

/// Restores the last known VPN ip/port so the Docker healthcheck passes again.
/// Triggers `DockerGluetunHealthy` via the Docker event watcher.
async fn gluetun_health_up_handler(State(s): State<Arc<ChaosState>>) -> (StatusCode, Json<Value>) {
    let (ip, port) = {
        let vpn = s.vpn.read().await;
        (vpn.ip.clone(), vpn.port)
    };
    match s.gluetun.set(&ip, port).await {
        Ok(()) => {
            s.vpn.write().await.healthy = true;
            tracing::info!("Chaos/gluetun: port file restored (health → up) ip={ip} port={port}");
            (StatusCode::OK, Json(json!({"healthy": true, "ip": ip, "port": port})))
        }
        Err(e) => {
            tracing::error!("Chaos/gluetun: health/up failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
        }
    }
}
