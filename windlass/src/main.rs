#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![allow(dead_code)]

mod shell;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use windlass_debug::{DebugController, DebugLogLayer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (debug_ctrl, debug_owned) = DebugController::new_with_owned();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let debug_log_layer =
        DebugLogLayer::new(debug_ctrl.log_tx.clone(), debug_ctrl.debug_mode_flag());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(debug_log_layer)
        .init();

    shell::run(debug_ctrl, debug_owned).await
}
