mod config;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uom::si::information::byte;

use windlass_clients::{gotify, mam, qbit};
use windlass_core::{actions::Action, events::Event, types::SystemState};
use windlass_debug::DebugController;
use windlass_local::{docker, monitors, vpn_files};
use windlass_types::{AlertPriority, AuthCookie, VpnIp, VpnPort, WakeupId};

pub use config::Config;

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// fires the Init event, then runs the event loop forever.
#[allow(clippy::too_many_lines)]
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;

    let (tx, mut rx) = mpsc::channel::<Event>(128);

    let (docker, boot) = docker::DockerClient::boot(config.dump_dir.clone(), tx.clone()).await?;
    let port_files =
        vpn_files::read_and_watch(&config.vpn_ip_file, &config.vpn_port_file, tx.clone()).await;

    info!("Windlass started");

    let debug_ctrl = DebugController::new();

    if std::env::var("DEBUG_MODE_ON_START").is_ok_and(|v| v == "true") {
        // obs_tx isn't built yet — enable after the broadcast channel is created below.
        info!("DEBUG_MODE_ON_START=true — debug mode will be enabled at startup");
    }

    let direct = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let qbit = qbit::QbitClient::new(
        direct.clone(),
        config.qbit_url.clone(),
        config.qbit_user.clone(),
        config.qbit_pass.0.expose_secret().to_owned(),
        debug_ctrl.clone(),
    );
    let mam = mam::MamClient::new(
        config.gluetun_proxy_url.as_deref(),
        config.mam_session.clone(),
        config.mam_seedbox_url.clone(),
        config.mam_load_url.clone(),
        &config.mam_user_agent,
        debug_ctrl.clone(),
    )?;
    let gotify = gotify::GotifyClient::new(
        direct.clone(),
        config.gotify_url.clone(),
        config.gotify_token.clone(),
        debug_ctrl.clone(),
    );

    let vpn_ip_file = config.vpn_ip_file.clone();
    let vpn_port_file = config.vpn_port_file.clone();
    let data_path = config.data_path.clone();

    let shared_state = Arc::new(tokio::sync::RwLock::new(SystemState::initial()));
    let (obs_tx, _) = tokio::sync::broadcast::channel::<windlass_core::Observation>(256);

    if std::env::var("DEBUG_MODE_ON_START").is_ok_and(|v| v == "true") {
        debug_ctrl.enable_debug(obs_tx.clone());
        info!("Debug mode enabled");
    }
    let app_state = windlass_web::AppState {
        event_tx: tx.clone(),
        state: shared_state.clone(),
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

    if let Err(e) = mam.check_session().await {
        warn!("MAM session check failed at startup: {e} — continuing anyway");
    }

    let actions = state.process_event(Event::Init {
        is_gluetun_healthy: boot.is_gluetun_healthy,
        port_files,
    });
    *shared_state.write().await = state.clone();
    let _ = obs_tx.send(windlass_core::Observation::StateSnapshot(state.clone()));
    ShellContext {
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
        debug_ctrl: &debug_ctrl,
        obs_tx: &obs_tx,
    }
    .dispatch(actions)
    .await;

    while let Some(event) = rx.recv().await {
        debug!(?event, "←");

        if debug_ctrl.is_frozen() {
            debug!(?event, "system frozen: dropping event");
            continue;
        }

        if matches!(event, Event::MamRateLimitViolation) {
            error!("MAM rate limit guard triggered — system frozen. Restart Windlass to resume.");
            let g = gotify.clone();
            tokio::spawn(async move {
                g.send_alert(
                    windlass_types::AlertPriority::Critical,
                    "🛑 MAM rate limit guard triggered — requests were too fast. System is frozen. Restart Windlass to resume.",
                ).await;
            });
            continue;
        }

        // Pause if debug mode is on or the event variant has a breakpoint.
        // The event is pushed to the queue (visible via GET /debug) then we wait
        // for a step permit from POST /debug/step/event before continuing.
        let event = if debug_ctrl.should_pause_on_event(event_variant(&event)) {
            debug_ctrl.enqueue_event(event.clone());
            let _ = obs_tx.send(windlass_core::Observation::EventReceived(event));
            debug_ctrl.acquire_event_step().await;
            match debug_ctrl.dequeue_event() {
                Some(e) => e,
                // Disabled while paused — discard and fetch the next event.
                None => continue,
            }
        } else {
            event
        };

        let _ = obs_tx.send(windlass_core::Observation::EventReceived(event.clone()));

        let actions = state.process_event(event);
        *shared_state.write().await = state.clone();
        let _ = obs_tx.send(windlass_core::Observation::StateSnapshot(state.clone()));
        ShellContext {
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
            debug_ctrl: &debug_ctrl,
            obs_tx: &obs_tx,
        }
        .dispatch(actions)
        .await;
    }

    Ok(())
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
    debug_ctrl: &'a DebugController,
    obs_tx: &'a tokio::sync::broadcast::Sender<windlass_core::Observation>,
}

impl ShellContext<'_> {
    /// Dispatches every action produced by the Core.
    /// In debug mode (or when an action variant has a breakpoint) the action is
    /// queued and waits for a step permit from `POST /debug/step/action`.
    async fn dispatch(&mut self, actions: Vec<Action>) {
        for action in actions {
            debug!(?action, "→");

            // Pause if debug mode is on or this action variant has a breakpoint.
            let action = if self
                .debug_ctrl
                .should_pause_on_action(action_variant(&action))
            {
                self.debug_ctrl.enqueue_action(action.clone());
                let _ = self
                    .obs_tx
                    .send(windlass_core::Observation::ActionDispatched(action));
                self.debug_ctrl.acquire_action_step().await;
                match self.debug_ctrl.dequeue_action() {
                    Some(a) => a,
                    // Debug disabled while paused — skip this action.
                    None => continue,
                }
            } else {
                action
            };

            let _ = self
                .obs_tx
                .send(windlass_core::Observation::ActionDispatched(action.clone()));

            match action {
                Action::ScheduleWakeup(id, duration) => self.schedule_wakeup(id, duration),
                Action::ReadPortFiles => self.read_port_files(),
                Action::FetchAndDumpAllLogs => self.fetch_and_dump_all_logs(),
                Action::StopDependentContainers => self.stop_dependent_containers(),
                Action::StartDependentContainers => self.start_dependent_containers(),
                Action::RestartGluetun => self.restart_gluetun(),
                Action::AuthenticateQbit => self.authenticate_qbit(),
                Action::SyncQbitPort(cookie, port) => self.sync_qbit_port(cookie, port),
                Action::UpdateMam(ip) => self.update_mam(ip),
                Action::CheckMamConnectability => self.check_mam_connectability(),
                Action::CheckDiskSpace => self.check_disk_space(),
                Action::CheckNewTorrents(cookie) => self.check_new_torrents(cookie),
                Action::SendGotifyAlert(priority, msg) => self.send_gotify_alert(priority, msg),
            }
        }
    }

    // ── Timers ────────────────────────────────────────────────────────────────

    fn schedule_wakeup(&mut self, id: WakeupId, duration: Duration) {
        // Cancel any existing timer for this id to prevent duplicate wakeup loops.
        if let Some(handle) = self.wakeups.remove(&id) {
            handle.abort();
        }
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let _ = tx.send(Event::Wakeup(id)).await;
        });
        self.wakeups.insert(id, handle);
    }

    // ── Port files ────────────────────────────────────────────────────────────

    /// Retry path only — the debounced file watcher handles normal reads.
    fn read_port_files(&self) {
        let ip_file = self.vpn_ip_file.to_owned();
        let port_file = self.vpn_port_file.to_owned();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                vpn_files::read_port_files(&ip_file, &port_file)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(Event::PortFileReadResult(result)).await;
        });
    }

    // ── Docker ────────────────────────────────────────────────────────────────

    fn fetch_and_dump_all_logs(&self) {
        let docker = self.docker.clone();
        let deps = self.dependents.to_vec();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            docker.fetch_and_dump_logs(&deps).await;
            let _ = tx.send(Event::LogsDumped).await;
        });
    }

    fn stop_dependent_containers(&self) {
        let docker = self.docker.clone();
        let deps = self.dependents.to_vec();
        tokio::spawn(async move {
            docker.stop_dependents(&deps).await;
        });
    }

    fn start_dependent_containers(&self) {
        let docker = self.docker.clone();
        let deps = self.dependents.to_vec();
        tokio::spawn(async move {
            docker.start_dependents(&deps).await;
        });
    }

    fn restart_gluetun(&self) {
        let docker = self.docker.clone();
        tokio::spawn(async move {
            docker.restart_gluetun().await;
        });
    }

    // ── qBittorrent ───────────────────────────────────────────────────────────

    fn authenticate_qbit(&self) {
        let qbit = self.qbit.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let event = qbit.authenticate().await;
            let _ = tx.send(event).await;
        });
    }

    fn sync_qbit_port(&self, cookie: AuthCookie, port: VpnPort) {
        let qbit = self.qbit.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let event = qbit.sync_port(&cookie, port).await;
            let _ = tx.send(event).await;
        });
    }

    // ── MAM ───────────────────────────────────────────────────────────────────

    fn update_mam(&self, _ip: VpnIp) {
        let mam = self.mam.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let event = mam.update_seedbox().await;
            let _ = tx.send(event).await;
        });
    }

    fn check_mam_connectability(&self) {
        let mam = self.mam.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let event = mam.check_connectability().await;
            let _ = tx.send(event).await;
        });
    }

    // ── Monitoring ────────────────────────────────────────────────────────────

    fn check_disk_space(&self) {
        let path = self.data_path.to_owned();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let space = tokio::task::spawn_blocking(move || monitors::check_disk_space(&path))
                .await
                .unwrap_or_else(|_| uom::si::f64::Information::new::<byte>(f64::MAX));
            let _ = tx.send(Event::DiskSpaceObserved(space)).await;
        });
    }

    fn check_new_torrents(&self, cookie: AuthCookie) {
        let tx = self.tx.clone();
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            // Shell sends the raw full list — Core owns the deduplication logic.
            let current = qbit.list_torrents(&cookie).await;
            let _ = tx.send(Event::NewTorrentsObserved(current)).await;
        });
    }

    // ── Alerts ────────────────────────────────────────────────────────────────

    fn send_gotify_alert(&self, priority: AlertPriority, message: String) {
        let gotify = self.gotify.clone();
        tokio::spawn(async move {
            gotify.send_alert(priority, &message).await;
        });
    }
}

// ── Variant name helpers ──────────────────────────────────────────────────────

/// Returns the variant name of an [`Event`] as a static string.
/// Used to look up breakpoints without heap allocation.
const fn event_variant(event: &Event) -> &'static str {
    match event {
        Event::Init { .. } => "Init",
        Event::ManualReset => "ManualReset",
        Event::DockerGluetunDied => "DockerGluetunDied",
        Event::DockerGluetunHealthy => "DockerGluetunHealthy",
        Event::PortFileReadResult(_) => "PortFileReadResult",
        Event::QbitAuthSuccess(_) => "QbitAuthSuccess",
        Event::QbitAuthFailed => "QbitAuthFailed",
        Event::QbitConnectionRefused => "QbitConnectionRefused",
        Event::QbitApiError(_) => "QbitApiError",
        Event::QbitPortSyncSuccess => "QbitPortSyncSuccess",
        Event::QbitPortSyncFailed(_) => "QbitPortSyncFailed",
        Event::MamUpdateSuccess => "MamUpdateSuccess",
        Event::MamAsnMismatch(_) => "MamAsnMismatch",
        Event::MamStatusObserved(_) => "MamStatusObserved",
        Event::DiskSpaceObserved(_) => "DiskSpaceObserved",
        Event::NewTorrentsObserved(_) => "NewTorrentsObserved",
        Event::LogsDumped => "LogsDumped",
        Event::Wakeup(_) => "Wakeup",
        Event::MamRateLimitViolation => "MamRateLimitViolation",
    }
}

/// Returns the variant name of an [`Action`] as a static string.
const fn action_variant(action: &Action) -> &'static str {
    match action {
        Action::ScheduleWakeup(_, _) => "ScheduleWakeup",
        Action::ReadPortFiles => "ReadPortFiles",
        Action::FetchAndDumpAllLogs => "FetchAndDumpAllLogs",
        Action::StopDependentContainers => "StopDependentContainers",
        Action::StartDependentContainers => "StartDependentContainers",
        Action::RestartGluetun => "RestartGluetun",
        Action::AuthenticateQbit => "AuthenticateQbit",
        Action::SyncQbitPort(_, _) => "SyncQbitPort",
        Action::UpdateMam(_) => "UpdateMam",
        Action::CheckMamConnectability => "CheckMamConnectability",
        Action::CheckDiskSpace => "CheckDiskSpace",
        Action::CheckNewTorrents(_) => "CheckNewTorrents",
        Action::SendGotifyAlert(_, _) => "SendGotifyAlert",
    }
}
