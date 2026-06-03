#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![allow(dead_code)]

mod shell;

use std::sync::Arc;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use windlass_observability::{ObservabilityController, ObservabilityLogLayer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Construct the observability controller first so the log layer
    // can capture every tracing event from boot onwards into the SSE
    // stream.  Budgets come from `WINDLASS_OBS_*` env vars with the
    // §37pre B7 constants as defaults (decision 19).
    // PAUSE_ON_START is applied here so cores that should start
    // paused are already paused before any runtime spawns.
    let obs_config = shell::config::load_observability_config()
        .map_err(|e| anyhow::anyhow!("WINDLASS_OBS_* config: {e}"))?;
    let observability = ObservabilityController::with_config(obs_config);
    let pause_on_start_raw = std::env::var("PAUSE_ON_START").ok();
    let pre_paused = windlass_observability::parse_pause_on_start(pause_on_start_raw.as_deref())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    for core in &pre_paused {
        observability.pause(*core);
    }

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let log_layer = ObservabilityLogLayer::new(Arc::downgrade(&observability));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(log_layer)
        .init();

    if !pre_paused.is_empty() {
        tracing::info!(
            "PAUSE_ON_START: pre-pausing cores: {}",
            pre_paused
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    shell::run(observability).await
}
