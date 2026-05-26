#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
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
    SeedboxUpdated,
    SeedboxUpdateFailed {
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
    UpdateSeedbox,
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
    desired_seedbox_port: Option<VpnPort>,
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

    fn refresh_or_update_seedbox(&self) -> Vec<MamAction> {
        if self.desired_seedbox_port.is_some() {
            vec![MamAction::UpdateSeedbox]
        } else {
            vec![MamAction::FetchStatus]
        }
    }

    fn converge_seedbox(&self) -> Vec<MamAction> {
        let Some(desired) = self.desired_seedbox_port else {
            return Vec::new();
        };
        if self.seedbox_port == Some(desired) {
            Vec::new()
        } else {
            vec![MamAction::UpdateSeedbox]
        }
    }

    fn seedbox_publish(&self, seedbox_port: Option<VpnPort>) -> Vec<MamPublish> {
        seedbox_port
            .filter(|port| {
                self.desired_seedbox_port
                    .is_none_or(|desired_port| desired_port == *port)
            })
            .map(|port| MamPublish::SeedboxPortReady { port })
            .into_iter()
            .collect()
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
            desired_seedbox_port: None,
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            MamEvent::Init => Outcome {
                actions: vec![MamAction::FetchStatus],
                publish: Vec::new(),
            },
            MamEvent::TimerFired(MamTimer::StatusRetry | MamTimer::RateLimitExpired) => Outcome {
                actions: self.refresh_or_update_seedbox(),
                publish: Vec::new(),
            },
            MamEvent::AuthSucceeded => {
                self.authenticated = true;
                Outcome {
                    actions: vec![MamAction::FetchStatus],
                    publish: vec![MamPublish::Ready],
                }
            }
            MamEvent::AuthFailed { reason }
            | MamEvent::StatusFailed { reason }
            | MamEvent::SeedboxUpdateFailed { reason } => Outcome {
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
                let mut publish = vec![if connectable {
                    MamPublish::Connectable { seedbox_port }
                } else {
                    MamPublish::NotConnectable {
                        reason: "MAM reports not connectable".to_string(),
                    }
                }];
                if connectable {
                    publish.extend(self.seedbox_publish(seedbox_port));
                }
                Outcome {
                    actions: self.converge_seedbox(),
                    publish,
                }
            }
            MamEvent::SeedboxUpdated => {
                let port = self.desired_seedbox_port;
                if let Some(p) = port {
                    self.seedbox_port = Some(p);
                }
                Outcome {
                    actions: Vec::new(),
                    publish: port
                        .map(|p| MamPublish::SeedboxPortReady { port: p })
                        .into_iter()
                        .collect(),
                }
            }
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
                self.desired_seedbox_port = Some(port);
                if self.seedbox_port == Some(port) {
                    return Self::outcome_with_publish(
                        Vec::new(),
                        vec![MamPublish::SeedboxPortReady { port }],
                        MamResponse::Accepted,
                    );
                }
                vec![MamAction::UpdateSeedbox]
            }
        };
        Self::outcome(actions, MamResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
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

    fn handle(machine: &mut MamMachine, event: MamEvent) -> Outcome<MamAction, MamPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn auth_success_publishes_ready_and_fetches_status() {
        let mut machine = machine();

        let out = handle(&mut machine, MamEvent::AuthSucceeded);

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

        let out = handle(&mut machine, MamEvent::RateLimited { retry_after });

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
        // Set a desired port so the machine knows which port was converged.
        let _ = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });

        let out = handle(&mut machine, MamEvent::SeedboxUpdated);

        assert_eq!(machine.seedbox_port(), Some(port));
        assert_eq!(out.publish, vec![MamPublish::SeedboxPortReady { port }]);
    }

    #[test]
    fn status_mismatch_updates_desired_seedbox_without_publishing_ready() {
        let mut machine = machine();
        let desired = VpnPort::try_new(51_820).unwrap();
        let observed = VpnPort::try_new(42_000).unwrap();
        let _ = machine.handle_command(
            Instant::now(),
            MamCommand::EnsureSeedboxPort { port: desired },
        );

        let out = handle(
            &mut machine,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: Some(observed),
            },
        );

        assert_eq!(out.actions, vec![MamAction::UpdateSeedbox]);
        assert_eq!(
            out.publish,
            vec![MamPublish::Connectable {
                seedbox_port: Some(observed),
            }]
        );
    }

    #[test]
    fn seedbox_update_failure_retries_desired_port_without_ready_publish() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });

        let failed = handle(
            &mut machine,
            MamEvent::SeedboxUpdateFailed {
                reason: "rate limited".to_string(),
            },
        );

        assert_eq!(
            failed.actions,
            vec![MamAction::ScheduleTimer {
                timer: MamTimer::StatusRetry,
                after: Duration::from_secs(5),
            }]
        );
        assert_eq!(
            failed.publish,
            vec![MamPublish::Unavailable {
                reason: "rate limited".to_string(),
            }]
        );

        let retry = handle(&mut machine, MamEvent::TimerFired(MamTimer::StatusRetry));

        assert_eq!(retry.actions, vec![MamAction::UpdateSeedbox]);
    }

    #[test]
    fn ensure_seedbox_port_publishes_when_already_converged() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });
        let _ = handle(&mut machine, MamEvent::SeedboxUpdated);

        let out = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });

        assert!(out.actions.is_empty());
        assert_eq!(out.publish, vec![MamPublish::SeedboxPortReady { port }]);
    }
}
