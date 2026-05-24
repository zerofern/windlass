#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome};
use windlass_types::VpnPort;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnConfig {
    pub health_poll_interval: Duration,
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
        event: Self::Event,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event {
            VpnEvent::Init => Outcome {
                actions: vec![
                    VpnAction::StartMonitoring,
                    VpnAction::InspectContainer,
                    VpnAction::ScheduleTimer {
                        timer: VpnTimer::HealthPoll,
                        after: self.config.health_poll_interval,
                    },
                ],
                publish: Vec::new(),
            },
            VpnEvent::ContainerHealthy => {
                self.connected = true;
                Outcome {
                    actions: vec![VpnAction::ReadPortFiles],
                    publish: vec![VpnPublish::Connected],
                }
            }
            VpnEvent::ContainerUnhealthy => {
                self.connected = false;
                self.port = None;
                Outcome {
                    actions: Vec::new(),
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
                self.port = port;
                let mut publish = vec![if connected {
                    VpnPublish::Connected
                } else {
                    VpnPublish::Disconnected
                }];
                publish.push(port.map_or(VpnPublish::PortUnavailable, |port| {
                    VpnPublish::PortReady { port }
                }));
                Outcome {
                    actions: Vec::new(),
                    publish,
                }
            }
            VpnEvent::StateReadFailed { .. } => Outcome {
                actions: vec![VpnAction::ScheduleTimer {
                    timer: VpnTimer::HealthPoll,
                    after: self.config.health_poll_interval,
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
            VpnCommand::StartMonitoring => vec![VpnAction::StartMonitoring],
            VpnCommand::RefreshState => vec![VpnAction::InspectContainer],
            VpnCommand::ReadForwardedPort => vec![VpnAction::ReadPortFiles],
        };
        Self::outcome(actions, VpnResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::Machine;
    use windlass_types::VpnPort;

    use crate::{VpnAction, VpnConfig, VpnEvent, VpnMachine, VpnPublish, VpnTimer};

    fn machine() -> VpnMachine {
        VpnMachine::new(
            VpnConfig {
                health_poll_interval: Duration::from_secs(2),
            },
            Instant::now(),
        )
    }

    #[test]
    fn init_starts_monitoring_and_health_poll() {
        let mut machine = machine();

        let out = machine.handle(Instant::now(), VpnEvent::Init);

        assert_eq!(
            out.actions,
            vec![
                VpnAction::StartMonitoring,
                VpnAction::InspectContainer,
                VpnAction::ScheduleTimer {
                    timer: VpnTimer::HealthPoll,
                    after: Duration::from_secs(2),
                },
            ]
        );
        assert!(out.publish.is_empty());
    }

    #[test]
    fn healthy_container_publishes_connected_and_reads_port_files() {
        let mut machine = machine();

        let out = machine.handle(Instant::now(), VpnEvent::ContainerHealthy);

        assert!(machine.is_connected());
        assert_eq!(out.actions, vec![VpnAction::ReadPortFiles]);
        assert_eq!(out.publish, vec![VpnPublish::Connected]);
    }

    #[test]
    fn port_file_changed_publishes_port_ready() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = machine.handle(Instant::now(), VpnEvent::PortFileChanged { port });

        assert_eq!(machine.port(), Some(port));
        assert_eq!(out.publish, vec![VpnPublish::PortReady { port }]);
    }
}
