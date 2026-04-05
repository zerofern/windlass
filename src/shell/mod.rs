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
use crate::types::{AuthCookie, WakeupId};

pub use config::Config;

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// fires the Init event, then runs the event loop forever.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;

    let docker = docker::connect()?;
    let is_gluetun_healthy = docker::is_gluetun_healthy(&docker).await;
    let dependents = docker::discover_dependents(&docker).await;

    // Derive the watch directory from the ip-file path.
    let watch_dir = std::path::Path::new(&config.vpn_ip_file)
        .parent()
        .map_or_else(|| "/tmp/gluetun".to_string(), |p| p.to_string_lossy().into_owned());

    // Read VPN files once at boot so the Core can fast-forward if Gluetun is already up.
    let boot_port_files = tokio::task::spawn_blocking({
        let ip_file = config.vpn_ip_file.clone();
        let port_file = config.vpn_port_file.clone();
        move || docker::read_port_files(&ip_file, &port_file)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));

    info!(
        gluetun_healthy = is_gluetun_healthy,
        dependents = ?dependents,
        port_files_ok = boot_port_files.is_ok(),
        "Windlass started"
    );

    let (tx, mut rx) = mpsc::channel::<Event>(128);

    docker::spawn_event_watcher(docker.clone(), tx.clone());
    docker::spawn_file_watcher(
        &watch_dir,
        config.vpn_ip_file.clone(),
        config.vpn_port_file.clone(),
        tx.clone(),
    );

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

    // Bootstrap the state machine with the real world's current state.
    let (new_state, actions) = process_event(
        state,
        Event::Init { is_gluetun_healthy, port_files: boot_port_files },
    );
    state = new_state;
    dispatch(
        actions,
        &config,
        &docker,
        &direct,
        &vpn,
        &mut wakeups,
        &mam_session,
        &dependents,
        &mut cached_cookie,
        &tx,
    );

    while let Some(event) = rx.recv().await {
        debug!(?event, "←");
        let (new_state, actions) = process_event(state, event);
        state = new_state;
        dispatch(
            actions,
            &config,
            &docker,
            &direct,
            &vpn,
            &mut wakeups,
            &mam_session,
            &dependents,
            &mut cached_cookie,
            &tx,
        );
    }

    Ok(())
}

/// Dispatches every action produced by the Core in one synchronous pass.
/// Side effects (HTTP, Docker, file I/O) are spawned as background tasks
/// that send result events back through `tx`.
#[allow(clippy::too_many_arguments)]
fn dispatch(
    actions: Vec<Action>,
    config: &Config,
    docker: &bollard::Docker,
    direct: &reqwest::Client,
    vpn: &reqwest::Client,
    wakeups: &mut HashMap<WakeupId, JoinHandle<()>>,
    mam_session: &Arc<Mutex<String>>,
    dependents: &[String],
    cached_cookie: &mut Option<AuthCookie>,
    tx: &mpsc::Sender<Event>,
) {
    for action in actions {
        dispatch_one(
            action,
            config,
            docker,
            direct,
            vpn,
            wakeups,
            mam_session,
            dependents,
            cached_cookie,
            tx,
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
// dispatch_one is a match over every Action variant — one arm per action.
// Splitting it would just scatter related code without reducing real complexity.
fn dispatch_one(
    action: Action,
    config: &Config,
    docker: &bollard::Docker,
    direct: &reqwest::Client,
    vpn: &reqwest::Client,
    wakeups: &mut HashMap<WakeupId, JoinHandle<()>>,
    mam_session: &Arc<Mutex<String>>,
    dependents: &[String],
    cached_cookie: &mut Option<AuthCookie>,
    tx: &mpsc::Sender<Event>,
) {
    debug!(?action, "→");
    match action {
        // ── Timers ────────────────────────────────────────────────────────
        Action::ScheduleWakeup(id, duration) => {
            // Cancel any existing timer for this id before spawning a new one
            // to prevent leaked duplicate wakeup loops.
            if let Some(handle) = wakeups.remove(&id) {
                handle.abort();
            }
            let tx = tx.clone();
            let handle = tokio::spawn(async move {
                tokio::time::sleep(duration).await;
                let _ = tx.send(Event::Wakeup(id)).await;
            });
            wakeups.insert(id, handle);
        }

        // ── Port files (retry path only — watcher handles normal reads) ──
        Action::ReadPortFiles => {
            let ip_file = config.vpn_ip_file.clone();
            let port_file = config.vpn_port_file.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    docker::read_port_files(&ip_file, &port_file)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                let _ = tx.send(Event::PortFileReadResult(result)).await;
            });
        }

        // ── Docker ────────────────────────────────────────────────────────
        Action::FetchAndDumpAllLogs => {
            let docker = docker.clone();
            let dump_dir = config.dump_dir.clone();
            let deps = dependents.to_vec();
            let tx = tx.clone();
            tokio::spawn(async move {
                docker::fetch_and_dump_logs(&docker, &dump_dir, &deps).await;
                let _ = tx.send(Event::LogsDumped).await;
            });
        }

        Action::StopDependentContainers => {
            let docker = docker.clone();
            let deps = dependents.to_vec();
            tokio::spawn(async move {
                docker::stop_dependents(&docker, &deps).await;
            });
        }

        Action::StartDependentContainers => {
            let docker = docker.clone();
            let deps = dependents.to_vec();
            tokio::spawn(async move {
                docker::start_dependents(&docker, &deps).await;
            });
        }

        Action::RestartGluetun => {
            let docker = docker.clone();
            tokio::spawn(async move {
                docker::restart_gluetun(&docker).await;
            });
        }

        // ── qBittorrent ───────────────────────────────────────────────────
        Action::AuthenticateQbit => {
            let client = direct.clone();
            let url = config.qbit_url.clone();
            let user = config.qbit_user.clone();
            let pass = config.qbit_pass.0.expose_secret().to_owned();
            let tx = tx.clone();
            tokio::spawn(async move {
                let event = qbit::authenticate(&client, &url, &user, &pass).await;
                let _ = tx.send(event).await;
            });
        }

        Action::SyncQbitPort(cookie, port) => {
            *cached_cookie = Some(cookie.clone());
            let client = direct.clone();
            let url = config.qbit_url.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let event = qbit::sync_port(&client, &url, &cookie, port).await;
                let _ = tx.send(event).await;
            });
        }

        // ── MAM ───────────────────────────────────────────────────────────
        Action::UpdateMam(_ip) => {
            let client = vpn.clone();
            let session = mam_session.clone();
            let url = config.mam_seedbox_url.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let current = session.lock().unwrap().clone();
                let (event, new_session) = mam::update_seedbox_at(&client, &current, &url).await;
                if let Some(rotated) = new_session {
                    *session.lock().unwrap() = rotated;
                }
                let _ = tx.send(event).await;
            });
        }

        Action::CheckMamConnectability => {
            let client = vpn.clone();
            let session = mam_session.clone();
            let url = config.mam_load_url.clone();
            let tx = tx.clone();
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

        // ── Monitoring ────────────────────────────────────────────────────
        Action::CheckDiskSpace => {
            let path = config.data_path.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let space = tokio::task::spawn_blocking(move || monitors::check_disk_space(&path))
                    .await
                    .unwrap_or_else(|_| uom::si::f64::Information::new::<byte>(f64::MAX));
                let _ = tx.send(Event::DiskSpaceObserved(space)).await;
            });
        }

        Action::CheckNewTorrents => {
            let Some(cookie) = cached_cookie.clone() else {
                // qBit not yet authenticated — send empty so Core re-arms the timer.
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(Event::NewTorrentsObserved(vec![])).await;
                });
                return;
            };
            let client = direct.clone();
            let url = config.qbit_url.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                // Shell sends the raw full list — Core owns the deduplication logic.
                let current = qbit::list_torrents(&client, &url, &cookie).await;
                let _ = tx.send(Event::NewTorrentsObserved(current)).await;
            });
        }

        // ── Alerts ────────────────────────────────────────────────────────
        Action::SendGotifyAlert(priority, message) => {
            let client = direct.clone();
            let url = config.gotify_url.clone();
            let token = config.gotify_token.clone();
            tokio::spawn(async move {
                gotify::send_alert(&client, &url, &token, priority, &message).await;
            });
        }
    }
}
