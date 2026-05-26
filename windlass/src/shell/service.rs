use std::time::{Duration, Instant};

use windlass_core::events::Event;
use windlass_db_core::{ActivityRecord, ActivitySource, DbCommand};
use windlass_domain_core::{WindlassConfig, WindlassEvent, WindlassMachine};
use windlass_machine::{Machine, Outcome, Timed};
use windlass_mam_core::{MamAction, MamConfig, MamMachine, MamPublish};
use windlass_qbit_core::{QbitAction, QbitConfig, QbitMachine, QbitPublish};
use windlass_vpn_core::{VpnAction, VpnConfig, VpnMachine, VpnPublish};

use super::service_events::{ServiceEvent, legacy_to_service_events};

/// Runs the new sans-I/O service cores beside the legacy core.
///
/// This is a migration bridge: it lets the runtime feed real events into the
/// service/domain boundaries while legacy-only orchestration remains available
/// as a rollback path.
pub(super) struct ServiceCores {
    domain: WindlassMachine,
    vpn: VpnMachine,
    qbit: QbitMachine,
    mam: MamMachine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceAction {
    Db(DbCommand),
    Vpn(VpnAction),
    Qbit(QbitAction),
    Mam(MamAction),
    ScheduleTimer {
        timer: windlass_domain_core::WindlassTimer,
        after: Duration,
    },
}

impl ServiceCores {
    #[must_use]
    pub fn new(snapshot_interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            domain: WindlassMachine::new(WindlassConfig { snapshot_interval }, now),
            vpn: VpnMachine::new(
                VpnConfig {
                    health_poll_interval: Duration::from_secs(30),
                },
                now,
            ),
            qbit: QbitMachine::new(
                QbitConfig {
                    auth_retry: Duration::from_secs(5),
                    sync_retry: Duration::from_secs(2),
                },
                now,
            ),
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
                let outcome = self.vpn.handle(now, Timed::new(now, event));
                let mut actions: Vec<ServiceAction> = outcome
                    .actions
                    .into_iter()
                    .map(ServiceAction::Vpn)
                    .collect();
                for publish in outcome.publish {
                    actions.extend(self.publish_vpn(now, publish));
                }
                actions
            }
            ServiceEvent::Qbit(event) => {
                let outcome = self.qbit.handle(now, Timed::new(now, event));
                let mut actions: Vec<ServiceAction> = outcome
                    .actions
                    .into_iter()
                    .map(ServiceAction::Qbit)
                    .collect();
                for publish in outcome.publish {
                    actions.extend(self.publish_qbit(now, publish));
                }
                actions
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

    fn publish_vpn(&mut self, now: Instant, publish: VpnPublish) -> Vec<ServiceAction> {
        let outcome = self
            .domain
            .handle(now, Timed::new(now, WindlassEvent::Vpn(publish)));
        self.actions_from_domain_outcome(now, outcome)
    }

    fn publish_qbit(&mut self, now: Instant, publish: QbitPublish) -> Vec<ServiceAction> {
        let outcome = self
            .domain
            .handle(now, Timed::new(now, WindlassEvent::Qbit(publish)));
        self.actions_from_domain_outcome(now, outcome)
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
                    let outcome = self.vpn.handle_command(now, command);
                    service_actions.extend(outcome.actions.into_iter().map(ServiceAction::Vpn));
                    for publish in outcome.publish {
                        service_actions.extend(self.publish_vpn(now, publish));
                    }
                }
                windlass_domain_core::WindlassAction::Qbit(command) => {
                    let outcome = self.qbit.handle_command(now, command);
                    service_actions.extend(outcome.actions.into_iter().map(ServiceAction::Qbit));
                    for publish in outcome.publish {
                        service_actions.extend(self.publish_qbit(now, publish));
                    }
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
        !matches!(
            action,
            ServiceAction::Mam(MamAction::FetchStatus)
                | ServiceAction::Vpn(VpnAction::InspectContainer | VpnAction::ReadPortFiles)
        )
    });
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use std::net::Ipv4Addr;

    use windlass_mam_core::MamAction;
    use windlass_qbit_core::QbitAction;
    use windlass_types::{AuthCookie, MamStatus, VpnIp, VpnPort, WakeupId};
    use windlass_vpn_core::VpnAction;

    use windlass_core::events::Event;
    use windlass_domain_core::{ServiceStatus, WindlassTimer};

    use super::{ServiceAction, ServiceCores};

    fn port() -> VpnPort {
        VpnPort::try_new(51_820).unwrap()
    }

    #[test]
    fn init_with_healthy_vpn_updates_domain_port() {
        let mut cores = ServiceCores::new(std::time::Duration::from_secs(60));

        let db_commands = cores.observe(&Event::Init {
            at: Utc::now(),
            is_gluetun_healthy: true,
            port_files: Ok((VpnIp(Ipv4Addr::new(10, 8, 0, 1)), port())),
        });

        assert_eq!(cores.state().vpn, ServiceStatus::Ready);
        assert_eq!(cores.state().forwarded_port, Some(port()));
        assert!(!db_commands.is_empty());
    }

    #[test]
    fn init_deduplicates_bootstrap_service_actions_without_mam_probe() {
        let mut cores = ServiceCores::new(std::time::Duration::from_secs(60));

        let actions = cores.observe(&Event::Init {
            at: Utc::now(),
            is_gluetun_healthy: true,
            port_files: Ok((VpnIp(Ipv4Addr::new(10, 8, 0, 1)), port())),
        });

        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ServiceAction::Qbit(QbitAction::Login)))
                .count(),
            1
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ServiceAction::Mam(MamAction::FetchStatus)))
                .count(),
            0
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ServiceAction::Vpn(VpnAction::ReadPortFiles)))
                .count(),
            0
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ServiceAction::Vpn(VpnAction::InspectContainer)))
                .count(),
            0
        );
    }

    #[test]
    fn qbit_and_mam_events_update_domain_services() {
        let mut cores = ServiceCores::new(std::time::Duration::from_secs(60));

        let _ = cores.observe(&Event::Init {
            at: Utc::now(),
            is_gluetun_healthy: true,
            port_files: Ok((VpnIp(Ipv4Addr::new(10, 8, 0, 1)), port())),
        });
        let _ = cores.observe(&Event::QbitAuthSuccess {
            at: Utc::now(),
            cookie: AuthCookie("sid".to_string()),
        });
        let _ = cores.observe(&Event::MamStatusObserved {
            at: Utc::now(),
            status: MamStatus::Connectable,
        });

        assert_eq!(cores.state().qbit, ServiceStatus::Ready);
        assert_eq!(cores.state().mam, ServiceStatus::Ready);
    }

    #[test]
    fn domain_activity_publish_becomes_db_command() {
        let mut cores = ServiceCores::new(std::time::Duration::from_secs(60));

        let actions = cores.observe(&Event::QbitAuthFailed { at: Utc::now() });

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
    fn db_failure_domain_event_becomes_activity_command() {
        let mut cores = ServiceCores::new(std::time::Duration::from_secs(60));

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
    fn qbit_set_port_maps_to_debug_action() {
        let cookie = AuthCookie("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();

        let mapped = ServiceAction::Qbit(QbitAction::SetListenPort {
            cookie: cookie.clone(),
            port,
        })
        .debug_action();

        assert!(matches!(
            mapped,
            Some(windlass_core::actions::Action::SyncQbitPort(mapped_cookie, mapped_port))
                if mapped_cookie == cookie && mapped_port == port
        ));
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
        let mut cores = ServiceCores::new(std::time::Duration::from_secs(60));
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
    fn forwarded_port_becomes_service_actions() {
        let mut cores = ServiceCores::new(std::time::Duration::from_secs(60));
        let cookie = AuthCookie("sid".to_string());
        let _ = cores.observe(&Event::QbitAuthSuccess {
            at: Utc::now(),
            cookie: cookie.clone(),
        });

        let actions = cores.observe(&Event::PortFileReadResult {
            at: Utc::now(),
            result: Ok((VpnIp(Ipv4Addr::new(10, 8, 0, 1)), port())),
        });

        assert!(
            actions.contains(&ServiceAction::Qbit(QbitAction::SetListenPort {
                cookie,
                port: port(),
            }))
        );
        assert!(
            actions.contains(&ServiceAction::Mam(MamAction::UpdateSeedboxPort {
                port: port(),
            }))
        );
    }
}
