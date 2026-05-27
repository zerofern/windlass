#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::VpnPort;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnConfig {
    pub health_poll_interval: Duration,
    pub unhealthy_poll_interval: Duration,
    pub port_read_retry_interval: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnCommand {
    StartMonitoring,
    RefreshState,
    ReadForwardedPort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnTimer {
    HealthPoll,
    PortReadRetry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnEvent {
    Init,
    ContainerHealthy,
    ContainerUnhealthy,
    PortFileChanged {
        port: VpnPort,
    },
    PublicIpChanged {
        ip: Ipv4Addr,
    },
    StateRead {
        connected: bool,
        port: Option<VpnPort>,
    },
    StateReadFailed {
        reason: String,
    },
    TimerFired(VpnTimer),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnAction {
    InspectContainer,
    ReadPortFiles,
    StartMonitoring,
    ScheduleTimer { timer: VpnTimer, after: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnPublish {
    Connected,
    Disconnected,
    PortReady { port: VpnPort },
    PortUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnTopic {
    Connectivity,
    Port,
}

impl HasTopic<VpnTopic> for VpnPublish {
    fn topic(&self) -> VpnTopic {
        match self {
            Self::Connected | Self::Disconnected => VpnTopic::Connectivity,
            Self::PortReady { .. } | Self::PortUnavailable => VpnTopic::Port,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnResponse {
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpnMachine {
    config: VpnConfig,
    connected: bool,
    port: Option<VpnPort>,
}

impl VpnMachine {
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    #[must_use]
    pub const fn port(&self) -> Option<VpnPort> {
        self.port
    }
}

impl Machine for VpnMachine {
    type Config = VpnConfig;
    type Event = VpnEvent;
    type Action = VpnAction;
    type Publish = VpnPublish;
    type Topic = VpnTopic;
    type Command = VpnCommand;
    type Response = VpnResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            connected: false,
            port: None,
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            VpnEvent::Init => Outcome {
                actions: vec![VpnAction::StartMonitoring, VpnAction::InspectContainer],
                publish: Vec::new(),
            },
            VpnEvent::ContainerHealthy => {
                self.connected = true;
                Outcome {
                    actions: vec![
                        VpnAction::ReadPortFiles,
                        VpnAction::ScheduleTimer {
                            timer: VpnTimer::HealthPoll,
                            after: self.config.health_poll_interval,
                        },
                    ],
                    publish: vec![VpnPublish::Connected],
                }
            }
            VpnEvent::ContainerUnhealthy => {
                self.connected = false;
                self.port = None;
                Outcome {
                    actions: vec![VpnAction::ScheduleTimer {
                        timer: VpnTimer::HealthPoll,
                        after: self.config.unhealthy_poll_interval,
                    }],
                    publish: vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable],
                }
            }
            VpnEvent::PortFileChanged { port } => {
                self.port = Some(port);
                Outcome {
                    actions: Vec::new(),
                    publish: vec![VpnPublish::PortReady { port }],
                }
            }
            VpnEvent::StateRead { connected, port } => {
                self.connected = connected;
                if connected {
                    self.port = port;
                    let port_publish = port.map_or(VpnPublish::PortUnavailable, |p| {
                        VpnPublish::PortReady { port: p }
                    });
                    Outcome {
                        actions: Vec::new(),
                        publish: vec![VpnPublish::Connected, port_publish],
                    }
                } else {
                    // A disconnected VPN never holds a forwarded port, regardless
                    // of what the shell reports. Mirror ContainerUnhealthy (VPN-1).
                    self.port = None;
                    Outcome {
                        actions: Vec::new(),
                        publish: vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable],
                    }
                }
            }
            VpnEvent::StateReadFailed { .. } => Outcome {
                actions: vec![VpnAction::ScheduleTimer {
                    timer: VpnTimer::PortReadRetry,
                    after: self.config.port_read_retry_interval,
                }],
                publish: Vec::new(),
            },
            VpnEvent::TimerFired(VpnTimer::HealthPoll) => Outcome {
                actions: vec![VpnAction::InspectContainer],
                publish: Vec::new(),
            },
            VpnEvent::TimerFired(VpnTimer::PortReadRetry) => Outcome {
                actions: vec![VpnAction::ReadPortFiles],
                publish: Vec::new(),
            },
            VpnEvent::PublicIpChanged { .. } => Outcome::none(),
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            VpnCommand::StartMonitoring => {
                vec![VpnAction::StartMonitoring, VpnAction::InspectContainer]
            }
            VpnCommand::RefreshState => vec![VpnAction::InspectContainer],
            VpnCommand::ReadForwardedPort => vec![VpnAction::ReadPortFiles],
        };
        Self::outcome(actions, VpnResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
    use windlass_types::VpnPort;

    use crate::{VpnAction, VpnCommand, VpnConfig, VpnEvent, VpnMachine, VpnPublish, VpnTimer};

    fn machine() -> VpnMachine {
        VpnMachine::new(
            VpnConfig {
                health_poll_interval: Duration::from_secs(2),
                unhealthy_poll_interval: Duration::from_millis(250),
                port_read_retry_interval: Duration::from_millis(500),
            },
            Instant::now(),
        )
    }

    fn handle(machine: &mut VpnMachine, event: VpnEvent) -> Outcome<VpnAction, VpnPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn init_starts_monitoring_and_health_poll() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::Init);

        assert_eq!(
            out.actions,
            vec![VpnAction::StartMonitoring, VpnAction::InspectContainer]
        );
        assert!(out.publish.is_empty());
    }

    #[test]
    fn start_monitoring_command_matches_init_actions() {
        let mut machine = machine();

        let out = machine.handle_command(Instant::now(), VpnCommand::StartMonitoring);

        assert_eq!(
            out.actions,
            vec![VpnAction::StartMonitoring, VpnAction::InspectContainer]
        );
    }

    #[test]
    fn healthy_container_publishes_connected_and_reads_port_files() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::ContainerHealthy);

        assert!(machine.is_connected());
        assert_eq!(
            out.actions,
            vec![
                VpnAction::ReadPortFiles,
                VpnAction::ScheduleTimer {
                    timer: VpnTimer::HealthPoll,
                    after: Duration::from_secs(2),
                },
            ]
        );
        assert_eq!(out.publish, vec![VpnPublish::Connected]);
    }

    #[test]
    fn unhealthy_container_publishes_disconnected_and_schedules_fast_poll() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::ContainerUnhealthy);

        assert!(!machine.is_connected());
        assert_eq!(
            out.actions,
            vec![VpnAction::ScheduleTimer {
                timer: VpnTimer::HealthPoll,
                after: Duration::from_millis(250),
            }]
        );
        assert_eq!(
            out.publish,
            vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable]
        );
    }

    #[test]
    fn port_file_changed_publishes_port_ready() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(&mut machine, VpnEvent::PortFileChanged { port });

        assert_eq!(machine.port(), Some(port));
        assert_eq!(out.publish, vec![VpnPublish::PortReady { port }]);
    }

    #[test]
    fn state_read_failed_schedules_port_read_retry() {
        let mut machine = machine();

        let out = handle(
            &mut machine,
            VpnEvent::StateReadFailed {
                reason: "files not ready".to_string(),
            },
        );

        assert_eq!(
            out.actions,
            vec![VpnAction::ScheduleTimer {
                timer: VpnTimer::PortReadRetry,
                after: Duration::from_millis(500),
            }]
        );
        assert!(out.publish.is_empty());
    }

    #[test]
    fn port_read_retry_timer_fires_read_port_files() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::TimerFired(VpnTimer::PortReadRetry));

        assert_eq!(out.actions, vec![VpnAction::ReadPortFiles]);
        assert!(out.publish.is_empty());
    }

    #[test]
    fn health_poll_timer_inspects_container() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::TimerFired(VpnTimer::HealthPoll));

        assert_eq!(out.actions, vec![VpnAction::InspectContainer]);
        assert!(out.publish.is_empty());
    }

    // StateRead four-shape tests (story 18 / VPN-4).

    #[test]
    fn state_read_connected_with_port_publishes_connected_and_port_ready() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: true,
                port: Some(port),
            },
        );

        assert!(machine.is_connected());
        assert_eq!(machine.port(), Some(port));
        assert_eq!(
            out.publish,
            vec![VpnPublish::Connected, VpnPublish::PortReady { port }]
        );
        assert!(out.actions.is_empty());
    }

    #[test]
    fn state_read_connected_without_port_publishes_connected_and_port_unavailable() {
        let mut machine = machine();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: true,
                port: None,
            },
        );

        assert!(machine.is_connected());
        assert_eq!(machine.port(), None);
        assert_eq!(
            out.publish,
            vec![VpnPublish::Connected, VpnPublish::PortUnavailable]
        );
        assert!(out.actions.is_empty());
    }

    #[test]
    fn state_read_disconnected_without_port_publishes_disconnected_and_port_unavailable() {
        let mut machine = machine();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: false,
                port: None,
            },
        );

        assert!(!machine.is_connected());
        assert_eq!(machine.port(), None);
        assert_eq!(
            out.publish,
            vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable]
        );
        assert!(out.actions.is_empty());
    }

    #[test]
    fn state_read_disconnected_with_port_clears_port_and_publishes_unavailable() {
        // Dishonest shell event: connected=false but port=Some(_). The machine
        // must defend: never advertise a port for a disconnected VPN (VPN-1/VPN-4).
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: false,
                port: Some(port),
            },
        );

        assert!(!machine.is_connected());
        assert_eq!(
            machine.port(),
            None,
            "port must be cleared when disconnected"
        );
        assert_eq!(
            out.publish,
            vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable]
        );
        assert!(out.actions.is_empty());
    }
}

#[cfg(test)]
mod prop_tests {
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};
    use windlass_types::VpnPort;

    use crate::{VpnConfig, VpnEvent, VpnMachine, VpnPublish, VpnTimer};

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    // Fully-arbitrary machine state (every `connected × port` combination,
    // including ones a real event history would not reach). VPN-2 is a *total*
    // invariant, so it must hold even on unreachable states.
    fn any_vpn_machine() -> impl Strategy<Value = VpnMachine> {
        (any::<bool>(), proptest::option::of(any_vpn_port())).prop_map(|(connected, port)| {
            let mut machine = VpnMachine::new(
                VpnConfig {
                    health_poll_interval: Duration::from_secs(2),
                    unhealthy_poll_interval: Duration::from_millis(250),
                    port_read_retry_interval: Duration::from_millis(500),
                },
                Instant::now(),
            );
            machine.connected = connected;
            machine.port = port;
            machine
        })
    }

    fn any_vpn_event() -> impl Strategy<Value = VpnEvent> {
        prop_oneof![
            Just(VpnEvent::Init),
            Just(VpnEvent::ContainerHealthy),
            Just(VpnEvent::ContainerUnhealthy),
            any_vpn_port().prop_map(|port| VpnEvent::PortFileChanged { port }),
            any::<[u8; 4]>().prop_map(|b| VpnEvent::PublicIpChanged {
                ip: Ipv4Addr::from(b)
            }),
            (any::<bool>(), proptest::option::of(any_vpn_port()))
                .prop_map(|(connected, port)| VpnEvent::StateRead { connected, port }),
            any::<String>().prop_map(|reason| VpnEvent::StateReadFailed { reason }),
            Just(VpnEvent::TimerFired(VpnTimer::HealthPoll)),
            Just(VpnEvent::TimerFired(VpnTimer::PortReadRetry)),
        ]
    }

    proptest! {
        // GLOBAL-1 (no panic): handle tolerates any (state, event).
        #[test]
        fn handle_never_panics(mut machine in any_vpn_machine(), event in any_vpn_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // VPN-2 (Guarantee C): every published `PortReady` carries the port the
        // machine currently holds, and is only published when a port is held.
        #[test]
        fn published_port_ready_matches_held_port(
            mut machine in any_vpn_machine(),
            event in any_vpn_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for publish in &out.publish {
                if let VpnPublish::PortReady { port } = publish {
                    prop_assert_eq!(machine.port(), Some(*port));
                }
            }
        }
    }
}
