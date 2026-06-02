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

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::debug;

use windlass_debug::{DebugController, DebugHistory};

use init::{ShellRuntime, init_shell};

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// then runs the event loop forever.
///
/// §37d closeout: the legacy debug-mode pause/queue path is gone with
/// `DebuggableEventStream` and `DebugDispatcher`.  The loop now reads
/// directly from the central event channel and forwards to the new
/// per-system service cores via `service_cores.observe`.  Pausing is
/// the new observability system's job (`ObservabilityController` per-
/// core gates); the legacy `DebugController` stays only to feed the
/// SSE snapshot until §37h replaces the dashboard wiring.
pub async fn run(
    debug_ctrl: DebugController,
    debug_owned: windlass_debug::DebugOwnedPart,
) -> Result<()> {
    let ShellRuntime {
        mut event_rx,
        mut history,
        mut cmd_rx,
        mut log_rx,
        mut exchange_rx,
        service_cores,
        ..
    } = init_shell(&debug_ctrl, debug_owned).await?;

    'main: loop {
        drain_channels(
            &mut history,
            &debug_ctrl,
            &mut cmd_rx,
            &mut log_rx,
            &mut exchange_rx,
        );

        let event = match event_rx.recv().await {
            None => break 'main,
            Some(e) => e,
        };

        debug!(?event, "←");

        service_cores.observe(&event);
    }

    Ok(())
}

fn drain_channels(
    history: &mut DebugHistory,
    debug_ctrl: &DebugController,
    cmd_rx: &mut mpsc::Receiver<windlass_debug::DebugCommand>,
    log_rx: &mut mpsc::Receiver<windlass_debug::LogEntry>,
    exchange_rx: &mut mpsc::Receiver<(uuid::Uuid, windlass_types::HttpExchange)>,
) {
    while let Ok(cmd) = cmd_rx.try_recv() {
        history.apply_cmd(cmd);
        debug_ctrl.publish(history);
    }
    while let Ok(log) = log_rx.try_recv() {
        history.append_log(log);
        debug_ctrl.publish(history);
    }
    while let Ok((action_id, exchange)) = exchange_rx.try_recv() {
        history.action_http_exchange(action_id, exchange);
        debug_ctrl.publish(history);
    }
}
