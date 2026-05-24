#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome};
use windlass_types::VpnPort;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MamConfig {
    pub status_retry: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamCommand {
    EnsureAuthenticated,
    EnsureSeedboxPort { port: VpnPort },
    RefreshStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamTimer {
    StatusRetry,
    RateLimitExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamEvent {
    Init,
    AuthSucceeded,
    AuthFailed {
        reason: String,
    },
    StatusFetched {
        connectable: bool,
        seedbox_port: Option<VpnPort>,
    },
    StatusFailed {
        reason: String,
    },
    SeedboxUpdated {
        port: VpnPort,
    },
    SeedboxUpdateFailed {
        port: VpnPort,
        reason: String,
    },
    RateLimited {
        retry_after: Duration,
    },
    TimerFired(MamTimer),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamAction {
    FetchStatus,
    UpdateSeedboxPort { port: VpnPort },
    ScheduleTimer { timer: MamTimer, after: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamPublish {
    Ready,
    Unavailable { reason: String },
    RateLimited { retry_after: Duration },
    Connectable { seedbox_port: Option<VpnPort> },
    NotConnectable { reason: String },
    SeedboxPortReady { port: VpnPort },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamTopic {
    Availability,
    Connectability,
    Seedbox,
}

impl HasTopic<MamTopic> for MamPublish {
    fn topic(&self) -> MamTopic {
        match self {
            Self::Ready | Self::Unavailable { .. } | Self::RateLimited { .. } => {
                MamTopic::Availability
            }
            Self::Connectable { .. } | Self::NotConnectable { .. } => MamTopic::Connectability,
            Self::SeedboxPortReady { .. } => MamTopic::Seedbox,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamResponse {
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MamMachine {
    config: MamConfig,
    authenticated: bool,
    seedbox_port: Option<VpnPort>,
}

impl MamMachine {
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    #[must_use]
    pub const fn seedbox_port(&self) -> Option<VpnPort> {
        self.seedbox_port
    }
}

impl Machine for MamMachine {
    type Config = MamConfig;
    type Event = MamEvent;
    type Action = MamAction;
    type Publish = MamPublish;
    type Topic = MamTopic;
    type Command = MamCommand;
    type Response = MamResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            authenticated: false,
            seedbox_port: None,
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Self::Event,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event {
            MamEvent::Init
            | MamEvent::TimerFired(MamTimer::StatusRetry | MamTimer::RateLimitExpired) => Outcome {
                actions: vec![MamAction::FetchStatus],
                publish: Vec::new(),
            },
            MamEvent::AuthSucceeded => {
                self.authenticated = true;
                Outcome {
                    actions: vec![MamAction::FetchStatus],
                    publish: vec![MamPublish::Ready],
                }
            }
            MamEvent::AuthFailed { reason } | MamEvent::StatusFailed { reason } => Outcome {
                actions: vec![MamAction::ScheduleTimer {
                    timer: MamTimer::StatusRetry,
                    after: self.config.status_retry,
                }],
                publish: vec![MamPublish::Unavailable { reason }],
            },
            MamEvent::StatusFetched {
                connectable,
                seedbox_port,
            } => {
                self.seedbox_port = seedbox_port;
                Outcome {
                    actions: Vec::new(),
                    publish: vec![if connectable {
                        MamPublish::Connectable { seedbox_port }
                    } else {
                        MamPublish::NotConnectable {
                            reason: "MAM reports not connectable".to_string(),
                        }
                    }],
                }
            }
            MamEvent::SeedboxUpdated { port } => {
                self.seedbox_port = Some(port);
                Outcome {
                    actions: Vec::new(),
                    publish: vec![MamPublish::SeedboxPortReady { port }],
                }
            }
            MamEvent::SeedboxUpdateFailed { port, reason } => Outcome {
                actions: vec![MamAction::ScheduleTimer {
                    timer: MamTimer::StatusRetry,
                    after: self.config.status_retry,
                }],
                publish: vec![
                    MamPublish::Unavailable { reason },
                    MamPublish::SeedboxPortReady { port },
                ],
            },
            MamEvent::RateLimited { retry_after } => Outcome {
                actions: vec![MamAction::ScheduleTimer {
                    timer: MamTimer::RateLimitExpired,
                    after: retry_after,
                }],
                publish: vec![MamPublish::RateLimited { retry_after }],
            },
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            MamCommand::EnsureAuthenticated | MamCommand::RefreshStatus => {
                vec![MamAction::FetchStatus]
            }
            MamCommand::EnsureSeedboxPort { port } => {
                vec![MamAction::UpdateSeedboxPort { port }]
            }
        };
        Self::outcome(actions, MamResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::Machine;
    use windlass_types::VpnPort;

    use crate::{MamAction, MamCommand, MamConfig, MamEvent, MamMachine, MamPublish, MamTimer};

    fn machine() -> MamMachine {
        MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
            },
            Instant::now(),
        )
    }

    #[test]
    fn auth_success_publishes_ready_and_fetches_status() {
        let mut machine = machine();

        let out = machine.handle(Instant::now(), MamEvent::AuthSucceeded);

        assert!(machine.is_authenticated());
        assert_eq!(out.actions, vec![MamAction::FetchStatus]);
        assert_eq!(out.publish, vec![MamPublish::Ready]);
    }

    #[test]
    fn ensure_authenticated_command_fetches_status() {
        let mut machine = machine();

        let out = machine.handle_command(Instant::now(), MamCommand::EnsureAuthenticated);

        assert_eq!(out.actions, vec![MamAction::FetchStatus]);
    }

    #[test]
    fn rate_limit_schedules_expiry_timer() {
        let mut machine = machine();
        let retry_after = Duration::from_secs(30);

        let out = machine.handle(Instant::now(), MamEvent::RateLimited { retry_after });

        assert_eq!(
            out.actions,
            vec![MamAction::ScheduleTimer {
                timer: MamTimer::RateLimitExpired,
                after: retry_after,
            }]
        );
        assert_eq!(out.publish, vec![MamPublish::RateLimited { retry_after }]);
    }

    #[test]
    fn seedbox_update_publishes_ready_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = machine.handle(Instant::now(), MamEvent::SeedboxUpdated { port });

        assert_eq!(machine.seedbox_port(), Some(port));
        assert_eq!(out.publish, vec![MamPublish::SeedboxPortReady { port }]);
    }
}
