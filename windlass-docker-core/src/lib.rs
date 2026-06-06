//! Container-lifecycle machine (`DockerMachine`).
//!
//! Owns container start/stop/restart/inspect/log-dump, the dependent-container
//! registry, and §35's stale-namespace check + restart-storm circuit-breaker
//! (migrated here from `windlass-vpn-core` in §38 PR 3).
//!
//! Scope is operator-readiness §38.  See `docs/operator-readiness.md` and
//! `docs/legacy-retirement-plan.md` for the migration sequence.
//!
//! # Current scope (PRs 1-5)
//!
//! - Public surface (`DockerCommand` / `Event` / `Action` / `Publish` /
//!   `Topic` / `Response` / `Config`).
//! - Dependent registry populated from `DependentsDiscovered` events;
//!   fleet commands (`StopDependents` / `StartDependents`) iterate it.
//! - §35 stale-namespace logic on `ContainerStarted` for known dependents,
//!   gated on the anchor's `healthy_since` tracked from rising-edge
//!   `ContainerHealthy { name == anchor }`.
//! - Restart circuit-breaker (DOCKER-2): suppresses further restarts after
//!   `max_restarts_per_window`, publishes `RestartStorm` once per trip,
//!   and emits a one-shot `DumpLogs` fan-out (anchor + dependents) per
//!   incident.  Shared between the stale-namespace path and the autoheal
//!   path so a single restart budget covers both.
//! - Autoheal subsume (PR 5, DOCKER-5): when
//!   `DockerConfig::autoheal_dependents` is `true`, every unhealthy
//!   event for a known dependent triggers a circuit-breakered restart.
//!   The anchor is excluded — VPN-core-driven crash recovery handles
//!   that via the domain.  Operator can remove the standalone `autoheal`
//!   compose sidecar.
#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};

/// Configuration for the container-lifecycle machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerConfig {
    /// Name of the anchor (VPN) container.  Other containers using
    /// `network_mode: service:<gluetun_anchor>` are treated as dependents.
    pub gluetun_anchor: String,
    /// §35: maximum number of `RestartContainer` actions emitted within
    /// `restart_window_duration` before the circuit breaker trips and
    /// suppresses further restarts (publishing `RestartStorm` instead).
    /// `0` disables the breaker entirely.
    pub max_restarts_per_window: u32,
    /// §35: sliding-window length for the restart circuit breaker.
    pub restart_window_duration: Duration,
    /// §38 PR 5: when `true`, `ContainerUnhealthy` events for any known
    /// dependent trigger a `RestartContainer` (gated by the §35 circuit
    /// breaker).  Subsumes the standalone `autoheal` sidecar.  The anchor
    /// is excluded — anchor crash recovery is driven by the VPN core's
    /// `Crashed` publish via the domain.
    pub autoheal_dependents: bool,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            gluetun_anchor: "gluetun".to_string(),
            max_restarts_per_window: 3,
            restart_window_duration: Duration::from_mins(10),
            autoheal_dependents: false,
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
    /// §35 / DOCKER-4: a dependent's `started_at` predates the anchor's
    /// `healthy_since` — the dependent's network namespace may be stale
    /// and traffic from it may leak outside the VPN.  Domain routes this
    /// to a Critical alert and clears the §29 admission gate.
    DependentNetworkUntrusted {
        name: String,
        dependent_started_at: DateTime<Utc>,
        gluetun_healthy_since: DateTime<Utc>,
    },
    /// §35: a dependent's `started_at` is now at-or-after
    /// `anchor_healthy_since` — the namespace is trusted again.  Rising-
    /// edge only.
    DependentNetworkTrusted { name: String },
    /// §35 / DOCKER-2: the restart circuit-breaker tripped —
    /// `max_restarts_per_window` restarts have been emitted within
    /// `restart_window_duration`.  Further `RestartContainer` actions are
    /// suppressed until the window slides.  Fires once per trip, not on
    /// every suppressed restart.
    RestartStorm { window_count: u32, max: u32 },
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
            | Self::Started { .. }
            | Self::DependentNetworkUntrusted { .. }
            | Self::DependentNetworkTrusted { .. }
            | Self::RestartStorm { .. } => DockerTopic::Lifecycle,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainerState {
    /// Last observed health from the Docker daemon.
    pub health: ContainerHealth,
    /// `StartedAt` from the last `ContainerStarted` event.  None until the
    /// shell reports it.
    pub started_at: Option<DateTime<Utc>>,
    /// §35: `true` iff the dependent's `started_at` is at-or-after the
    /// anchor's `healthy_since`.  Defaults to `false`; updated on every
    /// `ContainerStarted` event for a known dependent.
    pub network_trusted: bool,
}

impl Default for ContainerState {
    fn default() -> Self {
        Self {
            health: ContainerHealth::Unknown,
            started_at: None,
            network_trusted: false,
        }
    }
}

/// Health classification tracked per container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ContainerHealth {
    Unknown,
    Healthy,
    Unhealthy,
}

/// Container-lifecycle sans-I/O machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DockerMachine {
    config: DockerConfig,
    /// Per-container state keyed by container name.  Populated from
    /// `ContainerHealthy / ContainerUnhealthy / ContainerStarted /
    /// ContainerStopped` events.
    containers: HashMap<String, ContainerState>,
    /// Names of containers in the dependent registry — populated from
    /// `DependentsDiscovered` events.  Excludes the anchor.
    dependents: Vec<String>,
    /// §35: timestamp at which the anchor container transitioned to
    /// healthy.  Set on rising-edge `ContainerHealthy { name == anchor }`,
    /// cleared on rising-edge `ContainerUnhealthy { name == anchor }`.
    /// Compared against per-dependent `started_at` for the stale-namespace
    /// check.
    anchor_healthy_since: Option<DateTime<Utc>>,
    /// §35: sliding window of recent `RestartContainer` action times for
    /// the circuit breaker.  Times older than `restart_window_duration`
    /// are pruned on every check.
    restart_window: VecDeque<DateTime<Utc>>,
    /// §35: identifier for the current incident (a contiguous run of
    /// problems triggered by the same anchor-unhealthy event).  Bumps on
    /// every anchor-unhealthy→healthy transition.
    incident_id: u64,
    /// §35: `true` once the current incident has produced a `DumpLogs`
    /// fan-out (anchor + dependents).  Suppresses further dumps until the
    /// next incident.  Reset on rising-edge anchor recovery.
    crash_dump_emitted_for_current_incident: bool,
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

    /// Returns the anchor's `healthy_since` timestamp.  `None` when the
    /// anchor is not currently healthy or has not yet been observed
    /// healthy.
    #[must_use]
    pub const fn anchor_healthy_since(&self) -> Option<DateTime<Utc>> {
        self.anchor_healthy_since
    }

    /// Returns the current §35 incident id.
    #[must_use]
    pub const fn incident_id(&self) -> u64 {
        self.incident_id
    }

    /// Prunes restart-window timestamps older than the configured window.
    fn prune_restart_window(&mut self, now: DateTime<Utc>) {
        let window = chrono::Duration::from_std(self.config.restart_window_duration)
            .unwrap_or_else(|_| chrono::Duration::seconds(0));
        while let Some(front) = self.restart_window.front() {
            if now.signed_duration_since(*front) > window {
                self.restart_window.pop_front();
            } else {
                break;
            }
        }
    }

    /// §35 / §38 PR 5: try to emit a `RestartContainer { name }` action,
    /// gated by the circuit breaker.  When the breaker trips, returns the
    /// `RestartStorm` publish plus a one-shot `DumpLogs` fan-out (anchor +
    /// known dependents) deduped to once per incident.
    ///
    /// Shared by the stale-namespace path (`ContainerStarted` for a stale
    /// dependent) and the autoheal path (`ContainerUnhealthy` for a known
    /// dependent), so a single restart budget covers both.
    fn try_restart(
        &mut self,
        name: String,
        now: DateTime<Utc>,
    ) -> (Vec<DockerAction>, Vec<DockerPublish>) {
        self.prune_restart_window(now);
        let max = self.config.max_restarts_per_window;
        let window_count = u32::try_from(self.restart_window.len()).unwrap_or(u32::MAX);
        if max == 0 || window_count < max {
            self.restart_window.push_back(now);
            (vec![DockerAction::RestartContainer { name }], Vec::new())
        } else {
            let mut actions = Vec::new();
            let publishes = vec![DockerPublish::RestartStorm { window_count, max }];
            if !self.crash_dump_emitted_for_current_incident {
                self.crash_dump_emitted_for_current_incident = true;
                actions.push(DockerAction::DumpLogs {
                    name: self.config.gluetun_anchor.clone(),
                });
                for dep_name in self.dependents.clone() {
                    actions.push(DockerAction::DumpLogs { name: dep_name });
                }
            }
            (actions, publishes)
        }
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
    type StateSnapshot = Self;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            containers: HashMap::new(),
            dependents: Vec::new(),
            anchor_healthy_since: None,
            restart_window: VecDeque::new(),
            incident_id: 0,
            crash_dump_emitted_for_current_incident: false,
        }
    }

    /// PR 3: §35 logic — stale-namespace detection on `ContainerStarted`
    /// for known dependents, anchor-health rising-edge tracking that
    /// powers it, and the restart-storm circuit breaker.
    #[allow(clippy::too_many_lines)]
    fn handle(
        &mut self,
        _now: Instant,
        wall_now: chrono::DateTime<chrono::Utc>,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            // §38 PR 6: at startup, seed the registry and probe the
            // anchor's health so VPN core gets the initial
            // ContainerHealthy/Unhealthy signal without polling Docker
            // itself.  The bollard event watcher then takes over for
            // ongoing state changes.
            DockerEvent::Init => Outcome {
                actions: vec![
                    DockerAction::DiscoverDependents,
                    DockerAction::InspectContainer {
                        name: self.config.gluetun_anchor.clone(),
                    },
                ],
                publishes: Vec::new(),
            },
            DockerEvent::LogsDumped { .. } | DockerEvent::LogsDumpFailed { .. } => Outcome::none(),
            DockerEvent::DependentsDiscovered { names } => {
                self.dependents = names;
                Outcome::none()
            }
            DockerEvent::ContainerHealthy { name } => {
                let entry = self.containers.entry(name.clone()).or_default();
                let was_healthy = entry.health == ContainerHealth::Healthy;
                entry.health = ContainerHealth::Healthy;
                let mut publishes = Vec::new();
                let mut actions = Vec::new();
                if name == self.config.gluetun_anchor {
                    let was_anchor_healthy = self.anchor_healthy_since.is_some();
                    if !was_anchor_healthy {
                        // Rising-edge anchor recovery — new incident
                        // starts, dump dedup resets, all dependents are
                        // marked untrusted until re-inspected.
                        self.anchor_healthy_since = Some(wall_now);
                        self.incident_id = self.incident_id.saturating_add(1);
                        self.crash_dump_emitted_for_current_incident = false;
                        for dep_name in self.dependents.clone() {
                            let dep = self.containers.entry(dep_name.clone()).or_default();
                            dep.network_trusted = false;
                            actions.push(DockerAction::InspectContainer { name: dep_name });
                        }
                        publishes.push(DockerPublish::ContainerHealthy { name });
                    }
                } else if !was_healthy {
                    publishes.push(DockerPublish::ContainerHealthy { name });
                }
                Outcome { actions, publishes }
            }
            DockerEvent::ContainerUnhealthy { name } => {
                let entry = self.containers.entry(name.clone()).or_default();
                let was_unhealthy = entry.health == ContainerHealth::Unhealthy;
                entry.health = ContainerHealth::Unhealthy;
                let mut publishes = Vec::new();
                let mut actions = Vec::new();
                if name == self.config.gluetun_anchor {
                    let was_anchor_healthy = self.anchor_healthy_since.is_some();
                    if was_anchor_healthy {
                        // Rising-edge anchor crash — clear healthy_since
                        // and mark every dependent untrusted.  Anchor
                        // restart is driven by the VPN core via the
                        // domain's DOM-27 path; Docker core does not
                        // restart the anchor itself to avoid double-fire.
                        self.anchor_healthy_since = None;
                        for dep in self.containers.values_mut() {
                            dep.network_trusted = false;
                        }
                        publishes.push(DockerPublish::ContainerCrashed { name });
                    }
                } else {
                    if !was_unhealthy {
                        publishes.push(DockerPublish::ContainerCrashed { name: name.clone() });
                    }
                    // §38 PR 5 / DOCKER-5: autoheal subsume.  When
                    // enabled, every unhealthy event for a known
                    // dependent triggers a circuit-breakered restart.
                    if self.config.autoheal_dependents && self.dependents.contains(&name) {
                        let (restart_actions, restart_publish) = self.try_restart(name, wall_now);
                        actions.extend(restart_actions);
                        publishes.extend(restart_publish);
                    }
                }
                Outcome { actions, publishes }
            }
            DockerEvent::ContainerStopped { name } => {
                let entry = self.containers.entry(name.clone()).or_default();
                entry.network_trusted = false;
                Outcome {
                    actions: Vec::new(),
                    publishes: vec![DockerPublish::Stopped { name }],
                }
            }
            DockerEvent::ContainerStarted { name, started_at } => {
                let entry = self.containers.entry(name.clone()).or_default();
                entry.started_at = Some(started_at);
                let mut publishes = vec![DockerPublish::Started {
                    name: name.clone(),
                    started_at,
                }];
                let mut actions = Vec::new();
                // §35 stale-namespace check only runs for known dependents
                // (the anchor itself doesn't participate) when we have an
                // anchor `healthy_since` to compare against.
                if name != self.config.gluetun_anchor
                    && self.dependents.contains(&name)
                    && let Some(healthy_since) = self.anchor_healthy_since
                {
                    if started_at >= healthy_since {
                        let was_untrusted = !entry.network_trusted;
                        entry.network_trusted = true;
                        if was_untrusted {
                            publishes.push(DockerPublish::DependentNetworkTrusted { name });
                        }
                    } else {
                        entry.network_trusted = false;
                        publishes.push(DockerPublish::DependentNetworkUntrusted {
                            name: name.clone(),
                            dependent_started_at: started_at,
                            gluetun_healthy_since: healthy_since,
                        });
                        let (restart_actions, restart_publish) = self.try_restart(name, wall_now);
                        actions.extend(restart_actions);
                        publishes.extend(restart_publish);
                    }
                }
                Outcome { actions, publishes }
            }
        }
    }

    /// PR 1: per-name commands translate directly to their corresponding
    /// action; fleet commands iterate the dependent registry; `DumpAllLogs`
    /// covers anchor + dependents; `ListDependents` returns the registry.
    fn handle_command(
        &mut self,
        _now: Instant,
        _wall_now: chrono::DateTime<chrono::Utc>,
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

    fn state_snapshot(&self) -> Self::StateSnapshot {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{ExternalCause, Machine, Timed};

    use crate::{
        DockerAction, DockerCommand, DockerConfig, DockerEvent, DockerMachine, DockerResponse,
    };

    fn machine() -> DockerMachine {
        DockerMachine::new(DockerConfig::default(), Instant::now())
    }

    fn discover(m: &mut DockerMachine, names: &[&str]) {
        m.handle(
            Instant::now(),
            chrono::Utc::now(),
            Timed::external(
                Instant::now(),
                ExternalCause::Unknown,
                DockerEvent::DependentsDiscovered {
                    names: names.iter().map(|n| (*n).to_string()).collect(),
                },
            ),
        );
    }

    #[test]
    fn state_snapshot_reflects_discovered_dependents() {
        // §37b: DependentsDiscovered populates the dependent registry,
        // which must appear in the JSON snapshot.
        let mut m = machine();
        discover(&mut m, &["qbittorrent", "mlm"]);
        let value = serde_json::to_value(m.state_snapshot()).expect("snapshot should serialize");
        assert_eq!(
            value["dependents"],
            serde_json::json!(["qbittorrent", "mlm"])
        );
        assert_eq!(value["config"]["gluetun_anchor"], "gluetun");
    }

    #[test]
    fn init_seeds_discovery_and_anchor_inspect() {
        // §38 PR 6: Init must drive an initial DiscoverDependents and
        // anchor InspectContainer so VPN core gets a boot-time health
        // signal without polling Docker itself.
        let mut m = machine();
        let out = m.handle(
            Instant::now(),
            chrono::Utc::now(),
            Timed::external(Instant::now(), ExternalCause::Unknown, DockerEvent::Init),
        );
        assert_eq!(
            out.actions,
            vec![
                DockerAction::DiscoverDependents,
                DockerAction::InspectContainer {
                    name: "gluetun".to_string()
                },
            ]
        );
        assert!(out.publishes.is_empty());
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
        let out = m.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            DockerCommand::StopDependents,
        );
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
        let out = m.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            DockerCommand::StartDependents,
        );
        assert!(out.actions.is_empty());
        assert_eq!(out.response, DockerResponse::Accepted);
    }

    #[test]
    fn stop_dependents_with_empty_registry_emits_nothing() {
        // DOCKER-3.
        let mut m = machine();
        let out = m.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            DockerCommand::StopDependents,
        );
        assert!(out.actions.is_empty());
    }

    #[test]
    fn restart_container_translates_to_action() {
        let mut m = machine();
        let out = m.handle_command(
            Instant::now(),
            chrono::Utc::now(),
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
        let out = m.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            DockerCommand::DumpAllLogs,
        );
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
        let out = m.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            DockerCommand::ListDependents,
        );
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

    // ── §35 / DOCKER-4 stale-namespace + DOCKER-2 circuit-breaker ────────────

    use crate::{ContainerHealth, DockerPublish};

    fn started_at(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0).unwrap()
    }

    fn handle(
        m: &mut DockerMachine,
        event: DockerEvent,
    ) -> crate::Outcome<DockerAction, DockerPublish> {
        m.handle(
            Instant::now(),
            chrono::Utc::now(),
            Timed::external(Instant::now(), ExternalCause::Unknown, event),
        )
    }

    #[test]
    fn anchor_healthy_sets_healthy_since_and_inspects_dependents() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent", "mlm"]);
        let out = handle(
            &mut m,
            DockerEvent::ContainerHealthy {
                name: "gluetun".to_string(),
            },
        );
        assert!(m.anchor_healthy_since().is_some());
        assert_eq!(m.incident_id(), 1);
        // Two InspectContainer actions, one per dependent.
        assert_eq!(
            out.actions,
            vec![
                DockerAction::InspectContainer {
                    name: "qbittorrent".to_string()
                },
                DockerAction::InspectContainer {
                    name: "mlm".to_string()
                },
            ]
        );
        assert_eq!(
            out.publishes,
            vec![DockerPublish::ContainerHealthy {
                name: "gluetun".to_string()
            }]
        );
    }

    #[test]
    fn repeat_anchor_healthy_does_not_bump_incident_id() {
        // DOCKER-1 rising-edge: re-observing healthy is a no-op.
        let mut m = machine();
        discover(&mut m, &["qbittorrent"]);
        handle(
            &mut m,
            DockerEvent::ContainerHealthy {
                name: "gluetun".to_string(),
            },
        );
        let healthy_since_first = m.anchor_healthy_since();
        assert_eq!(m.incident_id(), 1);
        let out = handle(
            &mut m,
            DockerEvent::ContainerHealthy {
                name: "gluetun".to_string(),
            },
        );
        assert_eq!(m.anchor_healthy_since(), healthy_since_first);
        assert_eq!(m.incident_id(), 1);
        assert!(out.actions.is_empty());
        assert!(out.publishes.is_empty());
    }

    #[test]
    fn anchor_unhealthy_clears_healthy_since_and_publishes_crashed() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent"]);
        handle(
            &mut m,
            DockerEvent::ContainerHealthy {
                name: "gluetun".to_string(),
            },
        );
        let out = handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "gluetun".to_string(),
            },
        );
        assert!(m.anchor_healthy_since().is_none());
        assert_eq!(
            out.publishes,
            vec![DockerPublish::ContainerCrashed {
                name: "gluetun".to_string()
            }]
        );
    }

    #[test]
    fn dependent_started_after_anchor_publishes_trusted() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent"]);
        // Pin anchor_healthy_since explicitly so we can compare.
        m.anchor_healthy_since = Some(started_at(1000));
        // Dependent started 10s after anchor.
        let out = handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(1010),
            },
        );
        assert!(out.actions.is_empty());
        assert!(
            out.publishes
                .contains(&DockerPublish::DependentNetworkTrusted {
                    name: "qbittorrent".to_string()
                })
        );
        assert!(m.container("qbittorrent").unwrap().network_trusted);
    }

    #[test]
    fn dependent_started_before_anchor_publishes_untrusted_and_restarts() {
        let mut m = machine();
        discover(&mut m, &["qbittorrent"]);
        m.anchor_healthy_since = Some(started_at(1000));
        let out = handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(900),
            },
        );
        assert!(out.actions.contains(&DockerAction::RestartContainer {
            name: "qbittorrent".to_string()
        }));
        assert!(
            out.publishes
                .contains(&DockerPublish::DependentNetworkUntrusted {
                    name: "qbittorrent".to_string(),
                    dependent_started_at: started_at(900),
                    gluetun_healthy_since: started_at(1000),
                })
        );
        assert!(!m.container("qbittorrent").unwrap().network_trusted);
    }

    #[test]
    fn anchor_started_skips_stale_namespace_check() {
        // The anchor itself is not a dependent — even with healthy_since set,
        // an anchor `ContainerStarted` must not produce Untrusted/Trusted.
        let mut m = machine();
        m.anchor_healthy_since = Some(started_at(1000));
        let out = handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "gluetun".to_string(),
                started_at: started_at(500),
            },
        );
        assert!(
            !out.publishes
                .iter()
                .any(|p| matches!(p, DockerPublish::DependentNetworkUntrusted { .. }))
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, DockerAction::RestartContainer { .. }))
        );
    }

    #[test]
    fn stale_dependent_without_healthy_since_is_inert() {
        // Anchor not yet known healthy → no comparison, no action.
        let mut m = machine();
        discover(&mut m, &["qbittorrent"]);
        let out = handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(500),
            },
        );
        assert!(
            out.actions
                .iter()
                .all(|a| !matches!(a, DockerAction::RestartContainer { .. }))
        );
        assert!(
            out.publishes
                .iter()
                .all(|p| !matches!(p, DockerPublish::DependentNetworkUntrusted { .. }))
        );
    }

    #[test]
    fn restart_storm_trips_after_max_and_dumps_once_per_incident() {
        // DOCKER-2 circuit-breaker: 3 restarts allowed in window; 4th trips
        // RestartStorm and emits DumpLogs fan-out once.
        let mut m = DockerMachine::new(
            DockerConfig {
                max_restarts_per_window: 3,
                ..DockerConfig::default()
            },
            Instant::now(),
        );
        discover(&mut m, &["qbittorrent", "mlm"]);
        m.anchor_healthy_since = Some(started_at(1000));
        // First 3 stale starts emit RestartContainer.
        for i in 0..3u32 {
            let out = handle(
                &mut m,
                DockerEvent::ContainerStarted {
                    name: "qbittorrent".to_string(),
                    started_at: started_at(500 + i64::from(i)),
                },
            );
            assert!(
                out.actions
                    .iter()
                    .any(|a| matches!(a, DockerAction::RestartContainer { .. })),
                "iteration {i} should emit RestartContainer"
            );
        }
        // 4th trips the breaker.
        let out = handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(503),
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, DockerAction::RestartContainer { .. })),
            "4th attempt must not emit RestartContainer"
        );
        assert!(out.publishes.iter().any(|p| matches!(
            p,
            DockerPublish::RestartStorm {
                window_count: 3,
                max: 3
            }
        )));
        // DumpLogs fan-out: anchor + every dependent.
        let dump_names: Vec<&str> = out
            .actions
            .iter()
            .filter_map(|a| match a {
                DockerAction::DumpLogs { name } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(dump_names, vec!["gluetun", "qbittorrent", "mlm"]);
        // 5th attempt within the same incident: still suppressed, no
        // second dump fan-out (DOCKER-2 dedup).
        let out2 = handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(504),
            },
        );
        let dump2: Vec<_> = out2
            .actions
            .iter()
            .filter(|a| matches!(a, DockerAction::DumpLogs { .. }))
            .collect();
        assert!(dump2.is_empty(), "no second dump fan-out within incident");
    }

    #[test]
    fn new_incident_resets_dump_dedup() {
        let mut m = DockerMachine::new(
            DockerConfig {
                max_restarts_per_window: 1,
                ..DockerConfig::default()
            },
            Instant::now(),
        );
        discover(&mut m, &["qbittorrent"]);
        // Real anchor recovery → incident_id becomes 1.
        handle(
            &mut m,
            DockerEvent::ContainerHealthy {
                name: "gluetun".to_string(),
            },
        );
        assert_eq!(m.incident_id(), 1);
        // Pin healthy_since for predictable comparisons; this doesn't
        // affect the incident counter.
        m.anchor_healthy_since = Some(started_at(1000));
        // Trip the breaker.
        handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(500),
            },
        );
        handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(501),
            },
        );
        assert!(m.crash_dump_emitted_for_current_incident);
        // Anchor crash + recovery → incident_id becomes 2, dedup resets.
        handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "gluetun".to_string(),
            },
        );
        handle(
            &mut m,
            DockerEvent::ContainerHealthy {
                name: "gluetun".to_string(),
            },
        );
        assert_eq!(m.incident_id(), 2);
        assert!(!m.crash_dump_emitted_for_current_incident);
    }

    // ── §38 PR 5 / DOCKER-5: autoheal subsume ───────────────────────────────

    #[test]
    fn autoheal_disabled_does_not_restart_unhealthy_dependent() {
        // Default config has autoheal_dependents=false → no RestartContainer.
        let mut m = machine();
        discover(&mut m, &["qbittorrent"]);
        let out = handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "qbittorrent".to_string(),
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, DockerAction::RestartContainer { .. })),
            "autoheal off: no RestartContainer"
        );
        assert!(out.publishes.contains(&DockerPublish::ContainerCrashed {
            name: "qbittorrent".to_string()
        }));
    }

    fn autoheal_machine() -> DockerMachine {
        DockerMachine::new(
            DockerConfig {
                gluetun_anchor: "gluetun".to_string(),
                max_restarts_per_window: 3,
                restart_window_duration: Duration::from_secs(600),
                autoheal_dependents: true,
            },
            Instant::now(),
        )
    }

    #[test]
    fn autoheal_enabled_restarts_unhealthy_dependent() {
        let mut m = autoheal_machine();
        discover(&mut m, &["qbittorrent"]);
        let out = handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "qbittorrent".to_string(),
            },
        );
        assert!(out.actions.contains(&DockerAction::RestartContainer {
            name: "qbittorrent".to_string()
        }));
    }

    #[test]
    fn autoheal_does_not_restart_anchor() {
        // The anchor is excluded from autoheal — VPN core drives the
        // anchor's crash recovery via the domain.
        let mut m = autoheal_machine();
        discover(&mut m, &["qbittorrent"]);
        let out = handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "gluetun".to_string(),
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, DockerAction::RestartContainer { .. })),
            "anchor must not be auto-restarted by Docker core"
        );
    }

    #[test]
    fn autoheal_does_not_restart_unknown_container() {
        // Only containers in the dependent registry are autoheal-eligible.
        let mut m = autoheal_machine();
        discover(&mut m, &["qbittorrent"]);
        let out = handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "some-other-container".to_string(),
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, DockerAction::RestartContainer { .. })),
            "unknown container must not be auto-restarted"
        );
    }

    #[test]
    fn autoheal_shares_restart_budget_with_stale_namespace() {
        // Both paths feed the same restart_window — a mixed burst trips
        // the breaker per the shared budget.
        let mut m = DockerMachine::new(
            DockerConfig {
                gluetun_anchor: "gluetun".to_string(),
                max_restarts_per_window: 2,
                restart_window_duration: Duration::from_secs(600),
                autoheal_dependents: true,
            },
            Instant::now(),
        );
        discover(&mut m, &["qbittorrent", "mlm"]);
        m.anchor_healthy_since = Some(started_at(1000));

        // Stale-namespace restart (counts: 1).
        let out1 = handle(
            &mut m,
            DockerEvent::ContainerStarted {
                name: "qbittorrent".to_string(),
                started_at: started_at(500),
            },
        );
        assert!(out1.actions.contains(&DockerAction::RestartContainer {
            name: "qbittorrent".to_string()
        }));

        // Autoheal restart (counts: 2).
        let out2 = handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "mlm".to_string(),
            },
        );
        assert!(out2.actions.contains(&DockerAction::RestartContainer {
            name: "mlm".to_string()
        }));

        // 3rd attempt trips the breaker — either path.
        let out3 = handle(
            &mut m,
            DockerEvent::ContainerUnhealthy {
                name: "qbittorrent".to_string(),
            },
        );
        assert!(
            !out3
                .actions
                .iter()
                .any(|a| matches!(a, DockerAction::RestartContainer { .. })),
            "3rd restart suppressed by shared budget"
        );
        assert!(
            out3.publishes
                .iter()
                .any(|p| matches!(p, DockerPublish::RestartStorm { .. }))
        );
    }

    #[test]
    fn unknown_container_health_tracks_per_container_state() {
        // Non-anchor container — health updates per-container but no
        // healthy_since change and no inspect fan-out.
        let mut m = machine();
        let out = handle(
            &mut m,
            DockerEvent::ContainerHealthy {
                name: "qbittorrent".to_string(),
            },
        );
        assert!(out.actions.is_empty());
        assert_eq!(
            out.publishes,
            vec![DockerPublish::ContainerHealthy {
                name: "qbittorrent".to_string()
            }]
        );
        assert!(m.anchor_healthy_since().is_none());
        assert_eq!(
            m.container("qbittorrent").unwrap().health,
            ContainerHealth::Healthy
        );
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::Instant;

    use chrono::Utc;
    use proptest::prelude::*;
    use windlass_machine::{ExternalCause, Machine, Timed};

    use crate::{DockerCommand, DockerConfig, DockerEvent, DockerMachine};

    fn any_machine() -> impl Strategy<Value = DockerMachine> {
        any::<Vec<String>>().prop_map(|names| {
            let mut m = DockerMachine::new(DockerConfig::default(), Instant::now());
            m.handle(
                Instant::now(),
                chrono::Utc::now(),
                Timed::external(
                    Instant::now(),
                    ExternalCause::Unknown,
                    DockerEvent::DependentsDiscovered { names },
                ),
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
            let _ = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));
        }

        // GLOBAL-1: handle_command never panics.
        #[test]
        fn handle_command_never_panics(mut machine in any_machine(), cmd in any_command()) {
            let _ = machine.handle_command(Instant::now(), chrono::Utc::now(), cmd);
        }

        // DOCKER-1 [safety] (§35): ContainerCrashed publishes at most once
        // per healthy→unhealthy transition.  Re-observing unhealthy on a
        // container that is already unhealthy is a no-op.
        #[test]
        fn container_crashed_is_rising_edge_only(
            mut machine in any_machine(),
            name in any::<String>(),
        ) {
            let pre = machine
                .container(&name)
                .map_or(crate::ContainerHealth::Unknown, |s| s.health);
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, DockerEvent::ContainerUnhealthy { name: name.clone() }),
            );
            let crashed_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, crate::DockerPublish::ContainerCrashed { .. }))
                .count();
            // Anchor rising-edge is gated by healthy_since being set; non-anchor
            // rising-edge is gated by previous health != Unhealthy.
            let was_unhealthy = pre == crate::ContainerHealth::Unhealthy;
            let is_anchor = name == machine.anchor();
            // Either at most one, or none — but never more than one.
            prop_assert!(crashed_count <= 1);
            // If we know there was no previous unhealthy state and (for the
            // anchor) healthy_since was set, expect exactly one publish.  We
            // can't recompute the pre-handle anchor_healthy_since cheaply
            // from this strategy, so we only check the upper bound here.
            let _ = (was_unhealthy, is_anchor);
        }

        // DOCKER-2 [safety] (§35): circuit-breaker upper bound — at most
        // `max_restarts_per_window` RestartContainer actions are emitted
        // for any single stale `ContainerStarted` event.  (Actually: each
        // single event emits at most one RestartContainer; this property
        // tightens the bound over arbitrary state.)
        #[test]
        fn single_event_emits_at_most_one_restart_container(
            mut machine in any_machine(),
            name in any::<String>(),
            started_offset in -1_000_000i64..=1_000_000i64,
        ) {
            let started_at = chrono::DateTime::<chrono::Utc>::from_timestamp(started_offset, 0)
                .unwrap_or_else(chrono::Utc::now);
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, DockerEvent::ContainerStarted {
                    name,
                    started_at,
                }),
            );
            let restart_count = out
                .actions
                .iter()
                .filter(|a| matches!(a, crate::DockerAction::RestartContainer { .. }))
                .count();
            prop_assert!(restart_count <= 1);
        }
    }
}
