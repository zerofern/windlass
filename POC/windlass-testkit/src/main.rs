#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use windlass_testkit::mam;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mode = std::env::var("TESTKIT_MODE").unwrap_or_else(|_| "mam".to_string());

    match mode.as_str() {
        "mam" => {
            mam::run().await?;
        }
        other => anyhow::bail!("Unknown TESTKIT_MODE: {other}. Use 'mam'"),
    }

    Ok(())
}
