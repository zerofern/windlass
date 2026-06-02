use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;
use windlass_core::events::Event;
use windlass_db_core::DbMachine;
use windlass_disk_core::DiskMachine;
use windlass_domain_core::WindlassMachine;
use windlass_machine::{Command, ExternalCause, ServiceHandles, Timed};
use windlass_mam_core::MamMachine;
use windlass_qbit_core::QbitMachine;
use windlass_types::VpnPort;
use windlass_vpn_core::VpnMachine;

use super::service_events::{ServiceEvent, legacy_to_service_events};

/// Runs the new sans-I/O service cores beside the legacy core.
///
/// This is a migration bridge: it lets the runtime feed real events into the
/// service/domain boundaries while legacy-only orchestration remains available
/// as a rollback path.
///
/// Publish routing from service runtimes to the domain runtime is handled by
/// dedicated async forwarder tasks spawned in `init_shell`; this struct no longer
/// holds publish receivers.
pub(super) struct ServiceCores {
    domain: ServiceHandles<WindlassMachine>,
    db: ServiceHandles<DbMachine>,
    vpn: ServiceHandles<VpnMachine>,
    qbit: ServiceHandles<QbitMachine>,
    mam: ServiceHandles<MamMachine>,
    disk: ServiceHandles<DiskMachine>,
    /// Cached forwarded port, shared with the VPN forwarder task.
    /// Updated by the VPN forwarder on PortReady/PortUnavailable/Disconnected.
    /// Read synchronously by `observe` via the legacy event bridge so that
    /// legacy shell results (e.g. `QbitPortSyncSuccess`) can be translated
    /// correctly while legacy orchestration is still running.
    forwarded_port: Arc<Mutex<Option<VpnPort>>>,
}

impl ServiceCores {
    #[must_use]
    pub const fn new(
        domain: ServiceHandles<WindlassMachine>,
        db: ServiceHandles<DbMachine>,
        vpn: ServiceHandles<VpnMachine>,
        qbit: ServiceHandles<QbitMachine>,
        mam: ServiceHandles<MamMachine>,
        disk: ServiceHandles<DiskMachine>,
        forwarded_port: Arc<Mutex<Option<VpnPort>>>,
    ) -> Self {
        Self {
            domain,
            db,
            vpn,
            qbit,
            mam,
            disk,
            forwarded_port,
        }
    }

    /// Returns the sender for issuing DB commands to the DB runtime.
    pub const fn db_command_tx(&self) -> &mpsc::UnboundedSender<Command<DbMachine>> {
        &self.db.commands
    }

    /// Returns a clone of the shared forwarded-port arc so the VPN forwarder
    /// task can write to it.
    pub fn forwarded_port_arc(&self) -> Arc<Mutex<Option<VpnPort>>> {
        Arc::clone(&self.forwarded_port)
    }

    pub fn observe(&self, event: &Event) {
        let forwarded_port = self.forwarded_port.lock().map_or(None, |g| *g);
        for event in legacy_to_service_events(event, forwarded_port) {
            self.send_service_event(event);
        }
    }

    fn send_service_event(&self, event: ServiceEvent) {
        let now = Instant::now();
        // TODO(§37d): legacy-event bridge — these forwards inherit the
        // cause of the original `Event` (which is currently untyped).
        // Once envelopes flow through the bridge protocol, thread the
        // upstream cause through instead of Unknown.
        match event {
            ServiceEvent::Domain(event) => {
                let _ =
                    self.domain
                        .events
                        .send(Timed::external(now, ExternalCause::Unknown, event));
            }
            ServiceEvent::Vpn(event) => {
                let _ = self
                    .vpn
                    .events
                    .send(Timed::external(now, ExternalCause::Unknown, event));
            }
            ServiceEvent::Qbit(event) => {
                let _ = self
                    .qbit
                    .events
                    .send(Timed::external(now, ExternalCause::Unknown, event));
            }
            ServiceEvent::Mam(event) => {
                let _ = self
                    .mam
                    .events
                    .send(Timed::external(now, ExternalCause::Unknown, event));
            }
            ServiceEvent::Disk(event) => {
                let _ = self
                    .disk
                    .events
                    .send(Timed::external(now, ExternalCause::Unknown, event));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::mpsc;
    use windlass_db_core::{DbEvent, DbMachine, DbPublish};
    use windlass_disk_core::DiskMachine;
    use windlass_domain_core::{WindlassEvent, WindlassMachine, WindlassPublish, WindlassTopic};
    use windlass_machine::{Command, ExternalCause, ServiceHandles, Timed};
    use windlass_mam_core::{MamEvent, MamMachine};
    use windlass_qbit_core::{QbitEvent, QbitMachine, QbitPublish};
    use windlass_vpn_core::{VpnEvent, VpnMachine, VpnPublish};

    use super::ServiceCores;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn make_db_handles() -> (
        ServiceHandles<DbMachine>,
        mpsc::UnboundedReceiver<Command<DbMachine>>,
    ) {
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel::<Timed<DbEvent>>();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (sub_tx, _sub_rx) = mpsc::unbounded_channel();
        (
            ServiceHandles {
                events: ev_tx,
                commands: cmd_tx,
                subscribe: sub_tx,
            },
            cmd_rx,
        )
    }

    fn make_vpn_handles() -> (
        ServiceHandles<VpnMachine>,
        mpsc::UnboundedReceiver<Timed<VpnEvent>>,
    ) {
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Timed<VpnEvent>>();
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command<VpnMachine>>();
        let (sub_tx, _sub_rx) = mpsc::unbounded_channel();
        (
            ServiceHandles {
                events: ev_tx,
                commands: cmd_tx,
                subscribe: sub_tx,
            },
            ev_rx,
        )
    }

    fn make_qbit_handles() -> (
        ServiceHandles<QbitMachine>,
        mpsc::UnboundedReceiver<Command<QbitMachine>>,
    ) {
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel::<Timed<QbitEvent>>();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (sub_tx, _sub_rx) = mpsc::unbounded_channel();
        (
            ServiceHandles {
                events: ev_tx,
                commands: cmd_tx,
                subscribe: sub_tx,
            },
            cmd_rx,
        )
    }

    fn make_disk_handles() -> (
        ServiceHandles<DiskMachine>,
        mpsc::UnboundedReceiver<Timed<windlass_disk_core::DiskEvent>>,
    ) {
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Timed<windlass_disk_core::DiskEvent>>();
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command<DiskMachine>>();
        let (sub_tx, _sub_rx) = mpsc::unbounded_channel();
        (
            ServiceHandles {
                events: ev_tx,
                commands: cmd_tx,
                subscribe: sub_tx,
            },
            ev_rx,
        )
    }

    fn make_mam_handles() -> (
        ServiceHandles<MamMachine>,
        mpsc::UnboundedReceiver<Command<MamMachine>>,
    ) {
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel::<Timed<MamEvent>>();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (sub_tx, _sub_rx) = mpsc::unbounded_channel();
        (
            ServiceHandles {
                events: ev_tx,
                commands: cmd_tx,
                subscribe: sub_tx,
            },
            cmd_rx,
        )
    }

    fn make_domain_handles() -> (
        ServiceHandles<WindlassMachine>,
        mpsc::UnboundedReceiver<Timed<WindlassEvent>>,
    ) {
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Timed<WindlassEvent>>();
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command<WindlassMachine>>();
        let (sub_tx, _sub_rx) = mpsc::unbounded_channel();
        (
            ServiceHandles {
                events: ev_tx,
                commands: cmd_tx,
                subscribe: sub_tx,
            },
            ev_rx,
        )
    }

    fn make_cores() -> (
        ServiceCores,
        mpsc::UnboundedReceiver<Timed<WindlassEvent>>,
        Arc<Mutex<Option<windlass_types::VpnPort>>>,
    ) {
        let (domain_handles, domain_ev_rx) = make_domain_handles();
        let (db_handles, _db_cmd_rx) = make_db_handles();
        let (vpn_handles, _vpn_ev_rx) = make_vpn_handles();
        let (qbit_handles, _qbit_cmd_rx) = make_qbit_handles();
        let (mam_handles, _mam_cmd_rx) = make_mam_handles();
        let (disk_handles, _disk_ev_rx) = make_disk_handles();
        let forwarded_port = Arc::new(Mutex::new(None));
        let cores = ServiceCores::new(
            domain_handles,
            db_handles,
            vpn_handles,
            qbit_handles,
            mam_handles,
            disk_handles,
            Arc::clone(&forwarded_port),
        );
        (cores, domain_ev_rx, forwarded_port)
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    /// `VpnPublish::PortReady` forwarded by the async VPN forwarder task ends up
    /// on the domain event channel as `WindlassEvent::Vpn(PortReady{port})`.
    ///
    /// This test exercises the full forwarder-task path: it spawns a forwarder
    /// task just like `init_shell` does and asserts the domain runtime receives
    /// the event.
    #[tokio::test]
    async fn vpn_forwarder_task_forwards_port_ready_to_domain() {
        use windlass_types::VpnPort;

        let (domain_handles, mut domain_ev_rx) = make_domain_handles();
        let (db_handles, _db_cmd_rx) = make_db_handles();
        let (vpn_handles, _vpn_ev_rx) = make_vpn_handles();
        let (qbit_handles, _qbit_cmd_rx) = make_qbit_handles();
        let (mam_handles, _mam_cmd_rx) = make_mam_handles();
        let forwarded_port = Arc::new(Mutex::new(None));

        // The forwarder task reads from a subscription channel and writes to domain.
        let (vpn_pub_tx, mut vpn_pub_rx) = mpsc::channel::<VpnPublish>(8);
        let domain_ev_tx = domain_handles.events.clone();
        let forwarded_port_arc = Arc::clone(&forwarded_port);
        tokio::spawn(async move {
            while let Some(publish) = vpn_pub_rx.recv().await {
                match &publish {
                    VpnPublish::PortReady { port } => {
                        if let Ok(mut g) = forwarded_port_arc.lock() {
                            *g = Some(*port);
                        }
                    }
                    VpnPublish::PortUnavailable | VpnPublish::Disconnected => {
                        if let Ok(mut g) = forwarded_port_arc.lock() {
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

        let port = VpnPort::try_new(51_820).unwrap();
        vpn_pub_tx
            .send(VpnPublish::PortReady { port })
            .await
            .unwrap();

        let event = domain_ev_rx.recv().await.expect("domain event expected");
        assert_eq!(
            event.inner,
            WindlassEvent::Vpn(VpnPublish::PortReady { port })
        );

        // The shared forwarded_port must have been updated.
        assert_eq!(*forwarded_port.lock().unwrap(), Some(port));

        // Keep remaining handles alive.
        let _ = (
            domain_handles,
            db_handles,
            vpn_handles,
            qbit_handles,
            mam_handles,
        );
    }

    /// `VpnPublish::PortReady` flowing through the VPN forwarder task into a
    /// real spawned `WindlassMachine` domain runtime produces
    /// `QbitCommand::EnsureListenPort` and `MamCommand::EnsureSeedboxPort` on
    /// the respective command channels — the full subscription path.
    #[tokio::test]
    async fn vpn_port_ready_via_forwarder_task_sends_qbit_and_mam_commands() {
        use windlass_domain_core::{WindlassConfig, WindlassMachine};
        use windlass_machine::Command;
        use windlass_mam_core::MamCommand;
        use windlass_qbit_core::QbitCommand;
        use windlass_types::VpnPort;

        let port = VpnPort::try_new(51_820).unwrap();

        // Build the domain runtime.
        let (qbit_cmd_tx, mut qbit_cmd_rx) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (mam_cmd_tx, mut mam_cmd_rx) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (db_cmd_tx, _db_cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (vpn_cmd_tx, _vpn_cmd_rx) = mpsc::unbounded_channel::<Command<VpnMachine>>();

        let (docker_cmd_tx, _docker_cmd_rx) =
            mpsc::unbounded_channel::<Command<windlass_docker_core::DockerMachine>>();
        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            vpn: vpn_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
            docker: docker_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                    gluetun_anchor: "gluetun".to_string(),
                    initial_blacklist: std::collections::HashSet::new(),
                },
                domain_shell_cfg,
            )
            .await;

        // Simulate the VPN publish subscription channel.
        let (vpn_pub_tx, mut vpn_pub_rx) = mpsc::channel::<VpnPublish>(8);

        // Spawn the VPN forwarder task (same logic as init_shell).
        let domain_ev_tx = domain_handles.events.clone();
        let forwarded_port_arc: Arc<Mutex<Option<VpnPort>>> = Arc::new(Mutex::new(None));
        let fp_arc = Arc::clone(&forwarded_port_arc);
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

        // Send PortReady through the subscription channel.
        vpn_pub_tx
            .send(VpnPublish::PortReady { port })
            .await
            .unwrap();

        // Yield until we see both qBit and MAM commands.
        let mut got_qbit = false;
        let mut got_mam = false;
        for _ in 0..20 {
            tokio::task::yield_now().await;
            while let Ok((cmd, _)) = qbit_cmd_rx.try_recv() {
                if matches!(cmd, QbitCommand::EnsureListenPort { port: p } if p == port) {
                    got_qbit = true;
                }
            }
            while let Ok((cmd, _)) = mam_cmd_rx.try_recv() {
                if matches!(cmd, MamCommand::EnsureSeedboxPort { port: p } if p == port) {
                    got_mam = true;
                }
            }
            if got_qbit && got_mam {
                break;
            }
        }

        assert!(
            got_qbit,
            "expected QbitCommand::EnsureListenPort on qbit channel via subscription forwarder"
        );
        assert!(
            got_mam,
            "expected MamCommand::EnsureSeedboxPort on mam channel via subscription forwarder"
        );
    }

    /// DB forwarder task: non-RecordActivity failure is forwarded to the domain
    /// event channel as `WindlassEvent::DbFailed`.
    #[tokio::test]
    async fn db_forwarder_task_forwards_failure_to_domain() {
        use windlass_db_core::DbFailure;

        let (domain_handles, mut domain_ev_rx) = make_domain_handles();
        let (db_pub_tx, mut db_pub_rx) = mpsc::channel::<DbPublish>(8);

        // Spawn DB forwarder task (same logic as init_shell).
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

        db_pub_tx
            .send(DbPublish::Failed(DbFailure {
                operation: "SaveSystemSnapshot".to_string(),
                message: "disk full".to_string(),
                retryable: false,
            }))
            .await
            .unwrap();

        let event = domain_ev_rx.recv().await.expect("domain event expected");
        assert_eq!(
            event.inner,
            WindlassEvent::DbFailed {
                operation: "SaveSystemSnapshot".to_string(),
                message: "disk full".to_string(),
            }
        );
    }

    /// DB forwarder task: `RecordActivity` failures are NOT forwarded (recursion guard).
    #[tokio::test]
    async fn db_forwarder_task_does_not_forward_record_activity_failure() {
        use windlass_db_core::DbFailure;

        let (domain_handles, mut domain_ev_rx) = make_domain_handles();
        let (db_pub_tx, mut db_pub_rx) = mpsc::channel::<DbPublish>(8);

        // Spawn DB forwarder task.
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

        db_pub_tx
            .send(DbPublish::Failed(DbFailure {
                operation: "RecordActivity".to_string(),
                message: "connection lost".to_string(),
                retryable: true,
            }))
            .await
            .unwrap();

        // Give the task a chance to process.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert!(
            domain_ev_rx.try_recv().is_err(),
            "RecordActivity failure must not recurse into domain"
        );
    }

    /// Domain forwarder task: `WindlassPublish::Activity` becomes a
    /// `DbCommand::RecordActivity` on the DB command channel.
    #[tokio::test]
    async fn domain_forwarder_task_activity_becomes_db_record_activity() {
        use windlass_db_core::{ActivitySource, DbCommand};

        let (domain_pub_tx, mut domain_pub_rx) = mpsc::channel::<WindlassPublish>(8);
        let (db_cmd_tx, mut db_cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();

        // Spawn domain forwarder task (same logic as init_shell).
        tokio::spawn(async move {
            while let Some(publish) = domain_pub_rx.recv().await {
                match publish {
                    WindlassPublish::SystemState(_) => {}
                    WindlassPublish::Activity { message } => {
                        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
                        let _ = db_cmd_tx.send((
                            DbCommand::RecordActivity(windlass_db_core::ActivityRecord {
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

        domain_pub_tx
            .send(WindlassPublish::Activity {
                message: "test activity".to_string(),
            })
            .await
            .unwrap();

        // Wait for the DB command to arrive.
        let (cmd, _) = tokio::time::timeout(Duration::from_secs(1), db_cmd_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        match cmd {
            DbCommand::RecordActivity(record) => {
                assert_eq!(record.source, ActivitySource::Domain);
                assert_eq!(record.detail.as_deref(), Some("test activity"));
            }
            other => panic!("expected RecordActivity, got {:?}", other),
        }
    }

    /// `QbitPublish::Unavailable` → domain runtime → `WindlassPublish::Activity`
    /// → domain forwarder → `DbCommand::RecordActivity`.
    #[tokio::test]
    async fn qbit_unavailable_through_runtime_produces_activity_db_command() {
        use windlass_domain_core::{WindlassConfig, WindlassMachine};
        use windlass_machine::Command;

        let (qbit_cmd_tx, _qbit_cmd_rx) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (mam_cmd_tx, _mam_cmd_rx) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (db_cmd_tx, _db_cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (vpn_cmd_tx, _vpn_cmd_rx) = mpsc::unbounded_channel::<Command<VpnMachine>>();

        let (docker_cmd_tx, _docker_cmd_rx) =
            mpsc::unbounded_channel::<Command<windlass_docker_core::DockerMachine>>();
        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            vpn: vpn_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
            docker: docker_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                    gluetun_anchor: "gluetun".to_string(),
                    initial_blacklist: std::collections::HashSet::new(),
                },
                domain_shell_cfg,
            )
            .await;

        // Subscribe to Activity publishes.
        let (activity_tx, mut activity_rx) = mpsc::channel::<WindlassPublish>(8);
        domain_handles
            .subscribe
            .send((vec![WindlassTopic::Activity], activity_tx))
            .unwrap();

        // Inject a QbitPublish::Unavailable event.
        domain_handles
            .events
            .send(Timed::external(
                std::time::Instant::now(),
                ExternalCause::Unknown,
                WindlassEvent::Qbit(QbitPublish::Unavailable {
                    reason: "qBittorrent rejected credentials".to_string(),
                }),
            ))
            .unwrap();

        // Wait for the Activity publish.
        let publish = tokio::time::timeout(std::time::Duration::from_secs(1), activity_rx.recv())
            .await
            .expect("timeout waiting for activity publish")
            .expect("channel closed");

        match publish {
            WindlassPublish::Activity { message } => {
                assert_eq!(message, "qBittorrent rejected credentials");
            }
            other => panic!("expected Activity publish, got {:?}", other),
        }
    }

    /// `DbFailed` event round-trips: forwarded to domain → domain publishes
    /// `Activity` → drain produces `RecordActivity`.
    #[tokio::test]
    async fn db_failure_domain_event_becomes_activity_via_runtime() {
        use windlass_domain_core::{WindlassConfig, WindlassMachine};
        use windlass_machine::Command;

        let (qbit_cmd_tx, _qbit_cmd_rx) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (mam_cmd_tx, _mam_cmd_rx) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (db_cmd_tx, _db_cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (vpn_cmd_tx, _vpn_cmd_rx) = mpsc::unbounded_channel::<Command<VpnMachine>>();

        let (docker_cmd_tx, _docker_cmd_rx) =
            mpsc::unbounded_channel::<Command<windlass_docker_core::DockerMachine>>();
        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            vpn: vpn_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
            docker: docker_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                    gluetun_anchor: "gluetun".to_string(),
                    initial_blacklist: std::collections::HashSet::new(),
                },
                domain_shell_cfg,
            )
            .await;

        let (activity_tx, mut activity_rx) = mpsc::channel::<WindlassPublish>(8);
        domain_handles
            .subscribe
            .send((vec![WindlassTopic::Activity], activity_tx))
            .unwrap();

        domain_handles
            .events
            .send(Timed::external(
                std::time::Instant::now(),
                ExternalCause::Unknown,
                WindlassEvent::DbFailed {
                    operation: "SaveSystemSnapshot".to_string(),
                    message: "database unavailable".to_string(),
                },
            ))
            .unwrap();

        let publish = tokio::time::timeout(std::time::Duration::from_secs(1), activity_rx.recv())
            .await
            .expect("timeout waiting for activity")
            .expect("channel closed");

        match publish {
            WindlassPublish::Activity { message } => {
                assert!(
                    message.contains("SaveSystemSnapshot"),
                    "expected activity mentioning SaveSystemSnapshot, got: {message}"
                );
                assert!(
                    message.contains("database unavailable"),
                    "expected activity mentioning 'database unavailable', got: {message}"
                );
            }
            other => panic!("expected Activity publish, got {:?}", other),
        }
    }
}
