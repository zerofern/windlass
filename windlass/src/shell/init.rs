use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use secrecy::ExposeSecret;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use windlass_clients::{mam, qbit};
use windlass_core::{events::Event, types::SystemState};
use windlass_db::DbPool;
use windlass_debug::{
    DebugCommand, DebugController, DebugHistory, DebuggableEventStream, LogEntry, StoredEvent,
};
use windlass_local::{docker, vpn_files};
use windlass_types::WakeupId;

use windlass_mam_core::{MamConfig, MamMachine, MamPublish, MamTopic};
use windlass_qbit_core::{QbitConfig, QbitMachine, QbitPublish, QbitTopic};
use windlass_vpn_core::{VpnConfig, VpnMachine, VpnPublish, VpnTopic};

use super::config::Config;
use super::mam_shell::MamShell;
use super::qbit_shell::QbitShell;
use super::service::ServiceCores;
use super::vpn_shell::{VpnShell, VpnShellConfig};

/// All runtime state extracted from `init_shell` so `run` stays concise.
pub(super) struct ShellRuntime {
    pub(super) debug_stream: DebuggableEventStream,
    pub(super) docker: docker::DockerClient,
    pub(super) dependents: Vec<String>,
    pub(super) qbit: qbit::QbitClient,
    pub(super) mam: mam::MamClient,
    pub(super) db_pool: DbPool,
    pub(super) obs_tx: broadcast::Sender<windlass_core::Observation>,
    pub(super) tx: mpsc::Sender<Event>,
    pub(super) vpn_ip_file: String,
    pub(super) vpn_port_file: String,
    pub(super) data_path: String,
    pub(super) wakeups: HashMap<WakeupId, JoinHandle<()>>,
    pub(super) state: SystemState,
    pub(super) history: DebugHistory,
    pub(super) cmd_rx: mpsc::Receiver<DebugCommand>,
    pub(super) log_rx: mpsc::Receiver<LogEntry>,
    pub(super) queue_rx: mpsc::Receiver<StoredEvent>,
    pub(super) exchange_rx: mpsc::Receiver<(uuid::Uuid, windlass_types::HttpExchange)>,
    pub(super) causal_debug_tx: mpsc::Sender<(Event, uuid::Uuid)>,
    pub(super) causal_rx: mpsc::Receiver<(Event, uuid::Uuid)>,
    pub(super) service_cores: ServiceCores,
    pub(super) execute_service_actions: bool,
}

/// Bootstraps all infrastructure and returns the runtime bundle.
/// Spawns the HTTP server and sends the `Init` event before returning.
#[allow(clippy::too_many_lines)]
pub(super) async fn init_shell(
    debug_ctrl: &DebugController,
    debug_owned: windlass_debug::DebugOwnedPart,
) -> Result<ShellRuntime> {
    let config = Config::from_env()?;

    let (tx, rx) = mpsc::channel::<Event>(128);

    let (docker, boot) = docker::DockerClient::boot(config.dump_dir.clone(), tx.clone()).await?;

    let db_pool = DbPool::connect(&config.database_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to open Postgres database: {e}"))?;
    db_pool
        .migrate()
        .await
        .map_err(|e| anyhow::anyhow!("Database migration failed: {e}"))?;

    let port_files =
        vpn_files::read_and_watch(&config.vpn_ip_file, &config.vpn_port_file, tx.clone()).await;

    info!("Windlass started");

    let on_http = debug_ctrl.make_http_observer();

    let direct = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let qbit = qbit::QbitClient::new(
        direct,
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
        on_http,
    )?;

    let vpn_ip_file = config.vpn_ip_file.clone();
    let vpn_port_file = config.vpn_port_file.clone();
    let data_path = config.data_path.clone();

    let (obs_tx, _) = broadcast::channel::<windlass_core::Observation>(256);

    let debug_stream = DebuggableEventStream::new(
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
        db_pool: db_pool.clone(),
    };
    start_http_server(app_state).await?;

    let wakeups: HashMap<WakeupId, JoinHandle<()>> = HashMap::new();
    let blacklisted = windlass_db::download_queue::get_blacklisted_ids(&db_pool)
        .await
        .unwrap_or_default();
    let state = SystemState::initial()
        .with_compliance_config(
            config.unsatisfied_quota_limit,
            config.compliance_poll_interval_secs,
        )
        .with_blacklisted_ids(blacklisted);
    let (vpn_handles, _vpn_join) = windlass_machine::spawn::<VpnMachine, VpnShell>(
        VpnConfig {
            health_poll_interval: Duration::from_secs(30),
        },
        VpnShellConfig {
            docker: docker.clone(),
            vpn_ip_file: config.vpn_ip_file.clone(),
            vpn_port_file: config.vpn_port_file.clone(),
        },
    )
    .await;
    let (vpn_pub_tx, vpn_pub_rx) = mpsc::channel::<VpnPublish>(128);
    vpn_handles
        .subscribe
        .send((vec![VpnTopic::Connectivity, VpnTopic::Port], vpn_pub_tx))
        .expect("vpn pub subscription");

    let (qbit_handles, _qbit_join) = windlass_machine::spawn::<QbitMachine, QbitShell>(
        QbitConfig {
            auth_retry: Duration::from_secs(5),
            sync_retry: Duration::from_secs(2),
        },
        qbit.clone(),
    )
    .await;
    let (qbit_pub_tx, qbit_pub_rx) = mpsc::channel::<QbitPublish>(128);
    qbit_handles
        .subscribe
        .send((
            vec![
                QbitTopic::Availability,
                QbitTopic::ListenPort,
                QbitTopic::Torrents,
            ],
            qbit_pub_tx,
        ))
        .expect("qbit pub subscription");

    let (mam_handles, _mam_join) = windlass_machine::spawn::<MamMachine, MamShell>(
        MamConfig {
            status_retry: Duration::from_secs(30),
        },
        mam.clone(),
    )
    .await;
    let (mam_pub_tx, mam_pub_rx) = mpsc::channel::<MamPublish>(128);
    mam_handles
        .subscribe
        .send((
            vec![
                MamTopic::Availability,
                MamTopic::Connectability,
                MamTopic::Seedbox,
            ],
            mam_pub_tx,
        ))
        .expect("mam pub subscription");

    let service_cores = ServiceCores::new(
        Duration::from_secs(config.compliance_poll_interval_secs),
        vpn_handles,
        vpn_pub_rx,
        qbit_handles,
        qbit_pub_rx,
        mam_handles,
        mam_pub_rx,
    );
    let execute_service_actions = config.execute_service_actions;
    let history = DebugHistory::new(SystemState::initial());
    let cmd_rx = debug_owned.cmd_rx;
    let log_rx = debug_owned.log_rx;
    let queue_rx = debug_owned.queue_rx;
    let exchange_rx = debug_owned.exchange_rx;

    // Causation channel: action handlers send (event, action_id) here in debug mode.
    let (causal_debug_tx, causal_rx) = mpsc::channel::<(Event, uuid::Uuid)>(128);

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

    Ok(ShellRuntime {
        debug_stream,
        docker,
        dependents: boot.dependents,
        qbit,
        mam,
        db_pool,
        obs_tx,
        tx,
        vpn_ip_file,
        vpn_port_file,
        data_path,
        wakeups,
        state,
        history,
        cmd_rx,
        log_rx,
        queue_rx,
        exchange_rx,
        causal_debug_tx,
        causal_rx,
        service_cores,
        execute_service_actions,
    })
}

async fn start_http_server(app_state: windlass_web::AppState) -> Result<()> {
    let bind_addr = std::env::var("WINDLASS_BIND").unwrap_or_else(|_| "0.0.0.0:5010".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!(addr = %bind_addr, "HTTP server listening");
    tokio::spawn(async move {
        axum::serve(listener, windlass_web::router(app_state))
            .await
            .expect("HTTP server crashed");
    });
    Ok(())
}
