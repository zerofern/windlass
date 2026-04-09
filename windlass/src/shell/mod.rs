mod actions;
mod config;

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use windlass_clients::{gotify, mam, qbit};
use windlass_core::{actions::Action, events::Event, types::SystemState};
use windlass_debug::{CausalTx, DebugController, DebugDispatcher, DebugHistory, DebuggableEventStream};
use windlass_local::{docker, vpn_files};
use windlass_types::WakeupId;

pub use config::Config;

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// then runs the event loop forever.
/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// then runs the event loop forever.
///
/// `debug_ctrl` and `debug_owned` are created in `main` so the log layer
/// can be registered on the tracing subscriber before the shell starts.
pub async fn run(debug_ctrl: DebugController, debug_owned: windlass_debug::DebugOwnedPart) -> Result<()> {
    let config = Config::from_env()?;

    let (tx, rx) = mpsc::channel::<Event>(128);

    let (docker, boot) = docker::DockerClient::boot(config.dump_dir.clone(), tx.clone()).await?;
    let port_files =
        vpn_files::read_and_watch(&config.vpn_ip_file, &config.vpn_port_file, tx.clone()).await;

    info!("Windlass started");

    let on_http = debug_ctrl.make_http_observer();

    let direct = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let qbit = qbit::QbitClient::new(
        direct.clone(),
        config.qbit_url.clone(),
        config.qbit_user.clone(),
        config.qbit_pass.0.expose_secret().to_owned(),
        on_http.clone(),
    );
    let mam = mam::MamClient::new(
        config.gluetun_proxy_url.as_deref(),
        config.mam_session.clone(),
        config.mam_seedbox_url.clone(),
        config.mam_load_url.clone(),
        &config.mam_user_agent,
        on_http.clone(),
    )?;
    let gotify = gotify::GotifyClient::new(
        direct.clone(),
        config.gotify_url.clone(),
        config.gotify_token.clone(),
        on_http,
    );

    let vpn_ip_file = config.vpn_ip_file.clone();
    let vpn_port_file = config.vpn_port_file.clone();
    let data_path = config.data_path.clone();

    let (obs_tx, _) = tokio::sync::broadcast::channel::<windlass_core::Observation>(256);

    let mut debug_stream = DebuggableEventStream::new(
        rx,
        debug_owned.internal_rx,
        debug_ctrl.clone(),
        obs_tx.clone(),
    );

    let app_state = windlass_web::AppState {
        event_tx: tx.clone(),
        debug_ctrl: debug_ctrl.clone(),
        observations: obs_tx.clone(),
        chaos_url: std::env::var("CHAOS_URL").ok(),
    };
    let bind_addr = std::env::var("WINDLASS_BIND").unwrap_or_else(|_| "0.0.0.0:5010".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!(addr = %bind_addr, "HTTP server listening");
    tokio::spawn(async move {
        axum::serve(listener, windlass_web::router(app_state))
            .await
            .expect("HTTP server crashed");
    });

    let mut wakeups: HashMap<WakeupId, JoinHandle<()>> = HashMap::new();
    let mut state = SystemState::initial();
    let mut history = DebugHistory::new(SystemState::initial());
    let mut cmd_rx = debug_owned.cmd_rx;
    let mut log_rx = debug_owned.log_rx;
    let mut queue_rx = debug_owned.queue_rx;
    let mut exchange_rx = debug_owned.exchange_rx;

    // Causation channel: action handlers send (event, action_id) here in debug mode
    // so the loop can record caused_by_action on the resulting event.
    let (causal_debug_tx, mut causal_rx) = mpsc::channel::<(Event, uuid::Uuid)>(128);

    if let Err(e) = mam.check_session().await {
        warn!("MAM session check failed at startup: {e} — continuing anyway");
    }

    // Send Init into the channel so it flows through DebuggableEventStream.
    tx.send(Event::Init {
        at: chrono::Utc::now(),
        is_gluetun_healthy: boot.is_gluetun_healthy,
        port_files,
    })
    .await
    .expect("event channel open at startup");

    let debug_dispatcher = DebugDispatcher::new(debug_ctrl.clone());

    'main: loop {
        // ── Drain pending channels ────────────────────────────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            history.apply_cmd(cmd);
            debug_ctrl.publish(&history);
        }
        while let Ok(log) = log_rx.try_recv() {
            history.append_log(log);
            debug_ctrl.publish(&history);
        }
        while let Ok((action_id, exchange)) = exchange_rx.try_recv() {
            history.action_http_exchange(action_id, exchange);
            debug_ctrl.publish(&history);
        }

        // ── Obtain next event ─────────────────────────────────────────────────
        let (event, event_id) = if debug_ctrl.is_debug_mode() {
            // Debug mode: drain incoming StoredEvents into the queue, then pop
            // the front event (pausing for step if needed).
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
            // Non-debug mode: receive directly, pause only on breakpoints.
            match debug_stream.recv().await {
                None => break 'main,
                Some(e) => (e, None),
            }
        };

        debug!(?event, "←");

        let outcome = state.process_event(event, chrono::Utc::now());
        if outcome.state_changed {
            let _ = obs_tx.send(windlass_core::Observation::StateSnapshot(state.clone()));
        }

        let mut ctx = ShellContext {
            docker: &docker,
            qbit: &qbit,
            mam: &mam,
            gotify: &gotify,
            wakeups: &mut wakeups,
            dependents: &boot.dependents,
            tx: &tx,
            vpn_ip_file: &vpn_ip_file,
            vpn_port_file: &vpn_port_file,
            data_path: &data_path,
        };

        if let Some(eid) = event_id {
            let plain_tx = tx.clone();
            debug_dispatcher
                .dispatch(outcome.actions, |action| {
                    let action_id = history.action_started(&action, eid);
                    let causal = CausalTx::debug(action_id, causal_debug_tx.clone());
                    ctx.execute(action, causal);
                })
                .await;
            history.event_completed(eid, state.clone());
            debug_ctrl.publish(&history);
            drop(plain_tx);
        } else {
            let plain_tx = tx.clone();
            debug_dispatcher
                .dispatch(outcome.actions, |action| {
                    let causal = CausalTx::plain(uuid::Uuid::new_v4(), plain_tx.clone());
                    ctx.execute(action, causal);
                })
                .await;
        }
    }

    Ok(())
}

/// Waits for the next event in debug mode by draining the queue channel and
/// history, pausing on the front event for a step permit.
///
/// Returns `None` if all input channels have closed (shutdown). Otherwise
/// returns `(Event, Some(event_id))` after recording the event as started.
async fn dequeue_debug(
    history: &mut DebugHistory,
    queue_rx: &mut tokio::sync::mpsc::Receiver<windlass_debug::StoredEvent>,
    causal_rx: &mut tokio::sync::mpsc::Receiver<(windlass_core::events::Event, uuid::Uuid)>,
    cmd_rx: &mut tokio::sync::mpsc::Receiver<windlass_debug::DebugCommand>,
    log_rx: &mut tokio::sync::mpsc::Receiver<windlass_debug::LogEntry>,
    state: &SystemState,
    debug_ctrl: &DebugController,
) -> Option<(windlass_core::events::Event, Option<uuid::Uuid>)> {
    loop {
        // Drain all pending channels before checking the queue.
        while let Ok(stored) = queue_rx.try_recv() {
            history.push_stored_event(stored);
            debug_ctrl.publish(history);
        }
        while let Ok((event, action_id)) = causal_rx.try_recv() {
            let event_id = history.push_causal_event(event, action_id);
            history.action_completed(action_id, Some(event_id));
            debug_ctrl.publish(history);
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            history.apply_cmd(cmd);
            debug_ctrl.publish(history);
        }
        while let Ok(log) = log_rx.try_recv() {
            history.append_log(log);
            debug_ctrl.publish(history);
        }

        if history.queue_is_empty() {
            // Nothing to process — wait for an event, command, or log.
            tokio::select! {
                stored = queue_rx.recv() => match stored {
                    Some(s) => { history.push_stored_event(s); debug_ctrl.publish(history); }
                    None => return None,
                },
                causal = causal_rx.recv() => {
                    if let Some((event, action_id)) = causal {
                        let event_id = history.push_causal_event(event, action_id);
                        history.action_completed(action_id, Some(event_id));
                        debug_ctrl.publish(history);
                    }
                },
                cmd = cmd_rx.recv() => match cmd {
                    Some(c) => { history.apply_cmd(c); debug_ctrl.publish(history); }
                    None => return None,
                },
                log = log_rx.recv() => {
                    if let Some(l) = log { history.append_log(l); debug_ctrl.publish(history); }
                },
            }
            continue;
        }

        // Pause on the front event before processing it.
        let front_variant = history.queue_front_variant().unwrap();
        if debug_ctrl.should_pause_on_event(front_variant) {
            debug_ctrl.set_paused_on(Some(windlass_debug::PausedOn::Event {
                variant: front_variant,
            }));
            debug_ctrl.publish(history);

            let execute = loop {
                tokio::select! {
                    execute = debug_ctrl.acquire_step() => break execute,
                    stored = queue_rx.recv() => match stored {
                        Some(s) => { history.push_stored_event(s); debug_ctrl.publish(history); }
                        None => { debug_ctrl.set_paused_on(None); return None; }
                    },
                    causal = causal_rx.recv() => {
                        if let Some((event, action_id)) = causal {
                            let event_id = history.push_causal_event(event, action_id);
                            history.action_completed(action_id, Some(event_id));
                            debug_ctrl.publish(history);
                        }
                    },
                    cmd = cmd_rx.recv() => match cmd {
                        Some(c) => { history.apply_cmd(c); debug_ctrl.publish(history); }
                        None => { debug_ctrl.set_paused_on(None); return None; }
                    },
                    log = log_rx.recv() => {
                        if let Some(l) = log { history.append_log(l); debug_ctrl.publish(history); }
                    },
                }
            };

            debug_ctrl.set_paused_on(None);

            if !execute {
                // Skip: pop the front event without processing.
                history.pop_queue_front();
                debug_ctrl.publish(history);
                continue;
            }

            // Re-check: the queue may have changed while we waited.
            if history.queue_is_empty() {
                continue;
            }
        }

        // Pop the front event and record it as started.
        let stored = history.pop_queue_front().unwrap();
        let id = stored.id;
        let event = stored.event().clone();
        history.event_started_stored(stored, state.clone());
        debug_ctrl.publish(history);

        return Some((event, Some(id)));
    }
}

/// All shared shell state bundled together so action handlers don't need
/// a long argument list.
struct ShellContext<'a> {
    docker: &'a docker::DockerClient,
    qbit: &'a qbit::QbitClient,
    mam: &'a mam::MamClient,
    gotify: &'a gotify::GotifyClient,
    wakeups: &'a mut HashMap<WakeupId, JoinHandle<()>>,
    dependents: &'a [String],
    tx: &'a mpsc::Sender<Event>,
    vpn_ip_file: &'a str,
    vpn_port_file: &'a str,
    data_path: &'a str,
}

impl ShellContext<'_> {
    /// Executes a single action produced by the Core.
    fn execute(&mut self, action: Action, causal_tx: CausalTx) {
        match action {
            Action::ScheduleWakeup(id, duration) => self.schedule_wakeup(id, duration),
            Action::ReadPortFiles => self.read_port_files(causal_tx),
            Action::FetchAndDumpAllLogs => self.fetch_and_dump_all_logs(causal_tx),
            Action::StopDependentContainers => self.stop_dependent_containers(),
            Action::StartDependentContainers => self.start_dependent_containers(),
            Action::RestartGluetun => self.restart_gluetun(),
            Action::AuthenticateQbit => self.authenticate_qbit(causal_tx),
            Action::SyncQbitPort(cookie, port) => self.sync_qbit_port(cookie, port, causal_tx),
            Action::UpdateMam(ip) => self.update_mam(ip, causal_tx),
            Action::CheckMamConnectability => self.check_mam_connectability(causal_tx),
            Action::CheckDiskSpace => self.check_disk_space(causal_tx),
            Action::CheckNewTorrents(cookie) => self.check_new_torrents(cookie, causal_tx),
            Action::SendGotifyAlert(priority, msg) => self.send_gotify_alert(priority, msg),
        }
    }
}
