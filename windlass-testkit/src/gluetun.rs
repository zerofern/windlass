use axum::{Json, Router, extract::State, routing::post};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
struct GluetunState {
    ip_file: PathBuf,
    port_file: PathBuf,
}

#[derive(Deserialize)]
struct SetIpPort {
    ip: String,
    port: u16,
}

pub async fn run(ip_file: PathBuf, port_file: PathBuf) -> anyhow::Result<()> {
    tokio::fs::write(&ip_file, "10.8.0.1").await?;
    tokio::fs::write(&port_file, "51820").await?;
    tracing::info!("Gluetun mock: wrote initial VPN files");

    let state = Arc::new(RwLock::new(GluetunState { ip_file, port_file }));

    let app = Router::new()
        .route("/set", post(set_handler))
        .route("/clear-port", post(clear_port_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9001").await?;
    tracing::info!("Gluetun control API listening on :9001");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn set_handler(
    State(s): State<Arc<RwLock<GluetunState>>>,
    Json(body): Json<SetIpPort>,
) -> axum::http::StatusCode {
    let (ip_file, port_file) = {
        let s = s.read().await;
        (s.ip_file.clone(), s.port_file.clone())
    };
    let _ = tokio::fs::write(&ip_file, &body.ip).await;
    let _ = tokio::fs::write(&port_file, body.port.to_string()).await;
    tracing::info!("Gluetun: set ip={} port={}", body.ip, body.port);
    axum::http::StatusCode::OK
}

async fn clear_port_handler(State(s): State<Arc<RwLock<GluetunState>>>) -> axum::http::StatusCode {
    let port_file = s.read().await.port_file.clone();
    let _ = tokio::fs::write(&port_file, "").await;
    tracing::info!("Gluetun: cleared port file");
    axum::http::StatusCode::OK
}
