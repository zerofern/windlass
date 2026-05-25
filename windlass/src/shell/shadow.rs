use std::time::{Duration, Instant};

use windlass_core::events::Event;
use windlass_db_core::DbCommand;
use windlass_domain_core::{WindlassConfig, WindlassEvent, WindlassMachine};
use windlass_machine::Machine;
use windlass_mam_core::{MamAction, MamConfig, MamEvent, MamMachine, MamPublish};
use windlass_qbit_core::{QbitAction, QbitConfig, QbitEvent, QbitMachine, QbitPublish};
use windlass_types::{MamStatus, VpnPort, WakeupId};
use windlass_vpn_core::{VpnAction, VpnConfig, VpnEvent, VpnMachine, VpnPublish};

/// Runs the new sans-I/O cores beside the legacy core without executing actions.
///
/// This is a migration bridge: it lets the runtime feed real events into the new
/// service/domain boundaries and compare behavior in tests before those cores
/// become authoritative.
pub(super) struct ShadowCores {
    domain: WindlassMachine,
    vpn: VpnMachine,
    qbit: QbitMachine,
    mam: MamMachine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShadowAction {
    Db(DbCommand),
    Vpn(VpnAction),
    Qbit(QbitAction),
    Mam(MamAction),
    ScheduleTimer {
        timer: windlass_domain_core::WindlassTimer,
        after: Duration,
    },
}

impl ShadowCores {
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

    pub fn observe(&mut self, event: &Event) -> Vec<ShadowAction> {
        let now = Instant::now();
        let mut actions = Vec::new();
        for event in legacy_to_shadow_events(event, self.domain.state().forwarded_port) {
            actions.extend(self.apply(now, event));
        }
        dedup_shadow_actions(&mut actions);
        suppress_bootstrap_probe_actions(event, &mut actions);
        actions
    }

    #[cfg(test)]
    const fn state(&self) -> &windlass_domain_core::SystemStateView {
        self.domain.state()
    }

    fn apply(&mut self, now: Instant, event: ShadowEvent) -> Vec<ShadowAction> {
        match event {
            ShadowEvent::Domain(event) => {
                let outcome = self.domain.handle(now, event);
                self.actions_from_domain_actions(now, outcome.actions)
            }
            ShadowEvent::Vpn(event) => {
                let outcome = self.vpn.handle(now, event);
                let mut actions: Vec<ShadowAction> =
                    outcome.actions.into_iter().map(ShadowAction::Vpn).collect();
                for publish in outcome.publish {
                    actions.extend(self.publish_vpn(now, publish));
                }
                actions
            }
            ShadowEvent::Qbit(event) => {
                let outcome = self.qbit.handle(now, event);
                let mut actions: Vec<ShadowAction> = outcome
                    .actions
                    .into_iter()
                    .map(ShadowAction::Qbit)
                    .collect();
                for publish in outcome.publish {
                    actions.extend(self.publish_qbit(now, publish));
                }
                actions
            }
            ShadowEvent::Mam(event) => {
                let outcome = self.mam.handle(now, event);
                let mut actions: Vec<ShadowAction> =
                    outcome.actions.into_iter().map(ShadowAction::Mam).collect();
                for publish in outcome.publish {
                    actions.extend(self.publish_mam(now, publish));
                }
                actions
            }
        }
    }

    fn publish_vpn(&mut self, now: Instant, publish: VpnPublish) -> Vec<ShadowAction> {
        let outcome = self.domain.handle(now, WindlassEvent::Vpn(publish));
        self.actions_from_domain_actions(now, outcome.actions)
    }

    fn publish_qbit(&mut self, now: Instant, publish: QbitPublish) -> Vec<ShadowAction> {
        let outcome = self.domain.handle(now, WindlassEvent::Qbit(publish));
        self.actions_from_domain_actions(now, outcome.actions)
    }

    fn publish_mam(&mut self, now: Instant, publish: MamPublish) -> Vec<ShadowAction> {
        let outcome = self.domain.handle(now, WindlassEvent::Mam(publish));
        self.actions_from_domain_actions(now, outcome.actions)
    }

    fn actions_from_domain_actions(
        &mut self,
        now: Instant,
        actions: Vec<windlass_domain_core::WindlassAction>,
    ) -> Vec<ShadowAction> {
        let mut shadow_actions = Vec::new();
        for action in actions {
            match action {
                windlass_domain_core::WindlassAction::Db(command) => {
                    shadow_actions.push(ShadowAction::Db(command));
                }
                windlass_domain_core::WindlassAction::Vpn(command) => {
                    let outcome = self.vpn.handle_command(now, command);
                    shadow_actions.extend(outcome.actions.into_iter().map(ShadowAction::Vpn));
                    for publish in outcome.publish {
                        shadow_actions.extend(self.publish_vpn(now, publish));
                    }
                }
                windlass_domain_core::WindlassAction::Qbit(command) => {
                    let outcome = self.qbit.handle_command(now, command);
                    shadow_actions.extend(outcome.actions.into_iter().map(ShadowAction::Qbit));
                    for publish in outcome.publish {
                        shadow_actions.extend(self.publish_qbit(now, publish));
                    }
                }
                windlass_domain_core::WindlassAction::Mam(command) => {
                    let outcome = self.mam.handle_command(now, command);
                    shadow_actions.extend(outcome.actions.into_iter().map(ShadowAction::Mam));
                    for publish in outcome.publish {
                        shadow_actions.extend(self.publish_mam(now, publish));
                    }
                }
                windlass_domain_core::WindlassAction::ScheduleTimer { timer, after } => {
                    shadow_actions.push(ShadowAction::ScheduleTimer { timer, after });
                }
            }
        }
        shadow_actions
    }
}

fn dedup_shadow_actions(actions: &mut Vec<ShadowAction>) {
    let mut deduped = Vec::with_capacity(actions.len());
    for action in actions.drain(..) {
        if !deduped.contains(&action) {
            deduped.push(action);
        }
    }
    *actions = deduped;
}

fn suppress_bootstrap_probe_actions(event: &Event, actions: &mut Vec<ShadowAction>) {
    if !matches!(event, Event::Init { .. }) {
        return;
    }
    actions.retain(|action| {
        !matches!(
            action,
            ShadowAction::Mam(MamAction::FetchStatus)
                | ShadowAction::Vpn(VpnAction::InspectContainer | VpnAction::ReadPortFiles)
        )
    });
}

enum ShadowEvent {
    Domain(WindlassEvent),
    Vpn(VpnEvent),
    Qbit(QbitEvent),
    Mam(MamEvent),
}

#[allow(clippy::too_many_lines)]
fn legacy_to_shadow_events(event: &Event, forwarded_port: Option<VpnPort>) -> Vec<ShadowEvent> {
    match event {
        Event::Init {
            is_gluetun_healthy,
            port_files,
            ..
        } => {
            let mut events = vec![
                ShadowEvent::Domain(WindlassEvent::Init),
                ShadowEvent::Vpn(VpnEvent::Init),
                ShadowEvent::Qbit(QbitEvent::Init),
                ShadowEvent::Mam(MamEvent::Init),
            ];
            if *is_gluetun_healthy {
                events.push(ShadowEvent::Vpn(VpnEvent::ContainerHealthy));
            }
            if let Ok((_, port)) = port_files {
                events.push(ShadowEvent::Vpn(VpnEvent::PortFileChanged { port: *port }));
            }
            events
        }
        Event::DockerGluetunHealthy { .. } => vec![ShadowEvent::Vpn(VpnEvent::ContainerHealthy)],
        Event::DockerGluetunDied { .. } => vec![ShadowEvent::Vpn(VpnEvent::ContainerUnhealthy)],
        Event::PortFileReadResult { result, .. } => result.as_ref().map_or_else(
            |_| {
                vec![ShadowEvent::Vpn(VpnEvent::StateReadFailed {
                    reason: "port files unavailable".to_string(),
                })]
            },
            |(_, port)| vec![ShadowEvent::Vpn(VpnEvent::PortFileChanged { port: *port })],
        ),
        Event::QbitAuthSuccess { cookie, .. } => {
            vec![ShadowEvent::Qbit(QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            })]
        }
        Event::QbitAuthFailed { .. } => vec![ShadowEvent::Qbit(QbitEvent::AuthFailed {
            reason: "qBittorrent rejected credentials".to_string(),
        })],
        Event::QbitConnectionRefused { .. } => vec![ShadowEvent::Qbit(QbitEvent::AuthFailed {
            reason: "qBittorrent connection refused".to_string(),
        })],
        Event::QbitApiError { code, .. } => vec![ShadowEvent::Qbit(QbitEvent::AuthFailed {
            reason: format!("qBittorrent API error {}", code.0),
        })],
        Event::QbitPortSyncSuccess { .. } => forwarded_port.map_or_else(Vec::new, |port| {
            vec![ShadowEvent::Qbit(QbitEvent::ListenPortSet { port })]
        }),
        Event::QbitPortSyncFailed { code, .. } => forwarded_port.map_or_else(
            || {
                vec![ShadowEvent::Qbit(QbitEvent::PreferencesFailed {
                    reason: format!("qBittorrent port sync failed {}", code.0),
                })]
            },
            |port| {
                vec![ShadowEvent::Qbit(QbitEvent::ListenPortSetFailed {
                    port,
                    reason: format!("qBittorrent port sync failed {}", code.0),
                })]
            },
        ),
        Event::MamUpdateSuccess { .. } => forwarded_port.map_or_else(Vec::new, |port| {
            vec![ShadowEvent::Mam(MamEvent::SeedboxUpdated { port })]
        }),
        Event::MamAsnMismatch { ip, .. } => vec![ShadowEvent::Mam(MamEvent::StatusFailed {
            reason: format!("MAM ASN mismatch for {}", ip.0),
        })],
        Event::MamStatusObserved { status, .. } => match status {
            MamStatus::Connectable => vec![
                ShadowEvent::Mam(MamEvent::AuthSucceeded),
                ShadowEvent::Mam(MamEvent::StatusFetched {
                    connectable: true,
                    seedbox_port: forwarded_port,
                }),
            ],
            MamStatus::NotConnectable => vec![ShadowEvent::Mam(MamEvent::StatusFetched {
                connectable: false,
                seedbox_port: forwarded_port,
            })],
            MamStatus::Unreachable => vec![ShadowEvent::Mam(MamEvent::StatusFailed {
                reason: "MAM unreachable".to_string(),
            })],
        },
        Event::MamRateLimitViolation { .. } => vec![ShadowEvent::Mam(MamEvent::RateLimited {
            retry_after: Duration::from_secs(1),
        })],
        Event::QbitTorrentDetailsReceived { torrents, .. } => {
            vec![ShadowEvent::Qbit(QbitEvent::TorrentsListed {
                hashes: torrents
                    .iter()
                    .map(|torrent| torrent.hash.clone())
                    .collect(),
            })]
        }
        Event::Wakeup { id, .. } => match id {
            WakeupId::QbitAuthRetry => vec![ShadowEvent::Qbit(QbitEvent::TimerFired(
                windlass_qbit_core::QbitTimer::AuthRetry,
            ))],
            WakeupId::QbitSyncRetry => vec![ShadowEvent::Qbit(QbitEvent::TimerFired(
                windlass_qbit_core::QbitTimer::SyncRetry,
            ))],
            WakeupId::Heartbeat => vec![ShadowEvent::Mam(MamEvent::TimerFired(
                windlass_mam_core::MamTimer::StatusRetry,
            ))],
            WakeupId::RetryPortRead => vec![ShadowEvent::Vpn(VpnEvent::TimerFired(
                windlass_vpn_core::VpnTimer::PortReadRetry,
            ))],
            WakeupId::DiskCheck | WakeupId::TorrentCheck | WakeupId::CompliancePoll => Vec::new(),
        },
        Event::QbitPreferencesReceived { listen_port, .. } => {
            vec![ShadowEvent::Qbit(QbitEvent::PreferencesRead {
                listen_port: *listen_port,
            })]
        }
        Event::QbitPreferencesFailed { reason, .. } => {
            vec![ShadowEvent::Qbit(QbitEvent::PreferencesFailed {
                reason: reason.clone(),
            })]
        }
        Event::DiskSpaceObserved { .. }
        | Event::NewTorrentsObserved { .. }
        | Event::LogsDumped { .. }
        | Event::DeleteTorrentRequested { .. }
        | Event::ManualDownloadRequested { .. }
        | Event::TorrentAddedToQbit { .. }
        | Event::TorrentAddFailed { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use std::net::Ipv4Addr;

    use windlass_mam_core::MamAction;
    use windlass_qbit_core::QbitAction;
    use windlass_types::{AuthCookie, MamStatus, VpnIp, VpnPort};
    use windlass_vpn_core::VpnAction;

    use windlass_core::events::Event;
    use windlass_domain_core::ServiceStatus;

    use super::{ShadowAction, ShadowCores};

    fn port() -> VpnPort {
        VpnPort::try_new(51_820).unwrap()
    }

    #[test]
    fn init_with_healthy_vpn_updates_domain_port() {
        let mut cores = ShadowCores::new(std::time::Duration::from_secs(60));

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
        let mut cores = ShadowCores::new(std::time::Duration::from_secs(60));

        let actions = cores.observe(&Event::Init {
            at: Utc::now(),
            is_gluetun_healthy: true,
            port_files: Ok((VpnIp(Ipv4Addr::new(10, 8, 0, 1)), port())),
        });

        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ShadowAction::Qbit(QbitAction::Login)))
                .count(),
            1
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ShadowAction::Mam(MamAction::FetchStatus)))
                .count(),
            0
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ShadowAction::Vpn(VpnAction::ReadPortFiles)))
                .count(),
            0
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, ShadowAction::Vpn(VpnAction::InspectContainer)))
                .count(),
            0
        );
    }

    #[test]
    fn qbit_and_mam_events_update_domain_services() {
        let mut cores = ShadowCores::new(std::time::Duration::from_secs(60));

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
    fn forwarded_port_becomes_service_actions() {
        let mut cores = ShadowCores::new(std::time::Duration::from_secs(60));
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
            actions.contains(&ShadowAction::Qbit(QbitAction::SetListenPort {
                cookie,
                port: port(),
            }))
        );
        assert!(
            actions.contains(&ShadowAction::Mam(MamAction::UpdateSeedboxPort {
                port: port(),
            }))
        );
    }
}
