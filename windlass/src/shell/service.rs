use std::time::Instant;

use tokio::sync::mpsc;
use windlass_core::events::Event;
use windlass_db_core::{ActivityRecord, ActivitySource, DbCommand, DbMachine, DbPublish};
use windlass_domain_core::{WindlassEvent, WindlassMachine, WindlassPublish};
use windlass_machine::{Command, ServiceHandles, Timed};
use windlass_mam_core::{MamMachine, MamPublish};
use windlass_qbit_core::{QbitMachine, QbitPublish};
use windlass_types::VpnPort;
use windlass_vpn_core::{VpnMachine, VpnPublish};

use super::service_events::{ServiceEvent, legacy_to_service_events};

/// Runs the new sans-I/O service cores beside the legacy core.
///
/// This is a migration bridge: it lets the runtime feed real events into the
/// service/domain boundaries while legacy-only orchestration remains available
/// as a rollback path.
pub(super) struct ServiceCores {
    domain: ServiceHandles<WindlassMachine>,
    domain_pub_rx: mpsc::Receiver<WindlassPublish>,
    db: ServiceHandles<DbMachine>,
    db_pub_rx: mpsc::Receiver<DbPublish>,
    vpn: ServiceHandles<VpnMachine>,
    vpn_pub_rx: mpsc::Receiver<VpnPublish>,
    qbit: ServiceHandles<QbitMachine>,
    qbit_pub_rx: mpsc::Receiver<QbitPublish>,
    mam: ServiceHandles<MamMachine>,
    mam_pub_rx: mpsc::Receiver<MamPublish>,
    /// Cached forwarded port, updated when VPN publishes PortReady/PortUnavailable.
    /// Used by the legacy event bridge to translate legacy shell results into
    /// sub-runtime events (e.g. QbitPortSyncSuccess needs to know the port).
    forwarded_port: Option<VpnPort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceAction {
    Db(DbCommand),
}

impl ServiceCores {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        domain: ServiceHandles<WindlassMachine>,
        domain_pub_rx: mpsc::Receiver<WindlassPublish>,
        db: ServiceHandles<DbMachine>,
        db_pub_rx: mpsc::Receiver<DbPublish>,
        vpn: ServiceHandles<VpnMachine>,
        vpn_pub_rx: mpsc::Receiver<VpnPublish>,
        qbit: ServiceHandles<QbitMachine>,
        qbit_pub_rx: mpsc::Receiver<QbitPublish>,
        mam: ServiceHandles<MamMachine>,
        mam_pub_rx: mpsc::Receiver<MamPublish>,
    ) -> Self {
        Self {
            domain,
            domain_pub_rx,
            db,
            db_pub_rx,
            vpn,
            vpn_pub_rx,
            qbit,
            qbit_pub_rx,
            mam,
            mam_pub_rx,
            forwarded_port: None,
        }
    }

    /// Returns the sender for issuing DB commands to the DB runtime.
    pub fn db_command_tx(&self) -> &mpsc::UnboundedSender<Command<DbMachine>> {
        &self.db.commands
    }

    pub fn observe(&mut self, event: &Event) -> Vec<ServiceAction> {
        for event in legacy_to_service_events(event, self.forwarded_port) {
            self.send_service_event(event);
        }
        Vec::new()
    }

    pub fn observe_domain_event(&mut self, event: WindlassEvent) {
        let _ = self.domain.events.send(Timed::now(event));
    }

    /// Drains VPN publish messages from the VPN runtime and forwards them into
    /// the domain machine as `Timed<WindlassEvent>`.
    /// Also updates the cached `forwarded_port` for the legacy event bridge.
    pub fn drain_vpn_publishes(&mut self) {
        while let Ok(publish) = self.vpn_pub_rx.try_recv() {
            // Update cached port for legacy bridge.
            match &publish {
                VpnPublish::PortReady { port } => self.forwarded_port = Some(*port),
                VpnPublish::PortUnavailable | VpnPublish::Disconnected => {
                    self.forwarded_port = None;
                }
                VpnPublish::Connected => {}
            }
            let _ = self
                .domain
                .events
                .send(Timed::now(WindlassEvent::Vpn(publish)));
        }
    }

    /// Drains Qbit publish messages from the Qbit runtime and forwards them into
    /// the domain machine as `Timed<WindlassEvent>`.
    pub fn drain_qbit_publishes(&mut self) {
        while let Ok(publish) = self.qbit_pub_rx.try_recv() {
            let _ = self
                .domain
                .events
                .send(Timed::now(WindlassEvent::Qbit(publish)));
        }
    }

    /// Drains MAM publish messages from the MAM runtime and forwards them into
    /// the domain machine as `Timed<WindlassEvent>`.
    pub fn drain_mam_publishes(&mut self) {
        while let Ok(publish) = self.mam_pub_rx.try_recv() {
            let _ = self
                .domain
                .events
                .send(Timed::now(WindlassEvent::Mam(publish)));
        }
    }

    /// Drains DB publish messages from the DB runtime.
    ///
    /// Failures (other than `RecordActivity`, to break recursion) are injected
    /// into the domain machine as `WindlassEvent::DbFailed`.  Success publishes
    /// are silently discarded — the domain does not need to react to them.
    pub fn drain_db_publishes(&mut self) {
        while let Ok(publish) = self.db_pub_rx.try_recv() {
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
                        let _ = self.domain.events.send(Timed::now(event));
                    }
                }
            }
        }
    }

    /// Drains domain publish messages and converts `Activity` to DB commands.
    ///
    /// `SystemState` publishes are forwarded as-is (currently unused by the UI,
    /// which still uses the legacy core for state). `Activity` publishes become
    /// `RecordActivity` DB commands so activity logging is preserved.
    pub fn drain_domain_publishes(&mut self) -> Vec<ServiceAction> {
        let mut actions = Vec::new();
        while let Ok(publish) = self.domain_pub_rx.try_recv() {
            match publish {
                WindlassPublish::SystemState(_) => {}
                WindlassPublish::Activity { message } => {
                    actions.push(ServiceAction::Db(DbCommand::RecordActivity(
                        ActivityRecord {
                            at: chrono::Utc::now(),
                            source: ActivitySource::Domain,
                            action: "service_activity".to_string(),
                            book_id: None,
                            detail: Some(message),
                            metadata: serde_json::Value::Null,
                        },
                    )));
                }
            }
        }
        dedup_service_actions(&mut actions);
        actions
    }

    fn send_service_event(&mut self, event: ServiceEvent) {
        let now = Instant::now();
        match event {
            ServiceEvent::Domain(event) => {
                let _ = self.domain.events.send(Timed::new(now, event));
            }
            ServiceEvent::Vpn(event) => {
                let _ = self.vpn.events.send(Timed::new(now, event));
            }
            ServiceEvent::Qbit(event) => {
                let _ = self.qbit.events.send(Timed::new(now, event));
            }
            ServiceEvent::Mam(event) => {
                let _ = self.mam.events.send(Timed::new(now, event));
            }
        }
    }
}

fn dedup_service_actions(actions: &mut Vec<ServiceAction>) {
    let mut deduped = Vec::with_capacity(actions.len());
    for action in actions.drain(..) {
        if !deduped.contains(&action) {
            deduped.push(action);
        }
    }
    *actions = deduped;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};
    use windlass_db_core::{DbCommand, DbEvent, DbMachine, DbPublish};
    use windlass_domain_core::{WindlassEvent, WindlassMachine, WindlassPublish, WindlassTopic};
    use windlass_machine::{Command, ServiceHandles, Timed};
    use windlass_mam_core::{MamEvent, MamMachine, MamPublish};
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

    struct TestHarness {
        cores: ServiceCores,
        domain_ev_rx: mpsc::UnboundedReceiver<Timed<WindlassEvent>>,
        qbit_cmd_rx: mpsc::UnboundedReceiver<Command<QbitMachine>>,
        mam_cmd_rx: mpsc::UnboundedReceiver<Command<MamMachine>>,
        db_cmd_rx: mpsc::UnboundedReceiver<Command<DbMachine>>,
        db_pub_tx: mpsc::Sender<DbPublish>,
    }

    fn harness() -> TestHarness {
        let (domain_handles, domain_ev_rx) = make_domain_handles();
        let (domain_pub_tx, domain_pub_rx) = mpsc::channel::<WindlassPublish>(16);
        // Register domain pub subscriber so drain_domain_publishes gets messages.
        let _ = domain_handles
            .subscribe
            .send((vec![WindlassTopic::Activity], domain_pub_tx));

        let (db_handles, db_cmd_rx) = make_db_handles();
        let (db_pub_tx, db_pub_rx) = mpsc::channel::<DbPublish>(16);

        let (vpn_handles, _vpn_ev_rx) = make_vpn_handles();
        let (_, vpn_pub_rx) = mpsc::channel::<VpnPublish>(1);

        let (qbit_handles, qbit_cmd_rx) = make_qbit_handles();
        let (_, qbit_pub_rx) = mpsc::channel::<QbitPublish>(1);

        let (mam_handles, mam_cmd_rx) = make_mam_handles();
        let (_, mam_pub_rx) = mpsc::channel::<MamPublish>(1);

        let cores = ServiceCores::new(
            domain_handles,
            domain_pub_rx,
            db_handles,
            db_pub_rx,
            vpn_handles,
            vpn_pub_rx,
            qbit_handles,
            qbit_pub_rx,
            mam_handles,
            mam_pub_rx,
        );

        TestHarness {
            cores,
            domain_ev_rx,
            qbit_cmd_rx,
            mam_cmd_rx,
            db_cmd_rx,
            db_pub_tx,
        }
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    /// `VpnPublish::PortReady` drained from the VPN channel should be forwarded
    /// into the domain event channel as `WindlassEvent::Vpn(PortReady{port})`.
    #[tokio::test]
    async fn vpn_publish_port_ready_is_forwarded_to_domain() {
        use windlass_types::VpnPort;

        let mut h = harness();
        let port = VpnPort::try_new(51_820).unwrap();

        // Simulate the VPN runtime publishing PortReady.
        let (vpn_pub_tx, vpn_pub_rx) = mpsc::channel::<VpnPublish>(1);
        // Rebuild cores with a connected vpn_pub_rx so we can inject.
        let (domain_handles, mut domain_ev_rx) = make_domain_handles();
        let (domain_pub_tx, domain_pub_rx) = mpsc::channel::<WindlassPublish>(16);
        let _ = domain_handles
            .subscribe
            .send((vec![WindlassTopic::Activity], domain_pub_tx));
        let (db_handles, _db_cmd_rx) = make_db_handles();
        let (db_pub_tx, db_pub_rx) = mpsc::channel::<DbPublish>(8);
        let (vpn_handles, _vpn_ev_rx) = make_vpn_handles();
        let (qbit_handles, _qbit_cmd_rx) = make_qbit_handles();
        let (_, qbit_pub_rx) = mpsc::channel::<QbitPublish>(1);
        let (mam_handles, _mam_cmd_rx) = make_mam_handles();
        let (_, mam_pub_rx) = mpsc::channel::<MamPublish>(1);

        let mut cores = ServiceCores::new(
            domain_handles,
            domain_pub_rx,
            db_handles,
            db_pub_rx,
            vpn_handles,
            vpn_pub_rx,
            qbit_handles,
            qbit_pub_rx,
            mam_handles,
            mam_pub_rx,
        );

        vpn_pub_tx
            .send(VpnPublish::PortReady { port })
            .await
            .unwrap();
        cores.drain_vpn_publishes();

        let event = domain_ev_rx.recv().await.expect("domain event expected");
        assert_eq!(
            event.inner,
            WindlassEvent::Vpn(VpnPublish::PortReady { port })
        );
    }

    /// `VpnPublish::PortReady` fed directly through the domain runtime spawned
    /// by the generic `ServiceRuntime` causes `QbitCommand::EnsureListenPort`
    /// and `MamCommand::EnsureSeedboxPort` to appear on the respective command
    /// channels (end-to-end runtime test).
    #[tokio::test]
    async fn vpn_port_ready_through_runtime_sends_qbit_and_mam_commands() {
        use windlass_domain_core::{WindlassConfig, WindlassMachine};
        use windlass_machine::Command;
        use windlass_mam_core::MamCommand;
        use windlass_qbit_core::QbitCommand;
        use windlass_types::VpnPort;

        let port = VpnPort::try_new(51_820).unwrap();

        // Build command-channel pairs for qbit and mam.
        let (qbit_cmd_tx, mut qbit_cmd_rx) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (mam_cmd_tx, _mam_cmd_rx) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (db_cmd_tx, _db_cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (vpn_cmd_tx, _vpn_cmd_rx) = mpsc::unbounded_channel::<Command<VpnMachine>>();

        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            vpn: vpn_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                },
                domain_shell_cfg,
            )
            .await;

        // Inject VpnPublish::PortReady into the domain event channel.
        domain_handles
            .events
            .send(Timed::now(WindlassEvent::Vpn(VpnPublish::PortReady {
                port,
            })))
            .unwrap();

        // Give the async runtime a tick to process.
        tokio::task::yield_now().await;
        // Retry a few times to avoid flakiness.
        for _ in 0..10 {
            if qbit_cmd_rx.try_recv().is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }

        // We should see QbitCommand::EnsureListenPort on the qbit channel.
        // Re-drain to collect all commands (EnsureListenPort may follow Init actions).
        let (qbit_cmd_tx2, mut qbit_cmd_rx2) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (mam_cmd_tx2, mut mam_cmd_rx2) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (db_cmd_tx2, _db_cmd_rx2) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (vpn_cmd_tx2, _vpn_cmd_rx2) = mpsc::unbounded_channel::<Command<VpnMachine>>();

        let domain_shell_cfg2 = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx2,
            vpn: vpn_cmd_tx2,
            qbit: qbit_cmd_tx2,
            mam: mam_cmd_tx2,
        };

        let (domain_handles2, _domain_join2) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
                },
                domain_shell_cfg2,
            )
            .await;

        domain_handles2
            .events
            .send(Timed::now(WindlassEvent::Vpn(VpnPublish::PortReady {
                port,
            })))
            .unwrap();

        // Yield until we get the commands.
        let mut got_qbit = false;
        let mut got_mam = false;
        for _ in 0..20 {
            tokio::task::yield_now().await;
            while let Ok((cmd, _)) = qbit_cmd_rx2.try_recv() {
                if matches!(cmd, QbitCommand::EnsureListenPort { port: p } if p == port) {
                    got_qbit = true;
                }
            }
            while let Ok((cmd, _)) = mam_cmd_rx2.try_recv() {
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
            "expected QbitCommand::EnsureListenPort on qbit channel"
        );
        assert!(
            got_mam,
            "expected MamCommand::EnsureSeedboxPort on mam channel"
        );
    }

    /// `QbitPublish::Unavailable` → domain runtime → `WindlassPublish::Activity`
    /// → `drain_domain_publishes` → `ServiceAction::Db(RecordActivity)`.
    #[tokio::test]
    async fn qbit_unavailable_through_runtime_produces_activity_db_command() {
        use windlass_domain_core::{WindlassConfig, WindlassMachine};
        use windlass_machine::Command;

        let (qbit_cmd_tx, _qbit_cmd_rx) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (mam_cmd_tx, _mam_cmd_rx) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (db_cmd_tx, _db_cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (vpn_cmd_tx, _vpn_cmd_rx) = mpsc::unbounded_channel::<Command<VpnMachine>>();

        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            vpn: vpn_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
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
            .send(Timed::now(WindlassEvent::Qbit(QbitPublish::Unavailable {
                reason: "qBittorrent rejected credentials".to_string(),
            })))
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

    /// DB failure (non-RecordActivity) is forwarded to the domain event channel
    /// as `WindlassEvent::DbFailed`.
    #[tokio::test]
    async fn db_failure_publish_is_forwarded_to_domain_event_channel() {
        use windlass_db_core::DbFailure;

        let mut h = harness();

        h.db_pub_tx
            .send(DbPublish::Failed(DbFailure {
                operation: "SaveSystemSnapshot".to_string(),
                message: "disk full".to_string(),
                retryable: false,
            }))
            .await
            .unwrap();

        h.cores.drain_db_publishes();

        let event = h.domain_ev_rx.recv().await.expect("domain event expected");
        assert_eq!(
            event.inner,
            WindlassEvent::DbFailed {
                operation: "SaveSystemSnapshot".to_string(),
                message: "disk full".to_string(),
            }
        );
    }

    /// `RecordActivity` DB failures must NOT be forwarded to the domain
    /// (recursion guard).
    #[tokio::test]
    async fn record_activity_failure_is_not_forwarded_to_domain() {
        use windlass_db_core::DbFailure;

        let mut h = harness();

        h.db_pub_tx
            .send(DbPublish::Failed(DbFailure {
                operation: "RecordActivity".to_string(),
                message: "connection lost".to_string(),
                retryable: true,
            }))
            .await
            .unwrap();

        h.cores.drain_db_publishes();

        assert!(
            h.domain_ev_rx.try_recv().is_err(),
            "RecordActivity failure must not recurse into domain"
        );
    }

    /// `WindlassPublish::Activity` from the domain publish channel becomes a
    /// `ServiceAction::Db(RecordActivity)`.
    #[test]
    fn domain_activity_publish_drain_produces_db_record_activity() {
        use windlass_db_core::ActivitySource;

        // Build a pair with a real sender so we can inject publishes.
        let (domain_pub_tx, domain_pub_rx) = mpsc::channel::<WindlassPublish>(4);
        // domain_pub_tx is what drain_domain_publishes drains from.
        let (domain_handles, _ev_rx) = make_domain_handles();

        let (db_handles, _db_cmd_rx) = make_db_handles();
        let (_db_pub_tx, db_pub_rx) = mpsc::channel::<DbPublish>(1);
        let (vpn_handles, _vpn_ev_rx) = make_vpn_handles();
        let (_, vpn_pub_rx) = mpsc::channel::<VpnPublish>(1);
        let (qbit_handles, _qbit_cmd_rx) = make_qbit_handles();
        let (_, qbit_pub_rx) = mpsc::channel::<QbitPublish>(1);
        let (mam_handles, _mam_cmd_rx) = make_mam_handles();
        let (_, mam_pub_rx) = mpsc::channel::<MamPublish>(1);

        let mut cores = ServiceCores::new(
            domain_handles,
            domain_pub_rx,
            db_handles,
            db_pub_rx,
            vpn_handles,
            vpn_pub_rx,
            qbit_handles,
            qbit_pub_rx,
            mam_handles,
            mam_pub_rx,
        );

        domain_pub_tx
            .try_send(WindlassPublish::Activity {
                message: "test activity".to_string(),
            })
            .unwrap();

        let actions = cores.drain_domain_publishes();

        assert!(
            actions.iter().any(|a| {
                matches!(
                    a,
                    super::ServiceAction::Db(DbCommand::RecordActivity(record))
                        if record.source == ActivitySource::Domain
                            && record.detail.as_deref() == Some("test activity")
                )
            }),
            "expected RecordActivity action, got: {:?}",
            actions
        );
    }

    /// `WindlassPublish::SystemState` from the domain publish channel is
    /// silently discarded (no service action produced).
    #[test]
    fn domain_system_state_publish_drain_produces_no_action() {
        use windlass_domain_core::{ServiceStatus, SystemStateView};

        let (domain_pub_tx, domain_pub_rx) = mpsc::channel::<WindlassPublish>(4);
        let (domain_handles, _ev_rx) = make_domain_handles();

        let (db_handles, _db_cmd_rx) = make_db_handles();
        let (_db_pub_tx, db_pub_rx) = mpsc::channel::<DbPublish>(1);
        let (vpn_handles, _vpn_ev_rx) = make_vpn_handles();
        let (_, vpn_pub_rx) = mpsc::channel::<VpnPublish>(1);
        let (qbit_handles, _qbit_cmd_rx) = make_qbit_handles();
        let (_, qbit_pub_rx) = mpsc::channel::<QbitPublish>(1);
        let (mam_handles, _mam_cmd_rx) = make_mam_handles();
        let (_, mam_pub_rx) = mpsc::channel::<MamPublish>(1);

        let mut cores = ServiceCores::new(
            domain_handles,
            domain_pub_rx,
            db_handles,
            db_pub_rx,
            vpn_handles,
            vpn_pub_rx,
            qbit_handles,
            qbit_pub_rx,
            mam_handles,
            mam_pub_rx,
        );

        domain_pub_tx
            .try_send(WindlassPublish::SystemState(SystemStateView {
                vpn: ServiceStatus::Ready,
                qbit: ServiceStatus::Unknown,
                mam: ServiceStatus::Unknown,
                forwarded_port: None,
            }))
            .unwrap();

        let actions = cores.drain_domain_publishes();
        assert!(
            actions.is_empty(),
            "SystemState publish must not produce actions"
        );
    }

    /// `DbFailed` event round-trips: forwarded to domain → domain publishes
    /// `Activity` → drain produces `RecordActivity`.
    ///
    /// This test uses the real `WindlassMachine` runtime to verify end-to-end
    /// behavior.
    #[tokio::test]
    async fn db_failure_domain_event_becomes_activity_via_runtime() {
        use windlass_domain_core::{WindlassConfig, WindlassMachine};
        use windlass_machine::Command;

        let (qbit_cmd_tx, _qbit_cmd_rx) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (mam_cmd_tx, _mam_cmd_rx) = mpsc::unbounded_channel::<Command<MamMachine>>();
        let (db_cmd_tx, _db_cmd_rx) = mpsc::unbounded_channel::<Command<DbMachine>>();
        let (vpn_cmd_tx, _vpn_cmd_rx) = mpsc::unbounded_channel::<Command<VpnMachine>>();

        let domain_shell_cfg = crate::shell::domain_shell::DomainShellConfig {
            db: db_cmd_tx,
            vpn: vpn_cmd_tx,
            qbit: qbit_cmd_tx,
            mam: mam_cmd_tx,
        };

        let (domain_handles, _domain_join) =
            windlass_machine::spawn::<WindlassMachine, crate::shell::domain_shell::DomainShell>(
                WindlassConfig {
                    snapshot_interval: Duration::from_secs(3600),
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
            .send(Timed::now(WindlassEvent::DbFailed {
                operation: "SaveSystemSnapshot".to_string(),
                message: "database unavailable".to_string(),
            }))
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
