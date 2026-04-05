use bollard::Docker;
use bollard::container::{LogsOptions, RestartContainerOptions, StopContainerOptions};
use bollard::models::{EventMessageTypeEnum, HealthStatusEnum};
use bollard::container::ListContainersOptions;
use futures_util::StreamExt;
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::net::Ipv4Addr;
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::core::events::Event;
use crate::types::{VpnIp, VpnPort};

const GLUETUN: &str = "gluetun";

pub fn connect() -> anyhow::Result<Docker> {
    Ok(Docker::connect_with_socket_defaults()?)
}

pub async fn is_gluetun_healthy(docker: &Docker) -> bool {
    is_container_healthy(docker, GLUETUN).await
}

pub(crate) async fn is_container_healthy(docker: &Docker, name: &str) -> bool {
    let Ok(info) = docker.inspect_container(name, None).await else {
        return false;
    };
    matches!(
        info.state.and_then(|s| s.health).and_then(|h| h.status),
        Some(HealthStatusEnum::HEALTHY)
    )
}

pub async fn stop_container(docker: &Docker, name: &str) {
    let options = StopContainerOptions { t: 10 };
    if let Err(e) = docker.stop_container(name, Some(options)).await {
        warn!("Failed to stop {name}: {e}");
    }
}

pub async fn start_container(docker: &Docker, name: &str) {
    if let Err(e) = docker.start_container(name, None::<bollard::container::StartContainerOptions<String>>).await {
        warn!("Failed to start {name}: {e}");
    }
}

pub async fn restart_gluetun(docker: &Docker) {
    let options = RestartContainerOptions { t: 0 };
    if let Err(e) = docker.restart_container(GLUETUN, Some(options)).await {
        error!("Failed to restart gluetun: {e}");
    }
}

/// Discovers containers that share Gluetun's network namespace.
/// Called once at boot; result is stored in the Shell for the lifetime of the process.
/// Falls back to an empty list on error — actions will be no-ops, which is safe.
pub async fn discover_dependents(docker: &Docker) -> Vec<String> {
    discover_dependents_for(docker, GLUETUN).await
}

pub(crate) async fn discover_dependents_for(docker: &Docker, anchor: &str) -> Vec<String> {
    // Resolve the anchor's container ID so we match both forms Docker may store:
    //   "container:<name>" — written by docker-compose
    //   "container:<id>"   — written by plain `docker run --network container:<name>`
    let anchor_id = docker
        .inspect_container(anchor, None)
        .await
        .ok()
        .and_then(|info| info.id)
        .unwrap_or_default();

    let by_name = format!("container:{anchor}");
    let by_id = format!("container:{anchor_id}");

    let options = ListContainersOptions::<String> { all: true, ..Default::default() };
    let Ok(containers) = docker.list_containers(Some(options)).await else {
        warn!("Failed to discover dependent containers, falling back to empty list");
        return vec![];
    };
    containers
        .into_iter()
        .filter(|c| {
            c.host_config
                .as_ref()
                .and_then(|hc| hc.network_mode.as_deref())
                .map(|nm| nm == by_name || nm == by_id)
                .unwrap_or(false)
        })
        .filter_map(|c| {
            c.names?
                .into_iter()
                .next()
                .map(|n| n.trim_start_matches('/').to_string())
        })
        .collect()
}

pub async fn stop_dependents(docker: &Docker, dependents: &[String]) {
    for name in dependents {
        stop_container(docker, name).await;
    }
}

pub async fn start_dependents(docker: &Docker, dependents: &[String]) {
    for name in dependents {
        start_container(docker, name).await;
    }
}

pub async fn fetch_and_dump_logs(docker: &Docker, dump_dir: &str, dependents: &[String]) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if let Err(e) = tokio::fs::create_dir_all(dump_dir).await {
        warn!("Could not create dump dir {dump_dir}: {e}");
    }

    let containers = std::iter::once(GLUETUN).chain(dependents.iter().map(String::as_str));
    for name in containers {
        let options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            tail: "200".to_string(),
            timestamps: true,
            ..Default::default()
        };

        let file_path = format!("{dump_dir}/crash_{timestamp}_{name}.log");
        let mut lines = Vec::new();
        let mut stream = docker.logs(name, Some(options));
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

/// Spawns a background task that streams Docker events and translates
/// Gluetun health/die events into Core events.
pub fn spawn_event_watcher(docker: Docker, tx: mpsc::Sender<Event>) {
    tokio::spawn(async move {
        loop {
            let mut stream = docker.events(None::<bollard::system::EventsOptions<String>>);
            let mut last_err: Option<String> = None;
            while let Some(result) = stream.next().await {
                match result {
                    Err(e) => { last_err = Some(e.to_string()); }
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

                        if name != GLUETUN {
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

/// Spawns a background task that watches the Gluetun directory for file changes.
/// Uses `notify-debouncer-mini` to collapse the inotify storm from a single write
/// into one event per 100ms window. After the storm settles, the Shell reads both
/// VPN files immediately and emits `PortFileReadResult` — no Core round-trip needed.
pub fn spawn_file_watcher(
    watch_dir: String,
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
        .watch(Path::new(&watch_dir), RecursiveMode::NonRecursive)
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

            // Deduplicate: skip sending if the content is identical to the last
            // successful send — prevents feedback loops from read-triggered
            // inotify events firing the debouncer again.
            if let Ok(ref val) = result {
                if last_sent.as_ref() == Some(val) {
                    continue;
                }
                last_sent = Some(val.clone());
            }

            if tx.send(Event::PortFileReadResult(result)).await.is_err() {
                break;
            }
        }
    });
}

/// Reads and parses both VPN files atomically. Returns `Err` if either
/// file is missing, empty, or unparseable — the Core will retry.
pub fn read_port_files(
    ip_file: &str,
    port_file: &str,
) -> Result<(VpnIp, VpnPort), String> {
    let ip_str = std::fs::read_to_string(ip_file)
        .map_err(|e| format!("ip file: {e}"))?;
    let port_str = std::fs::read_to_string(port_file)
        .map_err(|e| format!("port file: {e}"))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{content}").unwrap();
        f
    }

    #[test]
    fn read_port_files_parses_valid_input() {
        let ip_f = write_temp("10.8.0.1");
        let port_f = write_temp("51820");
        let (ip, port) = read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(ip.0.to_string(), "10.8.0.1");
        assert_eq!(port.into_inner(), 51820);
    }

    #[test]
    fn read_port_files_trims_trailing_whitespace() {
        let ip_f = write_temp("  10.8.0.1  ");
        let port_f = write_temp("  51820  ");
        assert!(read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .is_ok());
    }

    #[test]
    fn read_port_files_missing_ip_file_returns_err() {
        let port_f = write_temp("51820");
        let err = read_port_files(
            "/nonexistent/ip_xyz",
            port_f.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("ip file"), "unexpected error: {err}");
    }

    #[test]
    fn read_port_files_missing_port_file_returns_err() {
        let ip_f = write_temp("10.8.0.1");
        let err = read_port_files(
            ip_f.path().to_str().unwrap(),
            "/nonexistent/port_xyz",
        )
        .unwrap_err();
        assert!(err.contains("port file"), "unexpected error: {err}");
    }

    #[test]
    fn read_port_files_malformed_ip_returns_err() {
        let ip_f = write_temp("not-an-ip");
        let port_f = write_temp("51820");
        let err = read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("ip parse"), "unexpected error: {err}");
    }

    #[test]
    fn read_port_files_malformed_port_returns_err() {
        let ip_f = write_temp("10.8.0.1");
        let port_f = write_temp("notaport");
        let err = read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("port parse"), "unexpected error: {err}");
    }

    // ── Tier 3: File-system integration ──────────────────────────────────────
    // These tests use the real filesystem and tokio runtime but need no Docker.

    #[tokio::test]
    async fn file_watcher_fires_port_file_result_on_write() {
        use std::time::Duration;

        let dir = tempfile::TempDir::new().unwrap();
        let ip_path = dir.path().join("ip");
        let port_path = dir.path().join("forwarded_port");
        let (tx, mut rx) = mpsc::channel(8);

        // Write both files before starting the watcher so the first debounced
        // read succeeds (both files must be present for a successful parse).
        std::fs::write(&ip_path, "10.8.0.1").unwrap();
        std::fs::write(&port_path, "51820").unwrap();

        spawn_file_watcher(
            dir.path().to_str().unwrap().to_string(),
            ip_path.to_str().unwrap().to_string(),
            port_path.to_str().unwrap().to_string(),
            tx,
        );

        // Give the debouncer time to register the watch before writing.
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Simulate Gluetun writing a new port.
        std::fs::write(&port_path, "51821").unwrap();

        let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out waiting for PortFileReadResult")
            .expect("channel closed unexpectedly");

        // The debouncer collapses the inotify storm into one PortFileReadResult.
        let expected_port = VpnPort::try_new(51821).unwrap();
        assert!(
            matches!(event, Event::PortFileReadResult(Ok((_, p))) if p == expected_port),
            "expected PortFileReadResult(Ok(_, 51821)), got {event:?}"
        );
    }

    #[tokio::test]
    async fn file_watcher_fires_exactly_once_per_write() {
        use std::time::Duration;

        let dir = tempfile::TempDir::new().unwrap();
        let ip_path = dir.path().join("ip");
        let port_path = dir.path().join("forwarded_port");
        std::fs::write(&ip_path, "10.8.0.1").unwrap();
        std::fs::write(&port_path, "51820").unwrap();

        let (tx, mut rx) = mpsc::channel(32);
        spawn_file_watcher(
            dir.path().to_str().unwrap().to_string(),
            ip_path.to_str().unwrap().to_string(),
            port_path.to_str().unwrap().to_string(),
            tx,
        );
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Write — this may trigger multiple raw inotify events.
        std::fs::write(&port_path, "51821").unwrap();

        // Wait for debounce window + a little extra to collect any extra firings.
        tokio::time::sleep(Duration::from_millis(350)).await;

        let mut count = 0;
        while let Ok(Some(_)) =
            tokio::time::timeout(Duration::from_millis(10), rx.recv()).await
        {
            count += 1;
        }
        assert_eq!(count, 1, "debouncer must emit exactly 1 event per write burst, got {count}");
    }

    #[tokio::test]
    async fn file_watcher_fires_on_subsequent_writes() {
        use std::time::Duration;

        let dir = tempfile::TempDir::new().unwrap();
        let ip_path = dir.path().join("ip");
        let port_path = dir.path().join("forwarded_port");
        std::fs::write(&ip_path, "10.8.0.1").unwrap();
        std::fs::write(&port_path, "51820").unwrap();

        let (tx, mut rx) = mpsc::channel(8);
        spawn_file_watcher(
            dir.path().to_str().unwrap().to_string(),
            ip_path.to_str().unwrap().to_string(),
            port_path.to_str().unwrap().to_string(),
            tx,
        );
        tokio::time::sleep(Duration::from_millis(150)).await;

        // First write
        std::fs::write(&port_path, "51821").unwrap();
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out on first write")
            .unwrap();

        // Wait for the debounce window to fully settle before triggering the second
        // write — avoids the drain loop consuming the second event.
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Second write — watcher must still be alive.
        std::fs::write(&port_path, "51822").unwrap();
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out on second write — watcher stopped after first event")
            .unwrap();
    }

    // ── Tier 4: Docker container integration ─────────────────────────────────
    // These tests start real containers via the Docker socket.
    // Run with: cargo test -- --include-ignored

    #[tokio::test]
    #[ignore = "requires Docker socket; run with: cargo test -- --include-ignored"]
    async fn docker_is_container_healthy_returns_false_for_nonexistent() {
        let docker = connect().expect("Docker socket unavailable");
        let healthy = is_container_healthy(&docker, "windlass_test_definitely_absent_xyz").await;
        assert!(!healthy);
    }

    #[tokio::test]
    #[ignore = "requires Docker socket; run with: cargo test -- --include-ignored"]
    async fn docker_discover_dependents_finds_containers_in_network_mode() {
        use bollard::container::{
            Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
        };
        use bollard::models::HostConfig;

        let docker = connect().expect("Docker socket unavailable");
        let anchor = "windlass_test_anchor";
        let dependent = "windlass_test_dep";

        // Best-effort cleanup of any previous run
        for name in [anchor, dependent] {
            let _ = docker
                .remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() }))
                .await;
        }

        // Start the anchor container
        docker
            .create_container(
                Some(CreateContainerOptions { name: anchor, platform: None }),
                Config::<String> {
                    image: Some("alpine".into()),
                    cmd: Some(vec!["sleep".into(), "30".into()]),
                    ..Default::default()
                },
            )
            .await
            .expect("create anchor container");
        docker
            .start_container(anchor, None::<StartContainerOptions<String>>)
            .await
            .expect("start anchor container");

        // Start the dependent container sharing the anchor's network
        docker
            .create_container(
                Some(CreateContainerOptions { name: dependent, platform: None }),
                Config::<String> {
                    image: Some("alpine".into()),
                    cmd: Some(vec!["sleep".into(), "30".into()]),
                    host_config: Some(HostConfig {
                        network_mode: Some(format!("container:{anchor}")),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create dependent container");
        docker
            .start_container(dependent, None::<StartContainerOptions<String>>)
            .await
            .expect("start dependent container");

        let found = discover_dependents_for(&docker, anchor).await;

        // Cleanup regardless of assertion outcome
        for name in [dependent, anchor] {
            let _ = docker.stop_container(name, Some(StopContainerOptions { t: 0 })).await;
            let _ = docker
                .remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() }))
                .await;
        }

        assert!(
            found.contains(&dependent.to_string()),
            "Expected {dependent:?} in discovered list, got: {found:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker socket; run with: cargo test -- --include-ignored"]
    async fn docker_fetch_and_dump_logs_creates_log_file() {
        use bollard::container::{
            Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
        };
        use std::time::Duration;

        let docker = connect().expect("Docker socket unavailable");
        let container_name = "windlass_test_logs";
        let dump_dir = tempfile::TempDir::new().unwrap();

        let _ = docker
            .remove_container(container_name, Some(RemoveContainerOptions { force: true, ..Default::default() }))
            .await;

        docker
            .create_container(
                Some(CreateContainerOptions { name: container_name, platform: None }),
                Config::<String> {
                    image: Some("alpine".into()),
                    cmd: Some(vec![
                        "sh".into(), "-c".into(),
                        "echo 'windlass_log_marker'; sleep 30".into(),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .expect("create log test container");
        docker
            .start_container(container_name, None::<StartContainerOptions<String>>)
            .await
            .expect("start log test container");

        // Wait for the echo to appear in logs
        tokio::time::sleep(Duration::from_millis(500)).await;

        fetch_and_dump_logs(
            &docker,
            dump_dir.path().to_str().unwrap(),
            &[container_name.to_string()],
        )
        .await;

        // Cleanup
        let _ = docker.stop_container(container_name, Some(StopContainerOptions { t: 0 })).await;
        let _ = docker
            .remove_container(container_name, Some(RemoveContainerOptions { force: true, ..Default::default() }))
            .await;

        // Verify at least one dump file was created
        let files: Vec<_> = std::fs::read_dir(dump_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(container_name))
            .collect();

        assert!(!files.is_empty(), "Expected a dump file for {container_name}");

        let content = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(
            content.contains("windlass_log_marker"),
            "Log file should contain 'windlass_log_marker', got: {content:?}"
        );
    }
}
