use bollard::Docker;
use bollard::container::ListContainersOptions;
use bollard::container::{LogsOptions, RestartContainerOptions};
use bollard::models::{EventMessageTypeEnum, HealthStatusEnum};
use chrono::Utc;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use windlass_core::events::Event;

// Re-exported so docker_tests.rs (via `use super::*`) can use it in Tier 4 tests.
pub use bollard::container::StopContainerOptions;

const GLUETUN: &str = "gluetun";

/// Wraps a `bollard::Docker` connection together with the project-level
/// configuration it needs. All Docker operations are methods so call sites
/// only pass `&self` instead of a long argument list.
#[derive(Clone)]
pub struct DockerClient {
    pub(crate) inner: Docker,
    pub gluetun_anchor: String,
    pub dump_dir: String,
}

/// Returned by [`DockerClient::boot`] — boot-time state needed to fire `Event::Init`.
pub struct DockerBootInfo {
    pub is_gluetun_healthy: bool,
    pub dependents: Vec<String>,
}

impl DockerClient {
    /// Connects to the Docker socket using the default system path.
    ///
    /// # Errors
    /// Returns an error if the Docker socket is unavailable.
    pub fn connect(dump_dir: String) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Docker::connect_with_socket_defaults()?,
            gluetun_anchor: GLUETUN.to_string(),
            dump_dir,
        })
    }

    /// Connects, probes Gluetun state, discovers dependents, and spawns the
    /// Docker event watcher. Returns the boot state needed for `Event::Init`.
    ///
    /// # Errors
    /// Returns an error if the Docker socket is unavailable.
    pub async fn boot(
        dump_dir: String,
        tx: mpsc::Sender<Event>,
    ) -> anyhow::Result<(Self, DockerBootInfo)> {
        let client = Self::connect(dump_dir)?;
        let is_gluetun_healthy = client.is_gluetun_healthy().await;
        let dependents = client.discover_dependents().await;
        info!(
            gluetun_healthy = is_gluetun_healthy,
            dependents = ?dependents,
            "Docker ready"
        );
        client.spawn_event_watcher(tx);
        Ok((
            client,
            DockerBootInfo {
                is_gluetun_healthy,
                dependents,
            },
        ))
    }

    // ── Boot helpers ──────────────────────────────────────────────────────────

    pub async fn is_gluetun_healthy(&self) -> bool {
        self.is_container_healthy(&self.gluetun_anchor).await
    }

    /// Returns all containers sharing Gluetun's network namespace.
    /// Falls back to an empty list on error — actions will be no-ops, which is safe.
    pub async fn discover_dependents(&self) -> Vec<String> {
        // Resolve the anchor's container ID so we match both forms Docker may store:
        //   "container:<name>" — written by docker-compose
        //   "container:<id>"   — written by plain `docker run --network container:<name>`
        let anchor = &self.gluetun_anchor;
        let anchor_id = self
            .inner
            .inspect_container(anchor, None)
            .await
            .ok()
            .and_then(|info| info.id)
            .unwrap_or_default();

        let by_name = format!("container:{anchor}");
        let by_id = format!("container:{anchor_id}");

        let options = ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        };
        let Ok(containers) = self.inner.list_containers(Some(options)).await else {
            warn!("Failed to discover dependent containers, falling back to empty list");
            return vec![];
        };
        containers
            .into_iter()
            .filter(|c| {
                c.host_config
                    .as_ref()
                    .and_then(|hc| hc.network_mode.as_deref())
                    .is_some_and(|nm| nm == by_name || nm == by_id)
            })
            .filter_map(|c| {
                c.names?
                    .into_iter()
                    .next()
                    .map(|n| n.trim_start_matches('/').to_string())
            })
            .collect()
    }

    // ── Background watchers ───────────────────────────────────────────────────

    /// Spawns a background task that streams Docker events and forwards
    /// Gluetun health/die events to the Core via `tx`.
    pub fn spawn_event_watcher(&self, tx: mpsc::Sender<Event>) {
        let docker = self.inner.clone();
        let anchor = self.gluetun_anchor.clone();
        spawn_health_poll_watcher(docker.clone(), anchor.clone(), tx.clone());
        tokio::spawn(async move {
            loop {
                let mut stream = docker.events(None::<bollard::system::EventsOptions<String>>);
                let mut last_err: Option<String> = None;
                while let Some(result) = stream.next().await {
                    match result {
                        Err(e) => {
                            last_err = Some(e.to_string());
                        }
                        Ok(msg) => {
                            last_err = None;
                            if msg.typ != Some(EventMessageTypeEnum::CONTAINER) {
                                continue;
                            }
                            let name = msg
                                .actor
                                .as_ref()
                                .and_then(|a| a.attributes.as_ref())
                                .and_then(|attrs| attrs.get("name"))
                                .map(String::as_str)
                                .unwrap_or_default();

                            if name != anchor {
                                continue;
                            }

                            let action = msg.action.as_deref().unwrap_or("");
                            let event = if action.starts_with("health_status: healthy") {
                                Event::DockerGluetunHealthy { at: Utc::now() }
                            } else if action.starts_with("health_status: unhealthy")
                                || action == "die"
                            {
                                Event::DockerGluetunDied { at: Utc::now() }
                            } else {
                                continue;
                            };

                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                }
                if let Some(err) = last_err {
                    warn!("Docker event stream ended with error: {err}. Reconnecting in 5s...");
                } else {
                    warn!("Docker event stream closed cleanly. Reconnecting in 5s...");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

    // ── Container lifecycle ───────────────────────────────────────────────────

    pub async fn is_container_healthy(&self, name: &str) -> bool {
        let Ok(info) = self.inner.inspect_container(name, None).await else {
            return false;
        };
        matches!(
            info.state.and_then(|s| s.health).and_then(|h| h.status),
            Some(HealthStatusEnum::HEALTHY)
        )
    }

    pub async fn stop_dependents(&self, dependents: &[String]) {
        for name in dependents {
            let options = StopContainerOptions { t: 10 };
            if let Err(e) = self.inner.stop_container(name, Some(options)).await {
                warn!("Failed to stop {name}: {e}");
            }
        }
    }

    pub async fn start_dependents(&self, dependents: &[String]) {
        for name in dependents {
            if let Err(e) = self
                .inner
                .start_container(
                    name,
                    None::<bollard::container::StartContainerOptions<String>>,
                )
                .await
            {
                warn!("Failed to start {name}: {e}");
            }
        }
    }

    pub async fn restart_gluetun(&self) {
        let options = RestartContainerOptions { t: 0 };
        if let Err(e) = self
            .inner
            .restart_container(&self.gluetun_anchor, Some(options))
            .await
        {
            error!("Failed to restart gluetun: {e}");
        }
    }

    pub async fn fetch_and_dump_logs(&self, dependents: &[String]) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        if let Err(e) = tokio::fs::create_dir_all(&self.dump_dir).await {
            warn!("Could not create dump dir {}: {e}", self.dump_dir);
        }

        let containers = std::iter::once(self.gluetun_anchor.as_str())
            .chain(dependents.iter().map(String::as_str));

        for name in containers {
            let options = LogsOptions::<String> {
                stdout: true,
                stderr: true,
                tail: "200".to_string(),
                timestamps: true,
                ..Default::default()
            };
            let file_path = format!("{}/crash_{timestamp}_{name}.log", self.dump_dir);
            let mut lines = Vec::new();
            let mut stream = self.inner.logs(name, Some(options));
            while let Some(item) = stream.next().await {
                match item {
                    Ok(line) => lines.push(line.to_string()),
                    Err(e) => warn!("Log error for {name}: {e}"),
                }
            }
            let content = lines.join("\n");
            if let Err(e) = tokio::fs::write(&file_path, &content).await {
                warn!("Failed to write dump file {file_path}: {e}");
            } else {
                info!("Dumped {name} logs → {file_path} ({} lines)", lines.len());
            }
        }
    }
}

fn spawn_health_poll_watcher(docker: Docker, anchor: String, tx: mpsc::Sender<Event>) {
    tokio::spawn(async move {
        let mut last_healthy: Option<bool> = None;
        loop {
            let healthy = docker
                .inspect_container(&anchor, None)
                .await
                .ok()
                .and_then(|info| info.state)
                .and_then(|state| state.health)
                .and_then(|health| health.status)
                == Some(HealthStatusEnum::HEALTHY);

            let event = match last_healthy.replace(healthy) {
                Some(true) if !healthy => Some(Event::DockerGluetunDied { at: Utc::now() }),
                Some(false) if healthy => Some(Event::DockerGluetunHealthy { at: Utc::now() }),
                _ => None,
            };
            if let Some(event) = event
                && tx.send(event).await.is_err()
            {
                return;
            }

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

#[cfg(test)]
#[path = "docker_tests.rs"]
mod tests;
