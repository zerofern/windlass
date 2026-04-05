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
    assert!(
        read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .is_ok()
    );
}

#[test]
fn read_port_files_missing_ip_file_returns_err() {
    let port_f = write_temp("51820");
    let err = read_port_files("/nonexistent/ip_xyz", port_f.path().to_str().unwrap()).unwrap_err();
    assert!(err.contains("ip file"), "unexpected error: {err}");
}

#[test]
fn read_port_files_missing_port_file_returns_err() {
    let ip_f = write_temp("10.8.0.1");
    let err = read_port_files(ip_f.path().to_str().unwrap(), "/nonexistent/port_xyz").unwrap_err();
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

#[tokio::test]
async fn file_watcher_fires_port_file_result_on_write() {
    use std::time::Duration;
    let dir = tempfile::TempDir::new().unwrap();
    let ip_path = dir.path().join("ip");
    let port_path = dir.path().join("forwarded_port");
    std::fs::write(&ip_path, "10.8.0.1").unwrap();
    std::fs::write(&port_path, "51820").unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    spawn_file_watcher_inner(
        dir.path().to_str().unwrap(),
        ip_path.to_str().unwrap().to_string(),
        port_path.to_str().unwrap().to_string(),
        tx,
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    std::fs::write(&port_path, "51821").unwrap();
    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timed out waiting for PortFileReadResult")
        .expect("channel closed unexpectedly");
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
    spawn_file_watcher_inner(
        dir.path().to_str().unwrap(),
        ip_path.to_str().unwrap().to_string(),
        port_path.to_str().unwrap().to_string(),
        tx,
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    std::fs::write(&port_path, "51821").unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    let mut count = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
        count += 1;
    }
    assert_eq!(
        count, 1,
        "debouncer must emit exactly 1 event per write burst, got {count}"
    );
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
    spawn_file_watcher_inner(
        dir.path().to_str().unwrap(),
        ip_path.to_str().unwrap().to_string(),
        port_path.to_str().unwrap().to_string(),
        tx,
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    std::fs::write(&port_path, "51821").unwrap();
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timed out on first write")
        .unwrap();
    // Wait for the debounce window to fully settle before the second write.
    tokio::time::sleep(Duration::from_millis(400)).await;
    std::fs::write(&port_path, "51822").unwrap();
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timed out on second write — watcher stopped after first event")
        .unwrap();
}

// ── Tier 4: Docker container integration ─────────────────────────────────
// Run with: cargo test -- --include-ignored

fn test_client() -> DockerClient {
    DockerClient::connect("/tmp".into(), "/tmp/ip".into(), "/tmp/port".into())
        .expect("Docker socket unavailable")
}

#[tokio::test]
#[ignore = "requires Docker socket; run with: cargo test -- --include-ignored"]
async fn docker_is_container_healthy_returns_false_for_nonexistent() {
    let client = test_client();
    assert!(
        !client
            .is_container_healthy("windlass_test_definitely_absent_xyz")
            .await
    );
}

#[tokio::test]
#[ignore = "requires Docker socket; run with: cargo test -- --include-ignored"]
async fn docker_discover_dependents_finds_containers_in_network_mode() {
    use bollard::container::{
        Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    };
    use bollard::models::HostConfig;

    let mut client = test_client();
    let docker = &client.inner.clone();
    let anchor = "windlass_test_anchor";
    let dependent = "windlass_test_dep";

    for name in [anchor, dependent] {
        let _ = docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }
    docker
        .create_container(
            Some(CreateContainerOptions {
                name: anchor,
                platform: None,
            }),
            Config::<String> {
                image: Some("alpine".into()),
                cmd: Some(vec!["sleep".into(), "30".into()]),
                ..Default::default()
            },
        )
        .await
        .expect("create anchor");
    docker
        .start_container(anchor, None::<StartContainerOptions<String>>)
        .await
        .expect("start anchor");

    docker
        .create_container(
            Some(CreateContainerOptions {
                name: dependent,
                platform: None,
            }),
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
        .expect("create dependent");
    docker
        .start_container(dependent, None::<StartContainerOptions<String>>)
        .await
        .expect("start dependent");

    client.gluetun_anchor = anchor.to_string();
    let found = client.discover_dependents().await;

    for name in [dependent, anchor] {
        let _ = docker
            .stop_container(name, Some(StopContainerOptions { t: 0 }))
            .await;
        let _ = docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    assert!(
        found.contains(&dependent.to_string()),
        "Expected {dependent:?} in {found:?}"
    );
}

#[tokio::test]
#[ignore = "requires Docker socket; run with: cargo test -- --include-ignored"]
async fn docker_fetch_and_dump_logs_creates_log_file() {
    use bollard::container::{
        Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    };
    use std::time::Duration;

    let base_client = test_client();
    let docker = &base_client.inner.clone();
    let container_name = "windlass_test_logs";
    let dump_dir = tempfile::TempDir::new().unwrap();

    let _ = docker
        .remove_container(
            container_name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name,
                platform: None,
            }),
            Config::<String> {
                image: Some("alpine".into()),
                cmd: Some(vec![
                    "sh".into(),
                    "-c".into(),
                    "echo 'windlass_log_marker'; sleep 30".into(),
                ]),
                ..Default::default()
            },
        )
        .await
        .expect("create container");
    docker
        .start_container(container_name, None::<StartContainerOptions<String>>)
        .await
        .expect("start container");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let dump_client = DockerClient::connect(
        dump_dir.path().to_str().unwrap().to_string(),
        "/tmp/ip".into(),
        "/tmp/port".into(),
    )
    .expect("Docker socket unavailable");
    dump_client
        .fetch_and_dump_logs(&[container_name.to_string()])
        .await;

    let _ = docker
        .stop_container(container_name, Some(StopContainerOptions { t: 0 }))
        .await;
    let _ = docker
        .remove_container(
            container_name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    let files: Vec<_> = std::fs::read_dir(dump_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(container_name))
        .collect();
    assert!(
        !files.is_empty(),
        "Expected a dump file for {container_name}"
    );
    let content = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(
        content.contains("windlass_log_marker"),
        "Missing marker in: {content:?}"
    );
}
