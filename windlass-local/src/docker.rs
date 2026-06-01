use bollard::Docker;
use bollard::container::ListContainersOptions;
use bollard::container::{LogsOptions, RestartContainerOptions};
use bollard::models::HealthStatusEnum;
use futures_util::StreamExt;
use tracing::{error, info, warn};

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

    /// Connects, probes Gluetun state, and discovers dependents.  Returns
    /// the boot snapshot needed for the legacy `Event::Init` bridge.
    /// §36 step 9b: the legacy bollard event watcher
    /// (`spawn_event_watcher`) is retired — `DockerShell` (in the
    /// windlass binary) owns its own watcher that feeds `DockerMachine`
    /// directly; the legacy path is redundant.
    ///
    /// # Errors
    /// Returns an error if the Docker socket is unavailable.
    pub async fn boot(dump_dir: String) -> anyhow::Result<(Self, DockerBootInfo)> {
        let client = Self::connect(dump_dir)?;
        let is_gluetun_healthy = client.is_gluetun_healthy().await;
        let dependents = client.discover_dependents().await;
        info!(
            gluetun_healthy = is_gluetun_healthy,
            dependents = ?dependents,
            "Docker ready"
        );
        Ok((
            client,
            DockerBootInfo {
                is_gluetun_healthy,
                dependents,
            },
        ))
    }

    /// Returns a reference to the underlying bollard handle so callers
    /// outside this crate (e.g. `windlass-docker-core`'s shell) can issue
    /// daemon operations directly without going through the legacy
    /// `DockerClient` methods.
    #[must_use]
    pub const fn bollard(&self) -> &Docker {
        &self.inner
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

#[cfg(test)]
#[path = "docker_tests.rs"]
mod tests;
