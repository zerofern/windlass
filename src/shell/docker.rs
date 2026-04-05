use bollard::Docker;
use bollard::container::ListContainersOptions;
use bollard::container::{LogsOptions, RestartContainerOptions, StopContainerOptions};
use bollard::models::{EventMessageTypeEnum, HealthStatusEnum};
use futures_util::StreamExt;
use notify_debouncer_mini::{DebounceEventResult, new_debouncer, notify::RecursiveMode};
use std::net::Ipv4Addr;
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::core::events::Event;
use crate::types::{VpnIp, VpnPort};

const GLUETUN: &str = "gluetun";

/// Wraps a `bollard::Docker` connection together with the project-level
/// configuration it needs. All Docker and file-watcher operations are methods
/// so call sites only pass `&self` instead of a long argument list.
#[derive(Clone)]
pub struct DockerClient {
    pub(crate) inner: Docker,
    pub gluetun_anchor: String,
    pub dump_dir: String,
    pub vpn_ip_file: String,
    pub vpn_port_file: String,
}

impl DockerClient {
    /// Connects to the Docker socket using the default system path.
    pub fn connect(
        dump_dir: String,
        vpn_ip_file: String,
        vpn_port_file: String,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Docker::connect_with_socket_defaults()?,
            gluetun_anchor: GLUETUN.to_string(),
            dump_dir,
            vpn_ip_file,
            vpn_port_file,
        })
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

    /// Reads both VPN files once at boot. Called before the event loop starts
    /// so the Core can fast-forward to connected state immediately.
    pub async fn read_boot_port_files(&self) -> Result<(VpnIp, VpnPort), String> {
        let ip_file = self.vpn_ip_file.clone();
        let port_file = self.vpn_port_file.clone();
        tokio::task::spawn_blocking(move || read_port_files(&ip_file, &port_file))
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
    }

    // ── Background watchers ───────────────────────────────────────────────────

    /// Spawns a background task that streams Docker events and forwards
    /// Gluetun health/die events to the Core via `tx`.
    pub fn spawn_event_watcher(&self, tx: mpsc::Sender<Event>) {
        let docker = self.inner.clone();
        let anchor = self.gluetun_anchor.clone();
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
                                Event::DockerGluetunHealthy
                            } else if action == "die" {
                                Event::DockerGluetunDied
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

    /// Spawns a debounced inotify watcher on the Gluetun directory.
    /// Collapses the raw inotify storm from a single write into one event per
    /// 100ms window, then reads both VPN files and emits `PortFileReadResult`.
    pub fn spawn_file_watcher(&self, tx: mpsc::Sender<Event>) {
        let watch_dir = Path::new(&self.vpn_ip_file).parent().map_or_else(
            || "/tmp/gluetun".to_string(),
            |p| p.to_string_lossy().into_owned(),
        );
        spawn_file_watcher_inner(
            &watch_dir,
            self.vpn_ip_file.clone(),
            self.vpn_port_file.clone(),
            tx,
        );
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
            .map(|d| d.as_secs())
            .unwrap_or(0);

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

// ── Free functions ────────────────────────────────────────────────────────────

/// Reads and parses both VPN files. Returns `Err` if either file is missing,
/// empty, or unparseable — the Core schedules a retry on error.
pub fn read_port_files(ip_file: &str, port_file: &str) -> Result<(VpnIp, VpnPort), String> {
    let ip_str = std::fs::read_to_string(ip_file).map_err(|e| format!("ip file: {e}"))?;
    let port_str = std::fs::read_to_string(port_file).map_err(|e| format!("port file: {e}"))?;

    let ip: Ipv4Addr = ip_str
        .trim()
        .parse()
        .map_err(|e| format!("ip parse: {e}"))?;

    let port_num: u16 = port_str
        .trim()
        .parse()
        .map_err(|e| format!("port parse: {e}"))?;

    let port = VpnPort::try_new(port_num).map_err(|e| format!("port validate: {e}"))?;

    Ok((VpnIp(ip), port))
}

/// Inner file-watcher spawn used by both `DockerClient::spawn_file_watcher`
/// and the Tier 3 tests (which construct paths manually).
pub fn spawn_file_watcher_inner(
    watch_dir: &str,
    ip_file: String,
    port_file: String,
    tx: mpsc::Sender<Event>,
) {
    // Capacity 1: if a read is already queued, drop extra signals.
    let (notify_tx, mut notify_rx) = mpsc::channel::<()>(1);

    let mut debouncer = new_debouncer(
        std::time::Duration::from_millis(100),
        move |_: DebounceEventResult| {
            // try_send: drop the signal if one is already pending so we never
            // queue more work than the processing loop can handle.
            let _ = notify_tx.try_send(());
        },
    )
    .expect("Failed to create file watcher debouncer");

    debouncer
        .watcher()
        .watch(Path::new(watch_dir), RecursiveMode::NonRecursive)
        .expect("Failed to watch gluetun dir");

    tokio::spawn(async move {
        let _debouncer = debouncer; // keep alive for the duration of the task
        let mut last_sent: Option<(VpnIp, VpnPort)> = None;
        while notify_rx.recv().await.is_some() {
            let ip_f = ip_file.clone();
            let port_f = port_file.clone();
            let result = tokio::task::spawn_blocking(move || read_port_files(&ip_f, &port_f))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));

            // Deduplicate: skip sending if content is identical to the last
            // successful send — prevents feedback loops where read-triggered
            // inotify events re-fire the debouncer.
            if let Ok(ref val) = result {
                if last_sent.as_ref() == Some(val) {
                    continue;
                }
                last_sent = Some(*val);
            }

            if tx.send(Event::PortFileReadResult(result)).await.is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
#[path = "docker_tests.rs"]
mod tests;
