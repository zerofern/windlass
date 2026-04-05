mod config;
mod docker;
mod gotify;
mod mam;
mod monitors;
mod qbit;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info};
use uom::si::information::byte;

use crate::core::{actions::Action, events::Event, process_event, types::SystemState};
use crate::types::{AlertPriority, AuthCookie, VpnIp, VpnPort, WakeupId};

pub use config::Config;

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// fires the Init event, then runs the event loop forever.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;

    let docker = docker::DockerClient::connect(
        config.dump_dir.clone(),
        config.vpn_ip_file.clone(),
        config.vpn_port_file.clone(),
    )?;

    let is_gluetun_healthy = docker.is_gluetun_healthy().await;
    let dependents = docker.discover_dependents().await;
    let boot_port_files = docker.read_boot_port_files().await;

    info!(
        gluetun_healthy = is_gluetun_healthy,
        dependents = ?dependents,
        port_files_ok = boot_port_files.is_ok(),
        "Windlass started"
    );

    let (tx, mut rx) = mpsc::channel::<Event>(128);

    docker.spawn_event_watcher(tx.clone());
    docker.spawn_file_watcher(tx.clone());

    let direct = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let vpn = {
        let builder = reqwest::Client::builder().timeout(Duration::from_secs(30));
        let builder = if let Some(ref proxy_url) = config.gluetun_proxy_url {
            builder.proxy(reqwest::Proxy::all(proxy_url)?)
        } else {
            builder
        };
        builder.build()?
    };

    let mam_session: Arc<Mutex<String>> = Arc::new(Mutex::new(config.mam_session.clone()));
    let mut wakeups: HashMap<WakeupId, JoinHandle<()>> = HashMap::new();
    let mut cached_cookie: Option<AuthCookie> = None;
    let mut state = SystemState::initial();

    let (new_state, actions) = process_event(
        state,
        Event::Init { is_gluetun_healthy, port_files: boot_port_files },
    );
    state = new_state;
    ShellContext::new(&config, &docker, &direct, &vpn, &mut wakeups, &mam_session, &dependents, &mut cached_cookie, &tx)
        .dispatch(actions);

    while let Some(event) = rx.recv().await {
        debug!(?event, "←");
        let (new_state, actions) = process_event(state, event);
        state = new_state;
        ShellContext::new(&config, &docker, &direct, &vpn, &mut wakeups, &mam_session, &dependents, &mut cached_cookie, &tx)
            .dispatch(actions);
    }

    Ok(())
}

/// All shared shell state bundled together so action handlers don't need
/// a long argument list.
struct ShellContext<'a> {
    config: &'a Config,
    docker: &'a docker::DockerClient,
    direct: &'a reqwest::Client,
    vpn: &'a reqwest::Client,
    wakeups: &'a mut HashMap<WakeupId, JoinHandle<()>>,
    mam_session: &'a Arc<Mutex<String>>,
    dependents: &'a [String],
    cached_cookie: &'a mut Option<AuthCookie>,
    tx: &'a mpsc::Sender<Event>,
}

impl<'a> ShellContext<'a> {
    #[allow(clippy::too_many_arguments)]
    // Constructor for a struct containing mutable references — const fn is not applicable here
    // despite what clippy suggests, since &mut references cannot be used in const context.
    fn new(
        config: &'a Config,
        docker: &'a docker::DockerClient,
        direct: &'a reqwest::Client,
        vpn: &'a reqwest::Client,
        wakeups: &'a mut HashMap<WakeupId, JoinHandle<()>>,
        mam_session: &'a Arc<Mutex<String>>,
        dependents: &'a [String],
        cached_cookie: &'a mut Option<AuthCookie>,
        tx: &'a mpsc::Sender<Event>,
    ) -> Self {
        Self { config, docker, direct, vpn, wakeups, mam_session, dependents, cached_cookie, tx }
    }

    /// Dispatches every action produced by the Core in one synchronous pass.
    fn dispatch(&mut self, actions: Vec<Action>) {
        for action in actions {
            debug!(?action, "→");
            match action {
                Action::ScheduleWakeup(id, duration)  => self.schedule_wakeup(id, duration),
                Action::ReadPortFiles                  => self.read_port_files(),
                Action::FetchAndDumpAllLogs            => self.fetch_and_dump_all_logs(),
                Action::StopDependentContainers        => self.stop_dependent_containers(),
                Action::StartDependentContainers       => self.start_dependent_containers(),
                Action::RestartGluetun                 => self.restart_gluetun(),
                Action::AuthenticateQbit               => self.authenticate_qbit(),
                Action::SyncQbitPort(cookie, port)     => self.sync_qbit_port(cookie, port),
                Action::UpdateMam(ip)                  => self.update_mam(ip),
                Action::CheckMamConnectability         => self.check_mam_connectability(),
                Action::CheckDiskSpace                 => self.check_disk_space(),
                Action::CheckNewTorrents               => self.check_new_torrents(),
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
        let ip_file = self.config.vpn_ip_file.clone();
        let port_file = self.config.vpn_port_file.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                docker::read_port_files(&ip_file, &port_file)
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
        let client = self.direct.clone();
        let url = self.config.qbit_url.clone();
        let user = self.config.qbit_user.clone();
        let pass = self.config.qbit_pass.0.expose_secret().to_owned();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let event = qbit::authenticate(&client, &url, &user, &pass).await;
            let _ = tx.send(event).await;
        });
    }

    fn sync_qbit_port(&mut self, cookie: AuthCookie, port: VpnPort) {
        *self.cached_cookie = Some(cookie.clone());
        let client = self.direct.clone();
        let url = self.config.qbit_url.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let event = qbit::sync_port(&client, &url, &cookie, port).await;
            let _ = tx.send(event).await;
        });
    }

    // ── MAM ───────────────────────────────────────────────────────────────────

    fn update_mam(&self, _ip: VpnIp) {
        let client = self.vpn.clone();
        let session = self.mam_session.clone();
        let url = self.config.mam_seedbox_url.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let current = session.lock().unwrap().clone();
            let (event, new_session) = mam::update_seedbox_at(&client, &current, &url).await;
            if let Some(rotated) = new_session {
                *session.lock().unwrap() = rotated;
            }
            let _ = tx.send(event).await;
        });
    }

    fn check_mam_connectability(&self) {
        let client = self.vpn.clone();
        let session = self.mam_session.clone();
        let url = self.config.mam_load_url.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let current = session.lock().unwrap().clone();
            let (event, new_session) =
                mam::check_connectability_at(&client, &current, &url).await;
            if let Some(rotated) = new_session {
                *session.lock().unwrap() = rotated;
            }
            let _ = tx.send(event).await;
        });
    }

    // ── Monitoring ────────────────────────────────────────────────────────────

    fn check_disk_space(&self) {
        let path = self.config.data_path.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let space = tokio::task::spawn_blocking(move || monitors::check_disk_space(&path))
                .await
                .unwrap_or_else(|_| uom::si::f64::Information::new::<byte>(f64::MAX));
            let _ = tx.send(Event::DiskSpaceObserved(space)).await;
        });
    }

    fn check_new_torrents(&self) {
        let tx = self.tx.clone();
        let Some(cookie) = self.cached_cookie.clone() else {
            // qBit not yet authenticated — send empty so Core re-arms the timer.
            tokio::spawn(async move {
                let _ = tx.send(Event::NewTorrentsObserved(vec![])).await;
            });
            return;
        };
        let client = self.direct.clone();
        let url = self.config.qbit_url.clone();
        tokio::spawn(async move {
            // Shell sends the raw full list — Core owns the deduplication logic.
            let current = qbit::list_torrents(&client, &url, &cookie).await;
            let _ = tx.send(Event::NewTorrentsObserved(current)).await;
        });
    }

    // ── Alerts ────────────────────────────────────────────────────────────────

    fn send_gotify_alert(&self, priority: AlertPriority, message: String) {
        let client = self.direct.clone();
        let url = self.config.gotify_url.clone();
        let token = self.config.gotify_token.clone();
        tokio::spawn(async move {
            gotify::send_alert(&client, &url, &token, priority, &message).await;
        });
    }
}
