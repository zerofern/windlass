use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use windlass_clients::{mam, qbit};
use windlass_db::DbPool;
use windlass_local::{docker, vpn_files};
use windlass_types::{VpnPort, WakeupId};

use windlass_db_core::{ActivityRecord, ActivitySource, DbCommand, DbMachine, DbPublish, DbTopic};
use windlass_disk_core::{DiskConfig, DiskMachine, DiskPublish, DiskTopic};
use windlass_docker_core::{DockerConfig, DockerMachine};
use windlass_domain_core::{
    WindlassConfig, WindlassEvent, WindlassMachine, WindlassPublish, WindlassTopic,
};
use windlass_machine::{ExternalCause, Timed};
use windlass_mam_core::{MamConfig, MamMachine, MamPublish, MamTopic};
use windlass_qbit_core::{QbitConfig, QbitMachine, QbitPublish, QbitTopic};
use windlass_vpn_core::{VpnConfig, VpnMachine, VpnPublish, VpnTopic};

use super::config::Config;
use super::db_shell::DbShell;
use super::disk_shell::DiskShell;
use super::docker_shell::{DockerShell, DockerShellConfig};
use super::domain_shell::{DomainShell, DomainShellConfig};
use super::mam_shell::MamShell;
use super::qbit_shell::QbitShell;
use super::service::ServiceCores;
use super::vpn_shell::{VpnShell, VpnShellConfig};

/// Owns everything constructed by `init_shell` that needs to outlive
/// the function call.  `shell::run` keeps the bundle alive for the
/// lifetime of the process so background tasks can keep using the
/// clones they captured at spawn time.
pub(super) struct ShellRuntime {
    pub(super) docker: docker::DockerClient,
    pub(super) dependents: Vec<String>,
    pub(super) qbit: qbit::QbitClient,
    pub(super) mam: mam::MamClient,
    pub(super) vpn_ip_file: String,
    pub(super) vpn_port_file: String,
    pub(super) data_path: String,
    pub(super) wakeups: HashMap<WakeupId, JoinHandle<()>>,
    pub(super) service_cores: ServiceCores,
    pub(super) execute_service_actions: bool,
}

/// Bootstraps all infrastructure and returns the runtime bundle.
/// Spawns the HTTP server and sends the `Init` event before returning.
///
/// `observability` is constructed in `main` so its log layer can
/// capture early-boot tracing events; this function only threads it
/// into every spawn site, into clients (via `HttpTap`), and into the
/// `AppState`.
#[allow(clippy::too_many_lines)]
pub(super) async fn init_shell(
    observability: std::sync::Arc<windlass_observability::ObservabilityController>,
) -> Result<ShellRuntime> {
    let config = Config::from_env()?;

    let (docker, boot) = docker::DockerClient::boot(config.dump_dir.clone()).await?;

    let db_pool = DbPool::connect(&config.database_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to open Postgres database: {e}"))?;
    db_pool
        .migrate()
        .await
        .map_err(|e| anyhow::anyhow!("Database migration failed: {e}"))?;

    // §36 step 9b: vpn_files now emits a typed `PortFileResult`; the
    // forwarder task is spawned below (after VPN handles exist) to feed
    // VpnEvent::PortFileChanged / PublicIpFromFile / StateReadFailed
    // directly into the VpnMachine.
    let (file_result_tx, file_result_rx) = mpsc::channel::<vpn_files::PortFileResult>(16);
    let port_files =
        vpn_files::read_and_watch(&config.vpn_ip_file, &config.vpn_port_file, file_result_tx).await;

    info!("Windlass started");

    let direct = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    // §37j: the controller is the only observability surface.  It was
    // constructed in `main` (so the log layer captures boot events)
    // and is threaded through ServiceRuntime::spawn (as RuntimeTap +
    // HttpTap) and into AppState for the /api/v1/observability routes.
    let runtime_tap: std::sync::Arc<dyn windlass_machine::RuntimeTap> = observability.clone();
    let http_tap: std::sync::Arc<dyn windlass_types::HttpTap> = observability.clone();

    let qbit = qbit::QbitClient::new(
        direct,
        config.qbit_url.clone(),
        config.qbit_user.clone(),
        config.qbit_pass.clone(),
        http_tap.clone(),
    );
    let mam = mam::MamClient::new(
        config.gluetun_proxy_url.as_deref(),
        config.mam_session.clone(),
        config.mam_seedbox_url.clone(),
        config.mam_load_url.clone(),
        &config.mam_user_agent,
        http_tap,
    )?;

    let vpn_ip_file = config.vpn_ip_file.clone();
    let vpn_port_file = config.vpn_port_file.clone();
    let data_path = config.data_path.clone();

    // §36 step 5: HTTP server start is deferred until after the domain
    // runtime is spawned so AppState can carry the domain command channel
    // (for `WindlassCommand::ManualDownload`).

    let wakeups: HashMap<WakeupId, JoinHandle<()>> = HashMap::new();
    let blacklisted = windlass_db::download_queue::get_blacklisted_ids(&db_pool)
        .await
        .unwrap_or_default();
    // §36 step 5: feed the blacklist into the new domain core so
    // manual-download admission can reject previously-dead torrents.
    let initial_blacklist: std::collections::HashSet<windlass_types::MamTorrentId> =
        blacklisted.iter().copied().collect();
    let (db_handles, _db_join) = windlass_machine::spawn::<DbMachine, DbShell>(
        windlass_machine::CoreId::Db,
        runtime_tap.clone(),
        (),
        db_pool.clone(),
    )
    .await;
    let (db_pub_tx, mut db_pub_rx) = mpsc::channel::<DbPublish>(128);
    db_handles
        .subscribe
        .send((vec![DbTopic::Failures, DbTopic::Results], db_pub_tx))
        .expect("db pub subscription");

    // §38 PR 2-3: spawn DockerShell + DockerMachine.  §35's stale-
    // namespace and restart-storm circuit-breaker now live here.  PR 4
    // (crash-recovery orchestration) will route publishes to the domain.
    let (docker_handles, _docker_join) = windlass_machine::spawn::<DockerMachine, DockerShell>(
        windlass_machine::CoreId::Docker,
        runtime_tap.clone(),
        DockerConfig {
            gluetun_anchor: docker.gluetun_anchor.clone(),
            // §35: restart circuit-breaker — 3 restarts per 10-minute
            // window.  Defaults preserved from the VPN-core era.
            max_restarts_per_window: 3,
            restart_window_duration: Duration::from_mins(10),
            // §38 PR 5: subsume the autoheal sidecar.  Docker core
            // restarts any unhealthy dependent (circuit-breakered).
            // Anchor crash recovery stays with the VPN→domain path.
            autoheal_dependents: true,
        },
        DockerShellConfig {
            docker: docker.clone(),
        },
    )
    .await;
    let (docker_pub_tx, mut docker_pub_rx) =
        mpsc::channel::<windlass_docker_core::DockerPublish>(128);
    docker_handles
        .subscribe
        .send((
            vec![
                windlass_docker_core::DockerTopic::Lifecycle,
                windlass_docker_core::DockerTopic::Logs,
            ],
            docker_pub_tx,
        ))
        .expect("docker pub subscription");

    // §36 step 4: spawn DiskShell + DiskMachine.  DiskShell has no
    // actions; events arrive via the service-events bridge from
    // `Event::DiskSpaceObserved`.  Domain DOM-9 consumes
    // `DiskPublish::BelowFloor` to fire the Warning alert and trigger
    // disk-pressure eviction.
    let (disk_handles, _disk_join) = windlass_machine::spawn::<DiskMachine, DiskShell>(
        windlass_machine::CoreId::Disk,
        runtime_tap.clone(),
        // 50 GiB hard floor — mirrors the legacy threshold so the alert
        // and eviction trigger at the same point.
        DiskConfig {
            hard_floor_bytes: 50 * 1_073_741_824,
        },
        (),
    )
    .await;
    let (disk_pub_tx, mut disk_pub_rx) = mpsc::channel::<DiskPublish>(128);
    disk_handles
        .subscribe
        .send((vec![DiskTopic::Pressure], disk_pub_tx))
        .expect("disk pub subscription");

    let (vpn_handles, _vpn_join) = windlass_machine::spawn::<VpnMachine, VpnShell>(
        windlass_machine::CoreId::Vpn,
        runtime_tap.clone(),
        VpnConfig {
            health_poll_interval: Duration::from_secs(30),
            unhealthy_poll_interval: Duration::from_secs(5),
            port_read_retry_interval: Duration::from_millis(500),
            // §31: ifconfig.co verification cadence + threshold.
            public_ip_verify_interval: Duration::from_hours(6),
            public_ip_verify_failure_threshold: 3,
        },
        VpnShellConfig {
            vpn_ip_file: config.vpn_ip_file.clone(),
            vpn_port_file: config.vpn_port_file.clone(),
            // §31: route ifconfig.co through Gluetun so the verified IP is
            // the VPN exit IP, not the host's public IP.
            vpn_proxy_url: config.gluetun_proxy_url.clone(),
            public_ip_verify_url: None,
            mam_ip_verify_url: None,
        },
    )
    .await;
    let (vpn_pub_tx, mut vpn_pub_rx) = mpsc::channel::<VpnPublish>(128);
    vpn_handles
        .subscribe
        .send((
            vec![VpnTopic::Connectivity, VpnTopic::Port, VpnTopic::PublicIp],
            vpn_pub_tx,
        ))
        .expect("vpn pub subscription");

    // §36 step 9b: pump vpn_files results (typed) directly into the
    // VpnMachine.  Replaces the legacy `Event::PortFileReadResult` path
    // via service_cores.observe — that bridge entry is now redundant.
    {
        let vpn_event_tx = vpn_handles.events.clone();
        let mut file_result_rx = file_result_rx;
        tokio::spawn(async move {
            while let Some(result) = file_result_rx.recv().await {
                match result {
                    Ok((ip, port)) => {
                        let _ = vpn_event_tx.send(Timed::external(
                            std::time::Instant::now(),
                            ExternalCause::Unknown,
                            windlass_vpn_core::VpnEvent::PortFileChanged { port },
                        ));
                        let _ = vpn_event_tx.send(Timed::external(
                            std::time::Instant::now(),
                            ExternalCause::Unknown,
                            windlass_vpn_core::VpnEvent::PublicIpFromFile { ip },
                        ));
                    }
                    Err(reason) => {
                        let _ = vpn_event_tx.send(Timed::external(
                            std::time::Instant::now(),
                            ExternalCause::Unknown,
                            windlass_vpn_core::VpnEvent::StateReadFailed { reason },
                        ));
                    }
                }
            }
        });
    }

    let (qbit_handles, _qbit_join) = windlass_machine::spawn::<QbitMachine, QbitShell>(
        windlass_machine::CoreId::Qbit,
        runtime_tap.clone(),
        QbitConfig {
            auth_retry: Duration::from_secs(5),
            sync_retry: Duration::from_secs(2),
            torrent_refresh: Duration::from_secs(30),
            hnr_seed_time: Duration::from_hours(72),
            // MAM Power User class cap (MAM Rule 2.8 — §25).
            // Set to 0 to disable the gate (e.g. for lower-class accounts where
            // the real limit is unknown; operators should set this explicitly).
            unsatisfied_quota_limit: 100,
            // §36 step 3: 3 consecutive port-sync failures trip the
            // persistent-failure publish (Warning alert + cookie clear).
            max_sync_attempts: 3,
        },
        qbit.clone(),
    )
    .await;
    let (qbit_pub_tx, mut qbit_pub_rx) = mpsc::channel::<QbitPublish>(128);
    qbit_handles
        .subscribe
        .send((
            vec![
                QbitTopic::Availability,
                QbitTopic::ListenPort,
                QbitTopic::Torrents,
                QbitTopic::Privacy,
                QbitTopic::Queue,
                QbitTopic::Quota,
            ],
            qbit_pub_tx,
        ))
        .expect("qbit pub subscription");

    let (mam_handles, _mam_join) = windlass_machine::spawn::<MamMachine, MamShell>(
        windlass_machine::CoreId::Mam,
        runtime_tap.clone(),
        MamConfig {
            status_retry: Duration::from_secs(30),
            // §26 upload-health gate defaults (binary GiB; see MamConfig docs).
            min_global_ratio: 2.0,
            min_upload_buffer_bytes: windlass_mam_core::DEFAULT_MIN_UPLOAD_BUFFER_BYTES,
            // §27 MAM keep-alive heartbeat defaults (300 s cadence, 3-failure
            // alert threshold; see MamConfig docs).
            keep_alive_interval: windlass_mam_core::DEFAULT_KEEP_ALIVE_INTERVAL,
            keep_alive_failure_threshold: windlass_mam_core::DEFAULT_KEEP_ALIVE_FAILURE_THRESHOLD,
            // §31 stale-registration refresh — 24h cookie/registration
            // refresh, mirrors Mousehole's STALE_RESPONSE_SECONDS.
            stale_registration_interval: windlass_mam_core::DEFAULT_STALE_REGISTRATION_INTERVAL,
        },
        mam.clone(),
    )
    .await;
    let (mam_pub_tx, mut mam_pub_rx) = mpsc::channel::<MamPublish>(128);
    mam_handles
        .subscribe
        .send((
            vec![
                MamTopic::Availability,
                MamTopic::Connectability,
                MamTopic::Seedbox,
                MamTopic::UploadHealth,
                MamTopic::KeepAlive,
                MamTopic::Compliance,
            ],
            mam_pub_tx,
        ))
        .expect("mam pub subscription");

    let (domain_handles, _domain_join) = windlass_machine::spawn::<WindlassMachine, DomainShell>(
        windlass_machine::CoreId::Domain,
        runtime_tap.clone(),
        WindlassConfig {
            snapshot_interval: Duration::from_secs(config.compliance_poll_interval_secs),
            gluetun_anchor: docker.gluetun_anchor.clone(),
            initial_blacklist,
        },
        DomainShellConfig {
            db: db_handles.commands.clone(),
            vpn: vpn_handles.commands.clone(),
            qbit: qbit_handles.commands.clone(),
            mam: mam_handles.commands.clone(),
            docker: docker_handles.commands.clone(),
        },
    )
    .await;
    let (domain_pub_tx, mut domain_pub_rx) = mpsc::channel::<WindlassPublish>(128);
    domain_handles
        .subscribe
        .send((
            vec![WindlassTopic::SystemState, WindlassTopic::Activity],
            domain_pub_tx,
        ))
        .expect("domain pub subscription");

    // §36 step 5: now that the domain runtime exists, build AppState and
    // start the HTTP server.  The web layer carries
    // `domain_command_tx` for `WindlassCommand::ManualDownload`.
    let app_state = windlass_web::AppState {
        domain_command_tx: domain_handles.commands.clone(),
        observability: observability.clone(),
        chaos_url: std::env::var("CHAOS_URL").ok(),
        db_pool: db_pool.clone(),
    };
    start_http_server(app_state).await?;

    // ── Shared forwarded-port state ───────────────────────────────────────────
    // Written by the VPN forwarder task; read synchronously by ServiceCores::observe
    // so the legacy event bridge can translate legacy shell results correctly.
    let forwarded_port: Arc<Mutex<Option<VpnPort>>> = Arc::new(Mutex::new(None));

    // ── VPN forwarder task ────────────────────────────────────────────────────
    // Drains VPN publishes, updates the shared forwarded_port cache, and injects
    // the publish into the domain event channel as Timed<WindlassEvent::Vpn(...)>.
    {
        let domain_ev_tx = domain_handles.events.clone();
        let fp_arc = Arc::clone(&forwarded_port);
        tokio::spawn(async move {
            while let Some(publish) = vpn_pub_rx.recv().await {
                match &publish {
                    VpnPublish::PortReady { port } => {
                        if let Ok(mut g) = fp_arc.lock() {
                            *g = Some(*port);
                        }
                    }
                    VpnPublish::PortUnavailable | VpnPublish::Disconnected => {
                        if let Ok(mut g) = fp_arc.lock() {
                            *g = None;
                        }
                    }
                    VpnPublish::Connected
                    | VpnPublish::Crashed
                    | VpnPublish::Recovered
                    | VpnPublish::PublicIpObserved { .. }
                    | VpnPublish::PublicIpUnavailable
                    | VpnPublish::PublicIpMismatch { .. }
                    | VpnPublish::PublicIpVerificationDegraded { .. }
                    | VpnPublish::MamIpVerificationDegraded { .. } => {}
                }
                let _ = domain_ev_tx.send(Timed::external(
                    std::time::Instant::now(),
                    ExternalCause::Unknown,
                    WindlassEvent::Vpn(publish),
                ));
            }
        });
    }

    // ── Docker forwarder task ─────────────────────────────────────────────────
    // §38 PR 3: drains Docker-core publishes (lifecycle + logs) and injects
    // them into the domain event channel.  Domain currently consumes only
    // the §35 stale-namespace / RestartStorm publishes; the others are
    // forwarded so PR 4 can wire crash-recovery without re-shaping init.
    {
        let domain_ev_tx = domain_handles.events.clone();
        tokio::spawn(async move {
            while let Some(publish) = docker_pub_rx.recv().await {
                let _ = domain_ev_tx.send(Timed::external(
                    std::time::Instant::now(),
                    ExternalCause::Unknown,
                    WindlassEvent::Docker(publish),
                ));
            }
        });
    }

    // ── qBit forwarder task ───────────────────────────────────────────────────
    // Drains qBit publishes and injects them into the domain event channel.
    // qBit does NOT subscribe to VPN facts; cross-service policy stays in the domain.
    {
        let domain_ev_tx = domain_handles.events.clone();
        tokio::spawn(async move {
            while let Some(publish) = qbit_pub_rx.recv().await {
                let _ = domain_ev_tx.send(Timed::external(
                    std::time::Instant::now(),
                    ExternalCause::Unknown,
                    WindlassEvent::Qbit(publish),
                ));
            }
        });
    }

    // ── MAM forwarder task ────────────────────────────────────────────────────
    // Drains MAM publishes and injects them into the domain event channel.
    // MAM does NOT subscribe to VPN facts; cross-service policy stays in the domain.
    {
        let domain_ev_tx = domain_handles.events.clone();
        tokio::spawn(async move {
            while let Some(publish) = mam_pub_rx.recv().await {
                let _ = domain_ev_tx.send(Timed::external(
                    std::time::Instant::now(),
                    ExternalCause::Unknown,
                    WindlassEvent::Mam(publish),
                ));
            }
        });
    }

    // ── Disk forwarder task ───────────────────────────────────────────────────
    // §36 step 4: drains DiskMachine publishes (BelowFloor / AboveFloor)
    // and injects them into the domain event channel.  Domain DOM-9
    // handles the Warning alert + eviction.
    {
        let domain_ev_tx = domain_handles.events.clone();
        tokio::spawn(async move {
            while let Some(publish) = disk_pub_rx.recv().await {
                let _ = domain_ev_tx.send(Timed::external(
                    std::time::Instant::now(),
                    ExternalCause::Unknown,
                    WindlassEvent::Disk(publish),
                ));
            }
        });
    }

    // ── DB forwarder task ─────────────────────────────────────────────────────
    // Drains DB publishes.  Failures other than `RecordActivity` are forwarded to
    // the domain event channel as `WindlassEvent::DbFailed`.  `RecordActivity`
    // failures are only logged (recursion guard: forwarding them would cause the
    // domain to issue another RecordActivity, creating an infinite loop).
    // `Succeeded` publishes are silently discarded.
    {
        let domain_ev_tx = domain_handles.events.clone();
        tokio::spawn(async move {
            while let Some(publish) = db_pub_rx.recv().await {
                match publish {
                    DbPublish::Succeeded { .. } => {}
                    DbPublish::Failed(failure) => {
                        if failure.operation == "RecordActivity" {
                            tracing::warn!(
                                operation = %failure.operation,
                                "DB activity log failed: {}",
                                failure.message
                            );
                        } else {
                            let event = WindlassEvent::DbFailed {
                                operation: failure.operation,
                                message: failure.message,
                            };
                            let _ = domain_ev_tx.send(Timed::external(
                                std::time::Instant::now(),
                                ExternalCause::Unknown,
                                event,
                            ));
                        }
                    }
                }
            }
        });
    }

    // ── Domain Activity → DB forwarder task ───────────────────────────────────
    // Drains domain publishes.  `Activity` publishes become `RecordActivity` DB
    // commands so activity logging is preserved.  `SystemState` publishes are
    // silently discarded (the UI still uses the legacy core for state).
    {
        let db_cmd_tx = db_handles.commands.clone();
        tokio::spawn(async move {
            while let Some(publish) = domain_pub_rx.recv().await {
                match publish {
                    WindlassPublish::SystemState(_) => {}
                    WindlassPublish::Activity { message } => {
                        let (reply_tx, _reply_rx) = oneshot::channel();
                        let _ = db_cmd_tx.send((
                            DbCommand::RecordActivity(ActivityRecord {
                                at: chrono::Utc::now(),
                                source: ActivitySource::Domain,
                                action: "service_activity".to_string(),
                                book_id: None,
                                detail: Some(message),
                                metadata: serde_json::Value::Null,
                            }),
                            reply_tx,
                        ));
                    }
                }
            }
        });
    }

    let service_cores = ServiceCores::new(
        domain_handles,
        db_handles,
        vpn_handles,
        qbit_handles,
        mam_handles,
        disk_handles,
        forwarded_port,
    );
    let execute_service_actions = config.execute_service_actions;

    if let Err(e) = mam.check_session().await {
        warn!("MAM session check failed at startup: {e} — continuing anyway");
    }

    // §37j: dispatch the boot Init events directly to each per-core
    // runtime's typed event channel.  Replaces the legacy
    // `tx.send(Event::Init { ... })` + `service_cores.observe` bridge
    // that used to translate one untyped Init into four typed ones.
    service_cores.dispatch_init(boot.is_gluetun_healthy, &port_files);

    Ok(ShellRuntime {
        docker,
        dependents: boot.dependents,
        qbit,
        mam,
        vpn_ip_file,
        vpn_port_file,
        data_path,
        wakeups,
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
