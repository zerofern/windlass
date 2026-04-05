#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![allow(dead_code)]

mod core;
mod shell;
mod types;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    shell::run().await
}
