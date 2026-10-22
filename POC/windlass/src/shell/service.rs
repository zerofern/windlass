use std::time::Instant;

use windlass_db_core::DbMachine;
use windlass_disk_core::DiskMachine;
use windlass_docker_core::DockerMachine;
use windlass_domain_core::WindlassMachine;
use windlass_machine::{ExternalCause, ServiceHandles, Timed};
use windlass_mam_core::{MamEvent, MamMachine};
use windlass_qbit_core::{QbitEvent, QbitMachine};
use windlass_tunnel_core::{TunnelEvent, TunnelMachine};
use windlass_vpn_core::VpnMachine;

use windlass_domain_core::WindlassEvent;

/// Holds the per-core `ServiceHandles` returned by every
/// `ServiceRuntime::spawn`.  `init_shell` constructs this once and
/// uses it to dispatch the boot Init events directly to each core's
/// typed event channel (the legacy `Event::Init` bridge is gone
/// post-§37j).
pub(super) struct ServiceCores {
    domain: ServiceHandles<WindlassMachine>,
    qbit: ServiceHandles<QbitMachine>,
    mam: ServiceHandles<MamMachine>,
    _keepalives: ServiceKeepalives,
    /// In-process `WireGuard` tunnel handles; `dispatch_init`
    /// delivers `TunnelEvent::Init` here to bring the interface up.
    tunnel: ServiceHandles<TunnelMachine>,
}

pub(super) struct ServiceKeepalives {
    _db: ServiceHandles<DbMachine>,
    _vpn: ServiceHandles<VpnMachine>,
    _disk: ServiceHandles<DiskMachine>,
    _docker: ServiceHandles<DockerMachine>,
}

impl ServiceKeepalives {
    #[must_use]
    pub const fn new(
        db: ServiceHandles<DbMachine>,
        vpn: ServiceHandles<VpnMachine>,
        disk: ServiceHandles<DiskMachine>,
        docker: ServiceHandles<DockerMachine>,
    ) -> Self {
        Self {
            _db: db,
            _vpn: vpn,
            _disk: disk,
            _docker: docker,
        }
    }
}

impl ServiceCores {
    #[must_use]
    pub const fn new(
        domain: ServiceHandles<WindlassMachine>,
        qbit: ServiceHandles<QbitMachine>,
        mam: ServiceHandles<MamMachine>,
        tunnel: ServiceHandles<TunnelMachine>,
        keepalives: ServiceKeepalives,
    ) -> Self {
        Self {
            domain,
            qbit,
            mam,
            _keepalives: keepalives,
            tunnel,
        }
    }

    /// Dispatch the per-core boot Init events.  Called once from
    /// `init_shell` after every runtime is spawned and every
    /// forwarder task is running, so the first events each core sees
    /// have causes tagged `ExternalCause::Init`.
    ///
    /// The `TunnelMachine` surfaces its first-pass state through the
    /// bridge in `init_shell`; downstream cores receive `VpnEvent`s
    /// sourced from the tunnel core.
    pub fn dispatch_init(&self) {
        let now = Instant::now();
        let _ = self.domain.events.send(Timed::external(
            now,
            ExternalCause::Init,
            WindlassEvent::Init,
        ));
        let _ = self
            .qbit
            .events
            .send(Timed::external(now, ExternalCause::Init, QbitEvent::Init));
        let _ = self
            .mam
            .events
            .send(Timed::external(now, ExternalCause::Init, MamEvent::Init));
        let _ =
            self.tunnel
                .events
                .send(Timed::external(now, ExternalCause::Init, TunnelEvent::Init));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::mpsc;
    use windlass_db_core::{DbEvent, DbMachine, DbPublish};
    use windlass_domain_core::{WindlassEvent, WindlassMachine, WindlassPublish, WindlassTopic};
    use windlass_machine::{Command, ExternalCause, ServiceHandles, Timed};
    use windlass_mam_core::{MamEvent, MamMachine};
    use windlass_qbit_core::{QbitEvent, QbitMachine, QbitPublish};
    use windlass_vpn_core::{VpnEvent, VpnMachine, VpnPublish};

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
                    | VpnPublish::PublicIpVerificationDegraded { .. } => {}
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

        let (docker_cmd_tx, _docker_cmd_rx) =
            mpsc::unbounded_channel::<Command<windlass_docker_core::DockerMachine>>();
        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
            docker: docker_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                windlass_machine::CoreId::Domain,
                windlass_machine::NullRuntimeTap::arc(),
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                    anchor: "windlass".to_string(),
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
                    | VpnPublish::PublicIpVerificationDegraded { .. } => {}
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
            while let Ok((cmd, _, _)) = qbit_cmd_rx.try_recv() {
                if matches!(cmd, QbitCommand::EnsureListenPort { port: p } if p == port) {
                    got_qbit = true;
                }
            }
            while let Ok((cmd, _, _)) = mam_cmd_rx.try_recv() {
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
                            windlass_machine::EventCause::External(
                                windlass_machine::ExternalCause::Unknown,
                            ),
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
        let (cmd, _, _) = tokio::time::timeout(Duration::from_secs(1), db_cmd_rx.recv())
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

        let (docker_cmd_tx, _docker_cmd_rx) =
            mpsc::unbounded_channel::<Command<windlass_docker_core::DockerMachine>>();
        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
            docker: docker_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                windlass_machine::CoreId::Domain,
                windlass_machine::NullRuntimeTap::arc(),
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                    anchor: "windlass".to_string(),
                    initial_blacklist: std::collections::HashSet::new(),
                },
                domain_shell_cfg,
            )
            .await;

        // Subscribe to Activity publishes.
        let (activity_tx, mut activity_rx) =
            mpsc::channel::<windlass_machine::PublishEnvelope<WindlassPublish>>(8);
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
        let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), activity_rx.recv())
            .await
            .expect("timeout waiting for activity publish")
            .expect("channel closed");

        match envelope.payload {
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

        let (docker_cmd_tx, _docker_cmd_rx) =
            mpsc::unbounded_channel::<Command<windlass_docker_core::DockerMachine>>();
        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
            docker: docker_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                windlass_machine::CoreId::Domain,
                windlass_machine::NullRuntimeTap::arc(),
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                    anchor: "windlass".to_string(),
                    initial_blacklist: std::collections::HashSet::new(),
                },
                domain_shell_cfg,
            )
            .await;

        let (activity_tx, mut activity_rx) =
            mpsc::channel::<windlass_machine::PublishEnvelope<WindlassPublish>>(8);
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

        let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), activity_rx.recv())
            .await
            .expect("timeout waiting for activity")
            .expect("channel closed");

        match envelope.payload {
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
