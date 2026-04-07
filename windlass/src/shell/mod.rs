mod actions;
mod config;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use windlass_clients::{gotify, mam, qbit};
use windlass_core::{actions::Action, events::Event, types::SystemState};
use windlass_debug::{DebugController, DebuggableEventStream, PausedOn, action_variant};
use windlass_local::{docker, vpn_files};
use windlass_types::WakeupId;

use arc_swap::ArcSwap;

pub use config::Config;

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// then runs the event loop forever.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;

    let (tx, rx) = mpsc::channel::<Event>(128);

    let (docker, boot) = docker::DockerClient::boot(config.dump_dir.clone(), tx.clone()).await?;
    let port_files =
        vpn_files::read_and_watch(&config.vpn_ip_file, &config.vpn_port_file, tx.clone()).await;

    info!("Windlass started");

    let debug_ctrl = DebugController::new();
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

    let shared_state = Arc::new(ArcSwap::from_pointee(SystemState::initial()));
    let (obs_tx, _) = tokio::sync::broadcast::channel::<windlass_core::Observation>(256);

    let mut debug_stream = DebuggableEventStream::new(rx, debug_ctrl.clone(), obs_tx.clone());

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

    // Send Init into the channel so it flows through DebuggableEventStream.
    // If DEBUG_MODE_ON_START=true, the stream will pause on it before
    // the main loop receives it.
    tx.send(Event::Init {
        is_gluetun_healthy: boot.is_gluetun_healthy,
        port_files,
    })
    .await
    .expect("event channel open at startup");

    while let Some(event) = debug_stream.recv().await {
        debug!(?event, "←");

        let _ = obs_tx.send(windlass_core::Observation::EventReceived(event.clone()));

        let actions = state.process_event(event);
        shared_state.store(Arc::new(state.clone()));
        let _ = obs_tx.send(windlass_core::Observation::StateSnapshot(state.clone()));
        DebuggableShell(ShellContext {
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
        })
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
    /// Executes a single action produced by the Core.
    fn execute(&mut self, action: Action) {
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

/// Wraps [`ShellContext`] with debug-mode pause/skip logic for action dispatch.
///
/// In debug mode (or when an action breakpoint is set), each action pauses
/// before execution and waits for `POST /debug/step`. A `POST /debug/skip`
/// discards the action instead.
struct DebuggableShell<'a>(ShellContext<'a>);

impl DebuggableShell<'_> {
    async fn dispatch(&mut self, actions: Vec<Action>) {
        let total = actions.len();
        self.0.debug_ctrl.set_pending_actions(&actions);

        for (idx, action) in actions.into_iter().enumerate() {
            debug!(?action, "→");

            let _ = self
                .0
                .obs_tx
                .send(windlass_core::Observation::ActionDispatched(action.clone()));

            let variant = action_variant(&action);
            if self.0.debug_ctrl.should_pause_on_action(variant) {
                self.0.debug_ctrl.set_paused_on(Some(PausedOn::Action {
                    variant,
                    index: idx + 1,
                    of: total,
                }));
                let execute = self.0.debug_ctrl.acquire_step().await;
                self.0.debug_ctrl.set_paused_on(None);
                if !execute {
                    continue; // skipped
                }
            }

            self.0.execute(action);
        }

        self.0.debug_ctrl.clear_pending_actions();
    }
}
