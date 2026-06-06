pub mod config;
mod db_shell;
mod disk_shell;
mod docker_shell;
mod domain_shell;
mod init;
mod mam_shell;
mod qbit_shell;
mod service;
mod vpn_shell;

use std::sync::Arc;

use anyhow::Result;
use tracing::info;
use windlass_observability::ObservabilityController;

use init::init_shell;

/// Entry point for the imperative shell.
///
/// §37j: the live `ObservabilityController` is constructed in `main`
/// (so logs can be captured from boot) and passed in here.  Pausing,
/// stepping, breakpoints, log capture, and the SSE stream all live in
/// `windlass-observability`.
///
/// Each per-system runtime is its own `tokio::spawn`ed task driven by
/// its own event channel; the shells own the I/O sites and feed
/// typed events directly.  This function only blocks waiting for a
/// shutdown signal — the runtimes are self-driving.
pub async fn run(observability: Arc<ObservabilityController>) -> Result<()> {
    // `init_shell` spawns every per-core runtime, every forwarder
    // task, the HTTP server, and dispatches the boot Init events.
    // The bundle it returns is kept alive (rather than dropped)
    // because it owns the runtime/handle/client values the spawned
    // tasks reference via clones.
    let _shell = init_shell(observability).await?;

    info!("Shell up; waiting for shutdown signal (Ctrl+C)");
    let _ = tokio::signal::ctrl_c().await;
    info!("Shutdown signal received");

    Ok(())
}
