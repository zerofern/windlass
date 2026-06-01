mod config;
mod db_shell;
mod dequeue;
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

use dequeue::dequeue_debug;
use init::{ShellRuntime, init_shell};

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// then runs the event loop forever.
///
/// §36 step 8 (2026-06-01): the legacy `SystemState::process_event`
/// shadow and its action dispatcher are gone.  The shell loop now does
/// one thing: forward every incoming event to the bridge
/// (`service_cores.observe`), which routes it to the relevant new
/// core (Vpn/Qbit/Mam/Disk/Docker) for the real decision.  All I/O
/// originates from the per-core shells via their own event channels —
/// the legacy `Event` type stays only as the bridge protocol the
/// I/O sites already use.  Step 9 will remove `windlass-core`
/// entirely once the SSE/UI is migrated off the legacy `SystemState`
/// shape.
///
/// `debug_ctrl` and `debug_owned` are created in `main` so the log layer
/// can be registered on the tracing subscriber before the shell starts.
pub async fn run(
    debug_ctrl: DebugController,
    debug_owned: windlass_debug::DebugOwnedPart,
) -> Result<()> {
    let ShellRuntime {
        mut debug_stream,
        state,
        mut history,
        mut cmd_rx,
        mut log_rx,
        mut queue_rx,
        mut exchange_rx,
        mut causal_rx,
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

        let (event, _event_id) = if debug_ctrl.is_debug_mode() {
            match dequeue_debug(
                &mut history,
                &mut queue_rx,
                &mut causal_rx,
                &mut cmd_rx,
                &mut log_rx,
                &state,
                &debug_ctrl,
            )
            .await
            {
                None => break 'main,
                Some(v) => v,
            }
        } else {
            match debug_stream.recv().await {
                None => break 'main,
                Some(e) => (e, None),
            }
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
