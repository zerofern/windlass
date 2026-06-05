#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

mod gluetun;

use windlass_testkit::mam;

use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mode = std::env::var("TESTKIT_MODE").unwrap_or_else(|_| "mam".to_string());

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
        "mam" => {
            mam::run().await?;
        }
        other => anyhow::bail!("Unknown TESTKIT_MODE: {other}. Use 'gluetun' or 'mam'"),
    }

    Ok(())
}
