//! Bollard-backed container control for tests.
//!
//! Used by `reset_stack()` (Windlass restart between tests) and by tests
//! that need to stop/start individual containers to model real outages
//! (e.g. "what happens when qBit goes away mid-flight").

use std::time::Duration;

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::container::{RestartContainerOptions, StartContainerOptions, StopContainerOptions};
use bollard::models::HealthStatusEnum;

fn client() -> Result<Docker> {
    Docker::connect_with_socket_defaults().context("connect to docker socket")
}

/// Restart a container by name (or container ID).  Returns once docker
/// confirms the restart, not once the contained service is healthy —
/// callers should poll the service's own readiness probe after.
pub async fn restart(name: &str) -> Result<()> {
    let opts = RestartContainerOptions { t: 0 };
    client()?
        .restart_container(name, Some(opts))
        .await
        .with_context(|| format!("restart container {name}"))
}

/// Stop a container with a graceful-shutdown deadline.  After `timeout`
/// the daemon sends SIGKILL.
pub async fn stop(name: &str, timeout: Duration) -> Result<()> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let opts = StopContainerOptions {
        t: timeout.as_secs() as i64,
    };
    client()?
        .stop_container(name, Some(opts))
        .await
        .with_context(|| format!("stop container {name}"))
}

/// Start a previously-stopped container.
pub async fn start(name: &str) -> Result<()> {
    client()?
        .start_container(name, None::<StartContainerOptions<String>>)
        .await
        .with_context(|| format!("start container {name}"))?;
    Ok(())
}

/// Restart-and-wait: restart by name and block until the container's
/// health probe reports `healthy` (or `running` if no healthcheck is
/// configured).  Useful when the test needs the service ready before
/// the next assertion.
pub async fn restart_and_wait_healthy(name: &str, timeout: Duration) -> Result<()> {
    restart(name).await?;
    super::wait_for(
        &format!("{name} healthy after restart"),
        timeout,
        || async { inspect(name).await.ok().is_some_and(|info| info.is_ready()) },
    )
    .await;
    Ok(())
}

/// Brief container state for tests.  Sourced from `docker inspect`.
pub struct ContainerInfo {
    /// `"running"`, `"exited"`, `"paused"`, `"created"`, `"restarting"`,
    /// `"removing"`, `"dead"`.
    pub state: String,
    /// `"healthy"`, `"unhealthy"`, `"starting"`, or `None` if no
    /// healthcheck is configured.
    pub health: Option<String>,
    /// ISO-8601 timestamp; useful for §35-style stale-namespace
    /// assertions ("dependent started before the anchor's `healthy_since`").
    pub started_at: String,
}

impl ContainerInfo {
    /// True when the container is running and (if a healthcheck exists)
    /// reports healthy.  Used by `restart_and_wait_healthy`.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state == "running" && self.health.as_deref().is_none_or(|h| h == "healthy")
    }
}

/// Read container state.  Errors if the container doesn't exist.
pub async fn inspect(name: &str) -> Result<ContainerInfo> {
    let resp = client()?
        .inspect_container(name, None)
        .await
        .with_context(|| format!("inspect container {name}"))?;
    let state = resp.state.unwrap_or_default();
    let health = state.health.and_then(|h| h.status).map(|s| match s {
        HealthStatusEnum::HEALTHY => "healthy".to_string(),
        HealthStatusEnum::UNHEALTHY => "unhealthy".to_string(),
        HealthStatusEnum::STARTING => "starting".to_string(),
        HealthStatusEnum::NONE => "none".to_string(),
        HealthStatusEnum::EMPTY => "empty".to_string(),
    });
    Ok(ContainerInfo {
        state: state
            .status
            .map(|s| format!("{s:?}").to_lowercase())
            .unwrap_or_default(),
        health,
        started_at: state.started_at.unwrap_or_default(),
    })
}
