//! Docker shell — translates between bollard and `DockerMachine`.
//!
//! Spawned alongside the other per-system shells in `init.rs` as part of
//! §38 PR 2.  Owns its own bollard events stream (separate from the legacy
//! `DockerClient::spawn_event_watcher`) and dispatches `DockerAction`
//! variants by calling the existing `DockerClient` primitives.
//!
//! **No behavior change yet.**  The new shell runs in parallel to the
//! legacy Docker flow; nothing in the system consumes its publishes or
//! issues its commands.  PR 3 (§35 migration) and PR 4 (crash-recovery
//! orchestration) will wire it in.
use std::time::Duration;

use bollard::Docker;
use bollard::container::{
    LogsOptions, RestartContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::models::EventMessageTypeEnum;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

use windlass_docker_core::{DockerAction, DockerEvent};
use windlass_local::docker::DockerClient;
use windlass_machine::{Shell, Timed};

pub struct DockerShellConfig {
    pub docker: DockerClient,
}

pub struct DockerShell {
    docker: DockerClient,
}

impl Shell for DockerShell {
    type Config = DockerShellConfig;
    type Event = DockerEvent;
    type Action = DockerAction;

    async fn new(config: Self::Config, event_tx: UnboundedSender<Timed<DockerEvent>>) -> Self {
        spawn_event_watcher(config.docker.bollard().clone(), event_tx);
        Self {
            docker: config.docker,
        }
    }

    fn dispatch(&mut self, action: DockerAction, event_tx: &UnboundedSender<Timed<DockerEvent>>) {
        match action {
            DockerAction::StartContainer { name } => {
                let docker = self.docker.bollard().clone();
                tokio::spawn(async move {
                    if let Err(e) = docker
                        .start_container(&name, None::<StartContainerOptions<String>>)
                        .await
                    {
                        warn!("Docker start_container({name}) failed: {e}");
                    }
                });
            }
            DockerAction::StopContainer { name } => {
                let docker = self.docker.bollard().clone();
                tokio::spawn(async move {
                    let options = StopContainerOptions { t: 10 };
                    if let Err(e) = docker.stop_container(&name, Some(options)).await {
                        warn!("Docker stop_container({name}) failed: {e}");
                    }
                });
            }
            DockerAction::RestartContainer { name } => {
                let docker = self.docker.bollard().clone();
                tokio::spawn(async move {
                    let options = RestartContainerOptions { t: 0 };
                    if let Err(e) = docker.restart_container(&name, Some(options)).await {
                        warn!("Docker restart_container({name}) failed: {e}");
                    }
                });
            }
            DockerAction::InspectContainer { name } => {
                let docker = self.docker.bollard().clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let started_at = inspect_started_at(&docker, &name).await;
                    if let Some(started_at) = started_at {
                        let _ = tx.send(Timed::now(DockerEvent::ContainerStarted {
                            name,
                            started_at,
                        }));
                    }
                });
            }
            DockerAction::DumpLogs { name } => {
                let docker = self.docker.bollard().clone();
                let dump_dir = self.docker.dump_dir.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    match dump_one(&docker, &dump_dir, &name).await {
                        Ok(path) => {
                            let _ = tx.send(Timed::now(DockerEvent::LogsDumped { name, path }));
                        }
                        Err(reason) => {
                            let _ =
                                tx.send(Timed::now(DockerEvent::LogsDumpFailed { name, reason }));
                        }
                    }
                });
            }
            DockerAction::DiscoverDependents => {
                let docker = self.docker.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let names = docker.discover_dependents().await;
                    let _ = tx.send(Timed::now(DockerEvent::DependentsDiscovered { names }));
                });
            }
        }
    }
}

/// Streams Docker daemon events for *every* container, mapping
/// `health_status` / `start` / `die` / `stop` / `kill` actions into
/// `DockerEvent::*`.  Independent of the legacy `DockerClient::
/// spawn_event_watcher` which is anchor-only and feeds `Event::Docker
/// Gluetun*` for the legacy path.
fn spawn_event_watcher(docker: Docker, event_tx: UnboundedSender<Timed<DockerEvent>>) {
    tokio::spawn(async move {
        loop {
            let mut stream = docker.events(None::<bollard::system::EventsOptions<String>>);
            while let Some(item) = stream.next().await {
                let Ok(msg) = item else { continue };
                if msg.typ != Some(EventMessageTypeEnum::CONTAINER) {
                    continue;
                }
                let Some(name) = msg
                    .actor
                    .as_ref()
                    .and_then(|a| a.attributes.as_ref())
                    .and_then(|attrs| attrs.get("name"))
                    .cloned()
                else {
                    continue;
                };
                let Some(action) = msg.action.as_deref() else {
                    continue;
                };
                let event = if action.starts_with("health_status: healthy") {
                    Some(DockerEvent::ContainerHealthy { name })
                } else if action.starts_with("health_status: unhealthy") {
                    Some(DockerEvent::ContainerUnhealthy { name })
                } else if action == "start" {
                    match inspect_started_at(&docker, &name).await {
                        Some(started_at) => {
                            Some(DockerEvent::ContainerStarted { name, started_at })
                        }
                        None => Some(DockerEvent::ContainerStarted {
                            name,
                            started_at: Utc::now(),
                        }),
                    }
                } else if action == "die" || action == "stop" || action == "kill" {
                    Some(DockerEvent::ContainerStopped { name })
                } else {
                    None
                };
                if let Some(event) = event
                    && event_tx.send(Timed::now(event)).is_err()
                {
                    return;
                }
            }
            warn!("Docker (PR2) event stream closed; reconnecting in 5s");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

async fn inspect_started_at(docker: &Docker, name: &str) -> Option<DateTime<Utc>> {
    let info = docker.inspect_container(name, None).await.ok()?;
    let raw = info.state?.started_at?;
    DateTime::parse_from_rfc3339(&raw).ok().map(Into::into)
}

/// Single-container log capture.  Mirrors the existing
/// `DockerClient::fetch_and_dump_logs` body for one container.  Inlined
/// here so the legacy fleet-dump method (still used by the legacy VPN
/// path) stays unchanged until PR 6.
async fn dump_one(docker: &Docker, dump_dir: &str, name: &str) -> Result<String, String> {
    if let Err(e) = tokio::fs::create_dir_all(dump_dir).await {
        warn!("Could not create dump dir {dump_dir}: {e}");
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let file_path = format!("{dump_dir}/crash_{timestamp}_{name}.log");
    let options = LogsOptions::<String> {
        stdout: true,
        stderr: true,
        tail: "200".to_string(),
        timestamps: true,
        ..Default::default()
    };
    let mut lines = Vec::new();
    let mut stream = docker.logs(name, Some(options));
    while let Some(item) = stream.next().await {
        match item {
            Ok(line) => lines.push(line.to_string()),
            Err(e) => return Err(format!("logs stream: {e}")),
        }
    }
    let content = lines.join("\n");
    tokio::fs::write(&file_path, &content)
        .await
        .map_err(|e| format!("write {file_path}: {e}"))?;
    info!("Dumped {name} logs → {file_path} ({} lines)", lines.len());
    Ok(file_path)
}
