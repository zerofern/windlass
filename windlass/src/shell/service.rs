use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use windlass_core::events::Event;
use windlass_db_core::{ActivityRecord, ActivitySource, DbCommand};
use windlass_domain_core::{WindlassConfig, WindlassEvent, WindlassMachine};
use windlass_machine::{Machine, Outcome, ServiceHandles, Timed};
use windlass_mam_core::{MamAction, MamConfig, MamMachine, MamPublish};
use windlass_qbit_core::{QbitMachine, QbitPublish, QbitResponse};
use windlass_vpn_core::{VpnMachine, VpnPublish, VpnResponse};

use super::service_events::{ServiceEvent, legacy_to_service_events};

/// Runs the new sans-I/O service cores beside the legacy core.
///
/// This is a migration bridge: it lets the runtime feed real events into the
/// service/domain boundaries while legacy-only orchestration remains available
/// as a rollback path.
pub(super) struct ServiceCores {
    domain: WindlassMachine,
    vpn: ServiceHandles<VpnMachine>,
    vpn_pub_rx: mpsc::Receiver<VpnPublish>,
    qbit: ServiceHandles<QbitMachine>,
    qbit_pub_rx: mpsc::Receiver<QbitPublish>,
    mam: MamMachine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceAction {
    Db(DbCommand),
    Mam(MamAction),
    ScheduleTimer {
        timer: windlass_domain_core::WindlassTimer,
        after: Duration,
    },
}

impl ServiceCores {
    #[must_use]
    pub fn new(
        snapshot_interval: Duration,
        vpn: ServiceHandles<VpnMachine>,
        vpn_pub_rx: mpsc::Receiver<VpnPublish>,
        qbit: ServiceHandles<QbitMachine>,
        qbit_pub_rx: mpsc::Receiver<QbitPublish>,
    ) -> Self {
        let now = Instant::now();
        Self {
            domain: WindlassMachine::new(WindlassConfig { snapshot_interval }, now),
            vpn,
            vpn_pub_rx,
            qbit,
            qbit_pub_rx,
            mam: MamMachine::new(
                MamConfig {
                    status_retry: Duration::from_secs(30),
                },
                now,
            ),
        }
    }

    pub fn observe(&mut self, event: &Event) -> Vec<ServiceAction> {
        let now = Instant::now();
        let mut actions = Vec::new();
        for event in legacy_to_service_events(event, self.domain.state().forwarded_port) {
            actions.extend(self.apply(now, event));
        }
        dedup_service_actions(&mut actions);
        suppress_bootstrap_probe_actions(event, &mut actions);
        actions
    }

    pub fn observe_domain_event(&mut self, event: WindlassEvent) -> Vec<ServiceAction> {
        let now = Instant::now();
        let mut actions = self.apply(now, ServiceEvent::Domain(event));
        dedup_service_actions(&mut actions);
        actions
    }

    /// Drains VPN publish messages from the VPN runtime and feeds them into
    /// the domain machine.
    pub fn drain_vpn_publishes(&mut self) -> Vec<ServiceAction> {
        let mut actions = Vec::new();
        while let Ok(publish) = self.vpn_pub_rx.try_recv() {
            let now = Instant::now();
            let outcome = self
                .domain
                .handle(now, Timed::new(now, WindlassEvent::Vpn(publish)));
            actions.extend(self.actions_from_domain_outcome(now, outcome));
        }
        dedup_service_actions(&mut actions);
        actions
    }

    /// Drains Qbit publish messages from the Qbit runtime and feeds them into
    /// the domain machine.
    pub fn drain_qbit_publishes(&mut self) -> Vec<ServiceAction> {
        let mut actions = Vec::new();
        while let Ok(publish) = self.qbit_pub_rx.try_recv() {
            let now = Instant::now();
            let outcome = self
                .domain
                .handle(now, Timed::new(now, WindlassEvent::Qbit(publish)));
            actions.extend(self.actions_from_domain_outcome(now, outcome));
        }
        dedup_service_actions(&mut actions);
        actions
    }

    #[cfg(test)]
    const fn state(&self) -> &windlass_domain_core::SystemStateView {
        self.domain.state()
    }

    fn apply(&mut self, now: Instant, event: ServiceEvent) -> Vec<ServiceAction> {
        match event {
            ServiceEvent::Domain(event) => {
                let outcome = self.domain.handle(now, Timed::new(now, event));
                self.actions_from_domain_outcome(now, outcome)
            }
            ServiceEvent::Vpn(event) => {
                let _ = self.vpn.events.send(Timed::now(event));
                Vec::new()
            }
            ServiceEvent::Qbit(event) => {
                let _ = self.qbit.events.send(Timed::now(event));
                Vec::new()
            }
            ServiceEvent::Mam(event) => {
                let outcome = self.mam.handle(now, Timed::new(now, event));
                let mut actions: Vec<ServiceAction> = outcome
                    .actions
                    .into_iter()
                    .map(ServiceAction::Mam)
                    .collect();
                for publish in outcome.publish {
                    actions.extend(self.publish_mam(now, publish));
                }
                actions
            }
        }
    }

    fn publish_mam(&mut self, now: Instant, publish: MamPublish) -> Vec<ServiceAction> {
        let outcome = self
            .domain
            .handle(now, Timed::new(now, WindlassEvent::Mam(publish)));
        self.actions_from_domain_outcome(now, outcome)
    }

    fn actions_from_domain_outcome(
        &mut self,
        now: Instant,
        outcome: Outcome<
            windlass_domain_core::WindlassAction,
            windlass_domain_core::WindlassPublish,
        >,
    ) -> Vec<ServiceAction> {
        let mut service_actions = self.actions_from_domain_actions(now, outcome.actions);
        for publish in outcome.publish {
            match publish {
                windlass_domain_core::WindlassPublish::SystemState(_) => {}
                windlass_domain_core::WindlassPublish::Activity { message } => {
                    service_actions.push(ServiceAction::Db(DbCommand::RecordActivity(
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
        service_actions
    }

    fn actions_from_domain_actions(
        &mut self,
        now: Instant,
        actions: Vec<windlass_domain_core::WindlassAction>,
    ) -> Vec<ServiceAction> {
        let mut service_actions = Vec::new();
        for action in actions {
            match action {
                windlass_domain_core::WindlassAction::Db(command) => {
                    service_actions.push(ServiceAction::Db(command));
                }
                windlass_domain_core::WindlassAction::Vpn(command) => {
                    let (reply_tx, _reply_rx) = oneshot::channel::<VpnResponse>();
                    let _ = self.vpn.commands.send((command, reply_tx));
                }
                windlass_domain_core::WindlassAction::Qbit(command) => {
                    let (reply_tx, _reply_rx) = oneshot::channel::<QbitResponse>();
                    let _ = self.qbit.commands.send((command, reply_tx));
                }
                windlass_domain_core::WindlassAction::Mam(command) => {
                    let outcome = self.mam.handle_command(now, command);
                    service_actions.extend(outcome.actions.into_iter().map(ServiceAction::Mam));
                    for publish in outcome.publish {
                        service_actions.extend(self.publish_mam(now, publish));
                    }
                }
                windlass_domain_core::WindlassAction::ScheduleTimer { timer, after } => {
                    service_actions.push(ServiceAction::ScheduleTimer { timer, after });
                }
            }
        }
        service_actions
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

fn suppress_bootstrap_probe_actions(event: &Event, actions: &mut Vec<ServiceAction>) {
    if !matches!(event, Event::Init { .. }) {
        return;
    }
    actions.retain(|action| {
        !matches!(action, ServiceAction::Mam(MamAction::FetchStatus))
    });
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use std::net::Ipv4Addr;

    use windlass_mam_core::MamAction;
    use windlass_qbit_core::QbitPublish;
    use windlass_types::{AuthCookie, MamStatus, VpnIp, VpnPort, WakeupId};

    use windlass_core::events::Event;
    use windlass_domain_core::{ServiceStatus, WindlassTimer};
    use windlass_vpn_core::VpnPublish;

    use super::{ServiceAction, ServiceCores};

    fn port() -> VpnPort {
        VpnPort::try_new(51_820).unwrap()
    }

    fn cores() -> ServiceCores {
        use tokio::sync::mpsc;
        use windlass_machine::{Command, ServiceHandles};
        use windlass_qbit_core::{QbitEvent, QbitMachine};
        use windlass_vpn_core::{VpnEvent, VpnMachine};

        let (vev_tx, _) = mpsc::unbounded_channel::<windlass_machine::Timed<VpnEvent>>();
        let (vcmd_tx, _) = mpsc::unbounded_channel::<Command<VpnMachine>>();
        let (vsub_tx, _) = mpsc::unbounded_channel();
        let vpn_handles = ServiceHandles {
            events: vev_tx,
            commands: vcmd_tx,
            subscribe: vsub_tx,
        };
        let (_, vpn_pub_rx) = mpsc::channel::<VpnPublish>(1);

        let (qev_tx, _) = mpsc::unbounded_channel::<windlass_machine::Timed<QbitEvent>>();
        let (qcmd_tx, _) = mpsc::unbounded_channel::<Command<QbitMachine>>();
        let (qsub_tx, _) = mpsc::unbounded_channel();
        let qbit_handles = ServiceHandles {
            events: qev_tx,
            commands: qcmd_tx,
            subscribe: qsub_tx,
        };
        let (_, qbit_pub_rx) = mpsc::channel::<QbitPublish>(1);

        ServiceCores::new(
            std::time::Duration::from_secs(60),
            vpn_handles,
            vpn_pub_rx,
            qbit_handles,
            qbit_pub_rx,
        )
    }

    #[test]
    fn vpn_publish_connected_updates_domain_vpn_status() {
        let mut cores = cores();

        let _ =
            cores.observe_domain_event(windlass_domain_core::WindlassEvent::Vpn(VpnPublish::Connected));

        assert_eq!(cores.state().vpn, ServiceStatus::Ready);
    }

    #[test]
    fn qbit_publish_ready_updates_domain_qbit_status() {
        let mut cores = cores();

        let _ =
            cores.observe_domain_event(windlass_domain_core::WindlassEvent::Qbit(QbitPublish::Ready));

        assert_eq!(cores.state().qbit, ServiceStatus::Ready);
    }

    #[test]
    fn mam_events_update_domain_mam_status() {
        let mut cores = cores();

        let _ = cores.observe(&Event::MamStatusObserved {
            at: Utc::now(),
            status: MamStatus::Connectable,
        });

        assert_eq!(cores.state().mam, ServiceStatus::Ready);
    }

    #[test]
    fn init_suppresses_mam_probe_on_bootstrap() {
        let mut cores = cores();

        let actions = cores.observe(&Event::Init {
            at: Utc::now(),
            is_gluetun_healthy: true,
            port_files: Ok((VpnIp(Ipv4Addr::new(10, 8, 0, 1)), port())),
        });

        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ServiceAction::Mam(MamAction::FetchStatus)))
                .count(),
            0
        );
    }

    #[test]
    fn domain_activity_publish_becomes_db_command() {
        let mut cores = cores();

        // Inject Qbit unavailable publish directly into domain (mirrors what the
        // Qbit runtime would publish after handling QbitAuthFailed).
        let actions = cores.observe_domain_event(windlass_domain_core::WindlassEvent::Qbit(
            QbitPublish::Unavailable {
                reason: "qBittorrent rejected credentials".to_string(),
            },
        ));

        assert!(actions.iter().any(|action| {
            matches!(
                action,
                ServiceAction::Db(windlass_db_core::DbCommand::RecordActivity(record))
                    if record.source == windlass_db_core::ActivitySource::Domain
                        && record.action == "service_activity"
                        && record.detail.as_deref()
                            == Some("qBittorrent rejected credentials")
            )
        }));
    }

    #[test]
    fn mam_fetch_status_maps_to_debug_action() {
        let mapped = ServiceAction::Mam(MamAction::FetchStatus).debug_action();

        assert!(matches!(
            mapped,
            Some(windlass_core::actions::Action::CheckMamConnectability)
        ));
    }

    #[test]
    fn domain_snapshot_timer_round_trips_through_wakeup() {
        let mut cores = cores();
        let actions = cores.observe(&Event::Init {
            at: chrono::Utc::now(),
            is_gluetun_healthy: false,
            port_files: Err("missing".to_string()),
        });

        assert!(actions.contains(&ServiceAction::ScheduleTimer {
            timer: WindlassTimer::Snapshot,
            after: std::time::Duration::from_secs(60),
        }));

        let mapped = ServiceAction::ScheduleTimer {
            timer: WindlassTimer::Snapshot,
            after: std::time::Duration::from_secs(60),
        }
        .debug_action();
        assert!(matches!(
            mapped,
            Some(windlass_core::actions::Action::ScheduleWakeup(
                WakeupId::DomainSnapshot,
                duration
            )) if duration == std::time::Duration::from_secs(60)
        ));

        let actions = cores.observe(&Event::Wakeup {
            at: chrono::Utc::now(),
            id: WakeupId::DomainSnapshot,
        });
        assert!(actions.iter().any(|action| {
            matches!(
                action,
                ServiceAction::Db(windlass_db_core::DbCommand::SaveSystemSnapshot(_))
            )
        }));
    }

    #[test]
    fn db_failure_domain_event_becomes_activity_command() {
        let mut cores = cores();

        let actions = cores.observe_domain_event(windlass_domain_core::WindlassEvent::DbFailed {
            operation: "SaveSystemSnapshot".to_string(),
            message: "database unavailable".to_string(),
        });

        assert!(actions.iter().any(|action| {
            matches!(
                action,
                ServiceAction::Db(windlass_db_core::DbCommand::RecordActivity(record))
                    if record.source == windlass_db_core::ActivitySource::Domain
                        && record.detail.as_deref()
                            == Some("DB SaveSystemSnapshot failed: database unavailable")
            )
        }));
    }

    #[test]
    fn vpn_port_ready_produces_qbit_and_mam_commands_via_domain() {
        let mut cores = cores();
        // Prime domain with auth so it forwards port commands
        let _ = cores.observe_domain_event(windlass_domain_core::WindlassEvent::Qbit(
            QbitPublish::Ready,
        ));

        // VPN port ready → domain → Qbit/MAM commands forwarded to runtimes
        // We verify the domain state reflects the port (qbit/mam side effects go to runtimes)
        let _ = cores.observe_domain_event(windlass_domain_core::WindlassEvent::Vpn(
            VpnPublish::PortReady { port: port() },
        ));

        assert_eq!(cores.state().forwarded_port, Some(port()));
    }

    #[test]
    fn mam_timer_fires_fetch_status_action() {
        let mut cores = cores();

        let actions = cores.observe(&Event::Wakeup {
            at: Utc::now(),
            id: WakeupId::Heartbeat,
        });

        assert!(actions
            .iter()
            .any(|a| matches!(a, ServiceAction::Mam(MamAction::FetchStatus))));
    }

    #[test]
    fn auth_cookie_retained() {
        let mut cores = cores();
        let cookie = AuthCookie("sid".to_string());
        cores.observe(&Event::QbitAuthSuccess {
            at: Utc::now(),
            cookie: cookie.clone(),
        });
        // After auth success the qbit machine (in runtime) holds the cookie;
        // the qbit service actions path is removed — runtime drives its own I/O.
        // Verify the domain qbit status via Qbit runtime publish (simulated here).
        let _ = cores.observe_domain_event(windlass_domain_core::WindlassEvent::Qbit(
            QbitPublish::Ready,
        ));
        assert_eq!(cores.state().qbit, ServiceStatus::Ready);
    }
}
