//! Container-lifecycle machine (`DockerMachine`).
//!
//! Owns container start/stop/restart/inspect/log-dump, the dependent-container
//! registry, and (in later PRs) §35's stale-namespace check, the restart-storm
//! circuit-breaker, and autoheal-style health-driven restarts.
//!
//! Scope is operator-readiness §38.  See `docs/operator-readiness.md` and
//! `docs/legacy-retirement-plan.md` for the migration sequence.
//!
//! # PR 1 scope (this file)
//!
//! Defines the public surface — `DockerCommand`, `DockerEvent`, `DockerAction`,
//! `DockerPublish`, `DockerTopic`, `DockerResponse`, `DockerConfig`, and an
//! `empty` `DockerMachine` impl that compiles and passes smoke tests.  Actual
//! behavior (event watcher, dependent registry, circuit-breaker, crash-recovery
//! orchestration) lands in subsequent PRs.
#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::collections::HashMap;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};

/// Configuration for the container-lifecycle machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerConfig {
    /// Name of the anchor (VPN) container.  Other containers using
    /// `network_mode: service:<gluetun_anchor>` are treated as dependents.
    pub gluetun_anchor: String,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            gluetun_anchor: "gluetun".to_string(),
        }
    }
}

/// Commands accepted by `DockerMachine`.
///
/// Fleet commands (`StopDependents`, `StartDependents`) iterate the
/// machine's discovered dependent registry; per-name commands operate on a
/// single container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockerCommand {
    /// Stop every container in the dependent registry (e.g. before a Gluetun
    /// restart so dependents don't sit with a broken network namespace).
    StopDependents,
    /// Start every container in the dependent registry (e.g. after Gluetun
    /// recovers).
    StartDependents,
    /// Restart a single named container.  Used for the anchor (Gluetun
    /// recovery) and, in PR 3+, for the §35 stale-namespace recovery path.
    RestartContainer { name: String },
    /// Stop a single named container.
    StopContainer { name: String },
    /// Start a single named container.
    StartContainer { name: String },
    /// Capture logs for a single container into the configured dump
    /// directory.
    DumpLogs { name: String },
    /// Capture logs for every known container (anchor + dependents) into
    /// the configured dump directory.
    DumpAllLogs,
    /// Query the dependent registry.  Returns the current list via
    /// `DockerResponse::Dependents`.
    ListDependents,
}

/// Events produced by the Docker shell (or domain) and consumed by
/// `DockerMachine`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockerEvent {
    /// Emitted once at machine creation.  PR 2 will use this to ask the
    /// shell for an initial dependent discovery.
    Init,
    /// Docker daemon reported a container is healthy.  Used to track
    /// rising-edge health transitions for `DockerPublish::ContainerHealthy`.
    ContainerHealthy { name: String },
    /// Docker daemon reported a container is unhealthy.  Used to track
    /// rising-edge crash transitions for `DockerPublish::ContainerCrashed`.
    ContainerUnhealthy { name: String },
    /// Docker daemon reported a container has stopped.
    ContainerStopped { name: String },
    /// Docker daemon reported a container has started.  Carries
    /// `started_at` so §35's stale-namespace check (PR 3) can compare it
    /// against the anchor's `healthy_since`.
    ContainerStarted {
        name: String,
        started_at: DateTime<Utc>,
    },
    /// Shell completed a `DiscoverDependents` action and reports the
    /// current list of containers sharing the anchor's network namespace.
    DependentsDiscovered { names: Vec<String> },
    /// Shell completed a `DumpLogs` action.
    LogsDumped { name: String, path: String },
    /// Shell's `DumpLogs` action failed.
    LogsDumpFailed { name: String, reason: String },
}

/// Actions that `DockerMachine` asks the shell to perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockerAction {
    /// Start a single named container.
    StartContainer { name: String },
    /// Stop a single named container.
    StopContainer { name: String },
    /// Restart a single named container.
    RestartContainer { name: String },
    /// Ask the shell for a container's `StartedAt` timestamp.  Result
    /// arrives as `DockerEvent::ContainerStarted`.
    InspectContainer { name: String },
    /// Capture a container's logs into the configured dump directory.
    /// Result arrives as `DockerEvent::LogsDumped` or `LogsDumpFailed`.
    DumpLogs { name: String },
    /// Refresh the dependent registry by enumerating containers using
    /// `network_mode: service:<gluetun_anchor>`.  Result arrives as
    /// `DockerEvent::DependentsDiscovered`.
    DiscoverDependents,
}

/// Facts published by `DockerMachine` to topic subscribers.
///
/// All variants are rising-edge: `ContainerCrashed` fires once per
/// healthy→unhealthy transition (re-observing unhealthy is a no-op);
/// `ContainerHealthy` fires once per unhealthy→healthy transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockerPublish {
    /// Rising-edge healthy→unhealthy transition.  DOCKER-1.
    ContainerCrashed { name: String },
    /// Rising-edge unhealthy→healthy transition.
    ContainerHealthy { name: String },
    /// Successful log capture from `DumpLogs`.
    LogsDumped { name: String, path: String },
    /// Container stopped (any reason).
    Stopped { name: String },
    /// Container started.
    Started {
        name: String,
        started_at: DateTime<Utc>,
    },
}

/// Topics for `DockerPublish` routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockerTopic {
    /// Container lifecycle signals (`ContainerCrashed`, `ContainerHealthy`,
    /// `Stopped`, `Started`).
    Lifecycle,
    /// Log-capture results (`LogsDumped`).
    Logs,
}

impl HasTopic<DockerTopic> for DockerPublish {
    fn topic(&self) -> DockerTopic {
        match self {
            Self::ContainerCrashed { .. }
            | Self::ContainerHealthy { .. }
            | Self::Stopped { .. }
            | Self::Started { .. } => DockerTopic::Lifecycle,
            Self::LogsDumped { .. } => DockerTopic::Logs,
        }
    }
}

/// Response returned to callers of `DockerMachine::handle_command`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockerResponse {
    Accepted,
    /// Reply to `DockerCommand::ListDependents`.
    Dependents {
        names: Vec<String>,
    },
}

/// Per-container known state.  Populated from Docker daemon events.
///
/// Fields beyond `name` are unused in PR 1 and exist to make the surface
/// stable across the §38 PR sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    /// Last observed health from the Docker daemon.
    pub health: ContainerHealth,
    /// `StartedAt` from the last `ContainerStarted` event.  None until the
    /// shell reports it.
    pub started_at: Option<DateTime<Utc>>,
}

/// Health classification tracked per container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerHealth {
    Unknown,
    Healthy,
    Unhealthy,
}

/// Container-lifecycle sans-I/O machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerMachine {
    config: DockerConfig,
    /// Per-container state keyed by container name.  Populated from
    /// `ContainerHealthy / ContainerUnhealthy / ContainerStarted /
    /// ContainerStopped` events.
    containers: HashMap<String, ContainerState>,
    /// Names of containers in the dependent registry — populated from
    /// `DependentsDiscovered` events.  Excludes the anchor.
    dependents: Vec<String>,
}

impl DockerMachine {
    /// Returns the configured anchor (VPN) container name.
    #[must_use]
    pub fn anchor(&self) -> &str {
        &self.config.gluetun_anchor
    }

    /// Returns the current dependent registry.
    #[must_use]
    pub fn dependents(&self) -> &[String] {
        &self.dependents
    }

    /// Returns per-container state if known.
    #[must_use]
    pub fn container(&self, name: &str) -> Option<&ContainerState> {
        self.containers.get(name)
    }
}

impl Machine for DockerMachine {
    type Config = DockerConfig;
    type Event = DockerEvent;
    type Action = DockerAction;
    type Publish = DockerPublish;
    type Topic = DockerTopic;
    type Command = DockerCommand;
    type Response = DockerResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            containers: HashMap::new(),
            dependents: Vec::new(),
        }
    }

    /// PR 1: minimal handler.  All events are accepted as no-ops except
    /// `DependentsDiscovered`, which populates the registry so PR 2 can
    /// drive fleet commands against it.  Rising-edge health publishes and
    /// circuit-breaker logic land in PRs 2-5.
    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            DockerEvent::Init
            | DockerEvent::ContainerHealthy { .. }
            | DockerEvent::ContainerUnhealthy { .. }
            | DockerEvent::ContainerStopped { .. }
            | DockerEvent::ContainerStarted { .. }
            | DockerEvent::LogsDumped { .. }
            | DockerEvent::LogsDumpFailed { .. } => Outcome {
                actions: Vec::new(),
                publish: Vec::new(),
            },
            DockerEvent::DependentsDiscovered { names } => {
                self.dependents = names;
                Outcome {
                    actions: Vec::new(),
                    publish: Vec::new(),
                }
            }
        }
    }

    /// PR 1: per-name commands translate directly to their corresponding
    /// action; fleet commands iterate the dependent registry; `DumpAllLogs`
    /// covers anchor + dependents; `ListDependents` returns the registry.
    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        match cmd {
            DockerCommand::StopDependents => {
                let actions = self
                    .dependents
                    .iter()
                    .map(|name| DockerAction::StopContainer { name: name.clone() })
                    .collect();
                Self::outcome(actions, DockerResponse::Accepted)
            }
            DockerCommand::StartDependents => {
                let actions = self
                    .dependents
                    .iter()
                    .map(|name| DockerAction::StartContainer { name: name.clone() })
                    .collect();
                Self::outcome(actions, DockerResponse::Accepted)
            }
            DockerCommand::RestartContainer { name } => Self::outcome(
                vec![DockerAction::RestartContainer { name }],
                DockerResponse::Accepted,
            ),
            DockerCommand::StopContainer { name } => Self::outcome(
                vec![DockerAction::StopContainer { name }],
                DockerResponse::Accepted,
            ),
            DockerCommand::StartContainer { name } => Self::outcome(
                vec![DockerAction::StartContainer { name }],
                DockerResponse::Accepted,
            ),
            DockerCommand::DumpLogs { name } => Self::outcome(
                vec![DockerAction::DumpLogs { name }],
                DockerResponse::Accepted,
            ),
            DockerCommand::DumpAllLogs => {
                let mut actions = vec![DockerAction::DumpLogs {
                    name: self.config.gluetun_anchor.clone(),
                }];
                actions.extend(
                    self.dependents
                        .iter()
                        .map(|name| DockerAction::DumpLogs { name: name.clone() }),
                );
                Self::outcome(actions, DockerResponse::Accepted)
            }
            DockerCommand::ListDependents => Self::outcome(
                Vec::new(),
                DockerResponse::Dependents {
                    names: self.dependents.clone(),
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use windlass_machine::{Machine, Timed};

    use crate::{
        DockerAction, DockerCommand, DockerConfig, DockerEvent, DockerMachine, DockerResponse,
    };

    fn machine() -> DockerMachine {
        DockerMachine::new(DockerConfig::default(), Instant::now())
    }

    fn discover(m: &mut DockerMachine, names: &[&str]) {
        m.handle(
            Instant::now(),
            Timed::now(DockerEvent::DependentsDiscovered {
                names: names.iter().map(|n| (*n).to_string()).collect(),
            }),
        );
    }

    #[test]
    fn init_is_a_no_op() {
        let mut m = machine();
        let out = m.handle(Instant::now(), Timed::now(DockerEvent::Init));
        assert!(out.actions.is_empty());
        assert!(out.publish.is_empty());
        assert!(m.dependents().is_empty());
    }

    #[test]
    fn dependents_discovered_populates_registry() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent", "mlm", "mousehole"]);
        assert_eq!(m.dependents(), ["qbittorrent", "mlm", "mousehole"]);
    }

    #[test]
    fn stop_dependents_emits_one_action_per_dependent() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent", "mlm"]);
        let out = m.handle_command(Instant::now(), DockerCommand::StopDependents);
        assert_eq!(
            out.actions,
            vec![
                DockerAction::StopContainer {
                    name: "qbittorrent".to_string()
                },
                DockerAction::StopContainer {
                    name: "mlm".to_string()
                },
            ]
        );
        assert_eq!(out.response, DockerResponse::Accepted);
    }

    #[test]
    fn start_dependents_with_empty_registry_emits_nothing() {
        // DOCKER-3: StartDependents/StopDependents is a no-op on empty registry.
        let mut m = machine();
        let out = m.handle_command(Instant::now(), DockerCommand::StartDependents);
        assert!(out.actions.is_empty());
        assert_eq!(out.response, DockerResponse::Accepted);
    }

    #[test]
    fn stop_dependents_with_empty_registry_emits_nothing() {
        // DOCKER-3.
        let mut m = machine();
        let out = m.handle_command(Instant::now(), DockerCommand::StopDependents);
        assert!(out.actions.is_empty());
    }

    #[test]
    fn restart_container_translates_to_action() {
        let mut m = machine();
        let out = m.handle_command(
            Instant::now(),
            DockerCommand::RestartContainer {
                name: "gluetun".to_string(),
            },
        );
        assert_eq!(
            out.actions,
            vec![DockerAction::RestartContainer {
                name: "gluetun".to_string()
            }]
        );
    }

    #[test]
    fn dump_all_logs_includes_anchor_and_dependents() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent"]);
        let out = m.handle_command(Instant::now(), DockerCommand::DumpAllLogs);
        assert_eq!(
            out.actions,
            vec![
                DockerAction::DumpLogs {
                    name: "gluetun".to_string()
                },
                DockerAction::DumpLogs {
                    name: "qbittorrent".to_string()
                },
            ]
        );
    }

    #[test]
    fn list_dependents_returns_current_registry() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent", "mlm"]);
        let out = m.handle_command(Instant::now(), DockerCommand::ListDependents);
        assert!(out.actions.is_empty());
        assert_eq!(
            out.response,
            DockerResponse::Dependents {
                names: vec!["qbittorrent".to_string(), "mlm".to_string()],
            }
        );
    }

    #[test]
    fn discovery_replaces_previous_registry() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent", "mlm", "mousehole"]);
        discover(&mut m, &["qbittorrent"]);
        assert_eq!(m.dependents(), ["qbittorrent"]);
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::Instant;

    use chrono::Utc;
    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};

    use crate::{DockerCommand, DockerConfig, DockerEvent, DockerMachine};

    fn any_machine() -> impl Strategy<Value = DockerMachine> {
        any::<Vec<String>>().prop_map(|names| {
            let mut m = DockerMachine::new(DockerConfig::default(), Instant::now());
            m.handle(
                Instant::now(),
                Timed::now(DockerEvent::DependentsDiscovered { names }),
            );
            m
        })
    }

    fn any_event() -> impl Strategy<Value = DockerEvent> {
        prop_oneof![
            Just(DockerEvent::Init),
            any::<String>().prop_map(|name| DockerEvent::ContainerHealthy { name }),
            any::<String>().prop_map(|name| DockerEvent::ContainerUnhealthy { name }),
            any::<String>().prop_map(|name| DockerEvent::ContainerStopped { name }),
            any::<String>().prop_map(|name| DockerEvent::ContainerStarted {
                name,
                started_at: Utc::now()
            }),
            any::<Vec<String>>().prop_map(|names| DockerEvent::DependentsDiscovered { names }),
            (any::<String>(), any::<String>())
                .prop_map(|(name, path)| DockerEvent::LogsDumped { name, path }),
            (any::<String>(), any::<String>())
                .prop_map(|(name, reason)| DockerEvent::LogsDumpFailed { name, reason }),
        ]
    }

    fn any_command() -> impl Strategy<Value = DockerCommand> {
        prop_oneof![
            Just(DockerCommand::StopDependents),
            Just(DockerCommand::StartDependents),
            Just(DockerCommand::DumpAllLogs),
            Just(DockerCommand::ListDependents),
            any::<String>().prop_map(|name| DockerCommand::RestartContainer { name }),
            any::<String>().prop_map(|name| DockerCommand::StopContainer { name }),
            any::<String>().prop_map(|name| DockerCommand::StartContainer { name }),
            any::<String>().prop_map(|name| DockerCommand::DumpLogs { name }),
        ]
    }

    proptest! {
        // GLOBAL-1: handle never panics.
        #[test]
        fn handle_never_panics(mut machine in any_machine(), event in any_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // GLOBAL-1: handle_command never panics.
        #[test]
        fn handle_command_never_panics(mut machine in any_machine(), cmd in any_command()) {
            let _ = machine.handle_command(Instant::now(), cmd);
        }

        // PR 1: no rising-edge publishes yet — the machine is a scaffold and
        // never publishes.  When PRs 2-5 add health tracking, this assertion
        // tightens into the real DOCKER-1 rising-edge invariant.
        #[test]
        fn pr1_machine_publishes_nothing(mut machine in any_machine(), event in any_event()) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            prop_assert!(out.publish.is_empty());
        }
    }
}
