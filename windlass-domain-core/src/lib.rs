#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_db_core::DbCommand;
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_mam_core::{MamCommand, MamPublish};
use windlass_qbit_core::{QbitCommand, QbitPublish};
use windlass_types::AlertPriority;
use windlass_types::VpnPort;
use windlass_vpn_core::{VpnCommand, VpnPublish};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindlassConfig {
    pub snapshot_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassEvent {
    Init,
    Vpn(VpnPublish),
    Qbit(QbitPublish),
    Mam(MamPublish),
    DbFailed { operation: String, message: String },
    TimerFired(WindlassTimer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassTimer {
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassAction {
    Vpn(VpnCommand),
    Qbit(QbitCommand),
    Mam(MamCommand),
    Db(DbCommand),
    SaveSystemSnapshot(SystemStateView),
    SendAlert {
        priority: AlertPriority,
        title: String,
        body: String,
    },
    ScheduleTimer {
        timer: WindlassTimer,
        after: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassPublish {
    SystemState(SystemStateView),
    Activity { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassTopic {
    SystemState,
    Activity,
}

impl HasTopic<WindlassTopic> for WindlassPublish {
    fn topic(&self) -> WindlassTopic {
        match self {
            Self::SystemState(_) => WindlassTopic::SystemState,
            Self::Activity { .. } => WindlassTopic::Activity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassCommand {
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassResponse {
    Accepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceStatus {
    Unknown,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemStateView {
    pub vpn: ServiceStatus,
    pub qbit: ServiceStatus,
    pub mam: ServiceStatus,
    pub forwarded_port: Option<VpnPort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindlassMachine {
    config: WindlassConfig,
    state: SystemStateView,
}

impl WindlassMachine {
    #[must_use]
    pub const fn state(&self) -> &SystemStateView {
        &self.state
    }

    fn snapshot_action(&self) -> WindlassAction {
        WindlassAction::SaveSystemSnapshot(self.state.clone())
    }
}

impl Machine for WindlassMachine {
    type Config = WindlassConfig;
    type Event = WindlassEvent;
    type Action = WindlassAction;
    type Publish = WindlassPublish;
    type Topic = WindlassTopic;
    type Command = WindlassCommand;
    type Response = WindlassResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            state: SystemStateView {
                vpn: ServiceStatus::Unknown,
                qbit: ServiceStatus::Unknown,
                mam: ServiceStatus::Unknown,
                forwarded_port: None,
            },
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            WindlassEvent::Init => Outcome {
                actions: vec![
                    WindlassAction::Vpn(VpnCommand::StartMonitoring),
                    WindlassAction::Qbit(QbitCommand::EnsureAuthenticated),
                    WindlassAction::Mam(MamCommand::EnsureAuthenticated),
                    WindlassAction::ScheduleTimer {
                        timer: WindlassTimer::Snapshot,
                        after: self.config.snapshot_interval,
                    },
                ],
                publish: Vec::new(),
            },
            WindlassEvent::Vpn(VpnPublish::Connected) => {
                self.state.vpn = ServiceStatus::Ready;
                self.publish_state()
            }
            WindlassEvent::Vpn(VpnPublish::Disconnected) => {
                self.state.vpn = ServiceStatus::Degraded;
                self.state.forwarded_port = None;
                self.publish_state()
            }
            WindlassEvent::Vpn(VpnPublish::PortReady { port }) => {
                self.state.forwarded_port = Some(port);
                Outcome {
                    actions: vec![
                        WindlassAction::Qbit(QbitCommand::EnsureListenPort { port }),
                        WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }),
                        self.snapshot_action(),
                    ],
                    publish: vec![WindlassPublish::SystemState(self.state.clone())],
                }
            }
            WindlassEvent::Vpn(VpnPublish::PortUnavailable) => {
                self.state.forwarded_port = None;
                self.publish_state()
            }
            WindlassEvent::Qbit(QbitPublish::Ready) => {
                self.state.qbit = ServiceStatus::Ready;
                self.publish_state()
            }
            WindlassEvent::Qbit(QbitPublish::Unavailable { reason }) => {
                self.state.qbit = ServiceStatus::Degraded;
                self.publish_state_with_activity(reason)
            }
            WindlassEvent::Qbit(
                QbitPublish::ListenPortReady { .. } | QbitPublish::TorrentsUpdated { .. },
            )
            | WindlassEvent::Mam(MamPublish::Connectable { .. }) => Outcome::none(),
            WindlassEvent::Mam(MamPublish::SeedboxPortReady { port }) => Outcome {
                actions: vec![WindlassAction::SendAlert {
                    priority: AlertPriority::Info,
                    title: "MAM seedbox updated".to_string(),
                    body: format!("MAM seedbox registered with port {}.", port.into_inner()),
                }],
                publish: Vec::new(),
            },
            WindlassEvent::Mam(MamPublish::Ready) => {
                self.state.mam = ServiceStatus::Ready;
                self.publish_state()
            }
            WindlassEvent::Mam(
                MamPublish::Unavailable { reason } | MamPublish::NotConnectable { reason },
            ) => {
                self.state.mam = ServiceStatus::Degraded;
                self.publish_state_with_activity(reason)
            }
            WindlassEvent::Mam(MamPublish::RateLimited { retry_after }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.publish_state_with_activity(format!(
                    "MAM rate limited for {}s",
                    retry_after.as_secs()
                ))
            }
            WindlassEvent::DbFailed { operation, message } => Outcome {
                actions: Vec::new(),
                publish: vec![WindlassPublish::Activity {
                    message: format!("DB {operation} failed: {message}"),
                }],
            },
            WindlassEvent::TimerFired(WindlassTimer::Snapshot) => Outcome {
                actions: vec![
                    self.snapshot_action(),
                    WindlassAction::ScheduleTimer {
                        timer: WindlassTimer::Snapshot,
                        after: self.config.snapshot_interval,
                    },
                ],
                publish: Vec::new(),
            },
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            WindlassCommand::Refresh => vec![
                WindlassAction::Vpn(VpnCommand::RefreshState),
                WindlassAction::Qbit(QbitCommand::RefreshTorrents),
                WindlassAction::Mam(MamCommand::RefreshStatus),
            ],
        };
        Self::outcome(actions, WindlassResponse::Accepted)
    }
}

impl WindlassMachine {
    fn publish_state(&self) -> Outcome<WindlassAction, WindlassPublish> {
        Outcome {
            actions: vec![self.snapshot_action()],
            publish: vec![WindlassPublish::SystemState(self.state.clone())],
        }
    }

    fn publish_state_with_activity(
        &self,
        message: String,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        Outcome {
            actions: vec![self.snapshot_action()],
            publish: vec![
                WindlassPublish::SystemState(self.state.clone()),
                WindlassPublish::Activity { message },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
    use windlass_mam_core::MamCommand;
    use windlass_mam_core::MamPublish;
    use windlass_qbit_core::QbitCommand;
    use windlass_types::VpnPort;
    use windlass_vpn_core::VpnPublish;

    use crate::{
        ServiceStatus, WindlassAction, WindlassConfig, WindlassEvent, WindlassMachine,
        WindlassPublish,
    };

    fn machine() -> WindlassMachine {
        WindlassMachine::new(
            WindlassConfig {
                snapshot_interval: Duration::from_secs(60),
            },
            Instant::now(),
        )
    }

    fn handle(
        machine: &mut WindlassMachine,
        event: WindlassEvent,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn vpn_port_ready_converges_qbit_and_mam() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            WindlassEvent::Vpn(VpnPublish::PortReady { port }),
        );

        assert_eq!(machine.state().forwarded_port, Some(port));
        assert!(
            out.actions
                .contains(&WindlassAction::Qbit(QbitCommand::EnsureListenPort {
                    port
                }))
        );
        assert!(
            out.actions
                .contains(&WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }))
        );
        assert!(matches!(
            out.publish.as_slice(),
            [WindlassPublish::SystemState(_)]
        ));
    }

    #[test]
    fn vpn_disconnected_degrades_vpn_and_clears_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        handle(
            &mut machine,
            WindlassEvent::Vpn(VpnPublish::PortReady { port }),
        );

        let out = handle(&mut machine, WindlassEvent::Vpn(VpnPublish::Disconnected));

        assert_eq!(machine.state().vpn, ServiceStatus::Degraded);
        assert_eq!(machine.state().forwarded_port, None);
        assert!(matches!(
            out.publish.as_slice(),
            [WindlassPublish::SystemState(_)]
        ));
    }

    #[test]
    fn mam_seedbox_port_ready_records_boot_alert() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::SeedboxPortReady { port }),
        );

        assert!(matches!(
            out.actions.as_slice(),
            [WindlassAction::SendAlert { title, .. }] if title == "MAM seedbox updated"
        ));
        assert!(out.publish.is_empty());
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};
    use windlass_mam_core::{MamCommand, MamPublish};
    use windlass_qbit_core::{QbitCommand, QbitPublish};
    use windlass_types::{TorrentHash, VpnPort};
    use windlass_vpn_core::VpnPublish;

    use crate::{
        ServiceStatus, SystemStateView, WindlassAction, WindlassConfig, WindlassEvent,
        WindlassMachine, WindlassTimer,
    };

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    fn any_torrent_hash() -> impl Strategy<Value = TorrentHash> {
        "[a-f0-9]{40}".prop_map(TorrentHash)
    }

    fn any_service_status() -> impl Strategy<Value = ServiceStatus> {
        prop_oneof![
            Just(ServiceStatus::Unknown),
            Just(ServiceStatus::Ready),
            Just(ServiceStatus::Degraded),
        ]
    }

    fn any_windlass_machine() -> impl Strategy<Value = WindlassMachine> {
        (
            any_service_status(),
            any_service_status(),
            any_service_status(),
            proptest::option::of(any_vpn_port()),
        )
            .prop_map(|(vpn, qbit, mam, forwarded_port)| {
                let mut machine = WindlassMachine::new(
                    WindlassConfig {
                        snapshot_interval: Duration::from_secs(60),
                    },
                    Instant::now(),
                );
                machine.state = SystemStateView {
                    vpn,
                    qbit,
                    mam,
                    forwarded_port,
                };
                machine
            })
    }

    fn any_vpn_publish() -> impl Strategy<Value = VpnPublish> {
        prop_oneof![
            Just(VpnPublish::Connected),
            Just(VpnPublish::Disconnected),
            any_vpn_port().prop_map(|port| VpnPublish::PortReady { port }),
            Just(VpnPublish::PortUnavailable),
        ]
    }

    fn any_qbit_publish() -> impl Strategy<Value = QbitPublish> {
        prop_oneof![
            Just(QbitPublish::Ready),
            any::<String>().prop_map(|reason| QbitPublish::Unavailable { reason }),
            any_vpn_port().prop_map(|port| QbitPublish::ListenPortReady { port }),
            prop::collection::vec(any_torrent_hash(), 0..4)
                .prop_map(|hashes| QbitPublish::TorrentsUpdated { hashes }),
        ]
    }

    fn any_mam_publish() -> impl Strategy<Value = MamPublish> {
        prop_oneof![
            Just(MamPublish::Ready),
            any::<String>().prop_map(|reason| MamPublish::Unavailable { reason }),
            (0u64..=3600).prop_map(|s| MamPublish::RateLimited {
                retry_after: Duration::from_secs(s)
            }),
            proptest::option::of(any_vpn_port())
                .prop_map(|seedbox_port| MamPublish::Connectable { seedbox_port }),
            any::<String>().prop_map(|reason| MamPublish::NotConnectable { reason }),
            any_vpn_port().prop_map(|port| MamPublish::SeedboxPortReady { port }),
        ]
    }

    fn any_windlass_event() -> impl Strategy<Value = WindlassEvent> {
        prop_oneof![
            Just(WindlassEvent::Init),
            any_vpn_publish().prop_map(WindlassEvent::Vpn),
            any_qbit_publish().prop_map(WindlassEvent::Qbit),
            any_mam_publish().prop_map(WindlassEvent::Mam),
            (any::<String>(), any::<String>())
                .prop_map(|(operation, message)| WindlassEvent::DbFailed { operation, message }),
            Just(WindlassEvent::TimerFired(WindlassTimer::Snapshot)),
        ]
    }

    proptest! {
        // GLOBAL-1 (no panic).
        #[test]
        fn handle_never_panics(mut machine in any_windlass_machine(), event in any_windlass_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // DOM-1 (Guarantee C, marquee): the domain never commands qBit or MAM to
        // converge on a port unless it currently holds that forwarded port.
        #[test]
        fn converge_commands_imply_forwarded_port(
            mut machine in any_windlass_machine(),
            event in any_windlass_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for action in &out.actions {
                if let WindlassAction::Qbit(QbitCommand::EnsureListenPort { port })
                    | WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }) = action
                {
                    prop_assert_eq!(machine.state().forwarded_port, Some(*port));
                }
            }
        }

        // DOM-2 (Guarantees B/C): losing VPN connectivity always clears the
        // forwarded port, regardless of prior state.
        #[test]
        fn vpn_loss_clears_forwarded_port(
            mut machine in any_windlass_machine(),
            lost in prop_oneof![
                Just(VpnPublish::Disconnected),
                Just(VpnPublish::PortUnavailable),
            ],
        ) {
            machine.handle(Instant::now(), Timed::now(WindlassEvent::Vpn(lost)));
            prop_assert!(machine.state().forwarded_port.is_none());
        }
    }
}
