// ── Tier 4: Docker container integration ─────────────────────────────────
// Run with: cargo test -- --include-ignored

use super::*;

fn test_client() -> DockerClient {
    DockerClient::connect("/tmp".into()).expect("Docker socket unavailable")
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

    let dump_client = DockerClient::connect(dump_dir.path().to_str().unwrap().to_string())
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
