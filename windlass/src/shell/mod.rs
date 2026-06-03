mod config;
mod db_shell;
mod disk_shell;
mod docker_shell;
mod domain_shell;
mod init;
mod mam_shell;
mod qbit_shell;
mod service;
mod service_events;
mod vpn_shell;

use std::sync::Arc;

use anyhow::Result;
use tracing::debug;
use windlass_observability::ObservabilityController;

use init::{ShellRuntime, init_shell};

/// Entry point for the imperative shell.
///
/// §37j: the live `ObservabilityController` is constructed in `main`
/// (so logs can be captured from boot) and passed in here.  Pausing,
/// stepping, breakpoints, log capture, and the SSE stream all live in
/// `windlass-observability`; this loop only forwards legacy events to
/// the per-core service bridge.
pub async fn run(observability: Arc<ObservabilityController>) -> Result<()> {
    let ShellRuntime {
        mut event_rx,
        service_cores,
        ..
    } = init_shell(observability).await?;

    'main: loop {
        let event = match event_rx.recv().await {
            None => break 'main,
            Some(e) => e,
        };

        debug!(?event, "←");

        service_cores.observe(&event);
    }

    Ok(())
}
