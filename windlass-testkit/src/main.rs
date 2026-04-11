#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

mod chaos;
mod gluetun;
mod scenarios;
mod wiremock_admin;

use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mode = std::env::var("TESTKIT_MODE").unwrap_or_else(|_| "chaos".to_string());

    match mode.as_str() {
        "gluetun" => {
            let ip_file = PathBuf::from(
                std::env::var("VPN_IP_FILE").unwrap_or_else(|_| "/tmp/gluetun/ip".to_string()),
            );
            let port_file = PathBuf::from(
                std::env::var("VPN_PORT_FILE")
                    .unwrap_or_else(|_| "/tmp/gluetun/forwarded_port".to_string()),
            );
            if let Some(parent) = ip_file.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            gluetun::run(ip_file, port_file).await?;
        }
        "chaos" => {
            let qbit_admin = std::env::var("QBIT_ADMIN_URL")
                .unwrap_or_else(|_| "http://mock-qbittorrent:8080/__admin".to_string());
            let mam_admin = std::env::var("MAM_ADMIN_URL")
                .unwrap_or_else(|_| "http://mock-mam:8080/__admin".to_string());
            let gluetun_control = std::env::var("GLUETUN_CONTROL_URL")
                .unwrap_or_else(|_| "http://mock-gluetun:9001".to_string());
            chaos::run(&qbit_admin, &mam_admin, &gluetun_control).await?;
        }
        other => anyhow::bail!("Unknown TESTKIT_MODE: {other}. Use 'gluetun' or 'chaos'"),
    }

    Ok(())
}
