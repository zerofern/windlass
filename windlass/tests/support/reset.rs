//! `reset_stack()` — bring the §34 dev stack to a clean state between
//! tests.
//!
//! Order matters here: we clear external state first (so the post-
//! restart Windlass boots into the world we want), then truncate the
//! DB, then restart Windlass and wait for it to come back.

use std::time::Duration;

use anyhow::{Context, Result};

use super::{
    DATABASE_URL, GLUETUN_CONTROL, MAM_BASE, WINDLASS_BASE, WINDLASS_CONTAINER, docker, mam, qbit,
};

/// Restart-window deadline for the Windlass container.
const RESTART_TIMEOUT: Duration = Duration::from_secs(45);

/// Restore the stack to a known baseline:
///
/// 1. Clear fake-MAM journal + restore endpoint defaults.
/// 2. Reset fake-Gluetun IP/port files + healthy state.
/// 3. Delete every torrent from qBit.
/// 4. Truncate Windlass DB tables (`alerts`, `activity_log`,
///    `download_queue`, `torrents`, `system_snapshots`).
/// 5. Restart the Windlass container; wait for `/api/v1/health` to
///    answer 200.
/// 6. Clear the fake-MAM journal again so the new test sees no
///    boot-time noise.
pub async fn reset_stack() -> Result<()> {
    // 1. fake-MAM.
    mam::FakeMam::new(MAM_BASE)
        .reset()
        .await
        .context("fake-mam reset")?;

    // 2. fake-Gluetun.  The testkit gluetun mode exposes /set + /clear-port.
    let http = reqwest::Client::new();
    http.post(format!("{GLUETUN_CONTROL}/set"))
        .json(&serde_json::json!({ "ip": "10.8.0.1", "port": 51_820 }))
        .send()
        .await
        .context("gluetun set")?
        .error_for_status()
        .context("gluetun set status")?;

    // 3. qBit.
    qbit::delete_all().await.context("qbit delete_all")?;

    // 4. Windlass DB.  We truncate the rolling-state tables; SQLx
    //    handles the migration schema and we don't want to drop it.
    truncate_db().await.context("truncate db")?;

    // 5. Restart Windlass.  The container has no healthcheck, so
    //    `restart_and_wait_healthy` falls back to "state == running"
    //    — we follow up with an HTTP readiness poll on /api/v1/health
    //    to ensure the API server is up before returning.
    docker::restart_and_wait_healthy(WINDLASS_CONTAINER, RESTART_TIMEOUT)
        .await
        .context("restart windlass")?;
    wait_for_windlass_ready(RESTART_TIMEOUT).await;

    // 6. Re-clear the fake-MAM journal: Windlass made check-session +
    //    keep-alive calls during boot that the next test shouldn't see.
    mam::FakeMam::new(MAM_BASE)
        .reset()
        .await
        .context("fake-mam reset (post-restart)")?;

    Ok(())
}

async fn truncate_db() -> Result<()> {
    let pool = sqlx::PgPool::connect(DATABASE_URL)
        .await
        .context("connect to test postgres")?;
    // Single TRUNCATE with CASCADE so order doesn't matter; RESTART
    // IDENTITY so id columns start from 1 every test.
    sqlx::query(
        "TRUNCATE TABLE \
            alerts, activity_log, download_queue, torrents, system_snapshots, books \
            RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .context("truncate")?;
    pool.close().await;
    Ok(())
}

async fn wait_for_windlass_ready(timeout: Duration) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build readiness client");
    super::wait_for("windlass /api/v1/health 200", timeout, || async {
        http.get(format!("{WINDLASS_BASE}/api/v1/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    })
    .await;
}
