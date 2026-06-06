//! §34 contract tests: real Docker stack, real qBittorrent, fake MAM,
//! fake Gluetun.  Each test starts from a known baseline via
//! `reset_stack()` and asserts on a wire Windlass depends on.
//!
//! Run with the dev stack up:
//!
//!     just stack-up
//!     cargo test --test integration_contracts -- --ignored --test-threads=1 --nocapture

mod support;

use std::sync::Arc;
use std::time::Duration;

use reqwest::cookie::Jar;
use serde_json::Value;

use support::{
    DATABASE_URL, GLUETUN_CONTROL, MAM_BASE, QBIT_BASE, QBIT_PASS, QBIT_USER, WINDLASS_BASE, mam,
    reset, wait_for,
};

/// Build a fresh, authed reqwest client against the real qBit.  Used
/// to read qBit's `/api/v2/app/preferences` for the port-sync
/// assertions.  Mirrors the helper in `support::qbit::authed_client`
/// (kept here to avoid making that one public).
async fn qbit_authed() -> reqwest::Client {
    let jar = Arc::new(Jar::default());
    let client = reqwest::Client::builder()
        .cookie_provider(Arc::clone(&jar))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build qbit client");
    let resp = client
        .post(format!("{QBIT_BASE}/api/v2/auth/login"))
        .header("Referer", QBIT_BASE)
        .form(&[("username", QBIT_USER), ("password", QBIT_PASS)])
        .send()
        .await
        .expect("qbit login");
    assert!(resp.status().is_success(), "qbit login status");
    client
}

async fn qbit_listen_port() -> Option<u64> {
    let client = qbit_authed().await;
    let prefs: Value = client
        .get(format!("{QBIT_BASE}/api/v2/app/preferences"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    prefs.get("listen_port").and_then(Value::as_u64)
}

// ── #1 — Gluetun re-syncs port to qBit ───────────────────────────────────────

/// **Contract:** when fake Gluetun writes a new IP/port pair to its
/// shared volume, Windlass observes the file change via `inotify` and
/// POSTs `setPreferences` to qBit with the new `listen_port` value.
///
/// Verifies the Gluetun-file → Windlass → qBit wire end-to-end.
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn gluetun_set_files_resyncs_port_to_qbit() {
    reset::reset_stack().await.expect("reset_stack");
    let http = reqwest::Client::new();

    // Push a new ip+port via the fake-Gluetun control plane.
    http.post(format!("{GLUETUN_CONTROL}/set"))
        .json(&serde_json::json!({ "ip": "10.8.0.2", "port": 51_821 }))
        .send()
        .await
        .expect("gluetun set")
        .error_for_status()
        .expect("gluetun set 200");

    // Wait for Windlass to push the new port into qBit's preferences.
    wait_for(
        "qbit listen_port becomes 51821",
        Duration::from_secs(30),
        || async { qbit_listen_port().await == Some(51_821) },
    )
    .await;
}

// ── #2 — Boot authenticates against qBit ─────────────────────────────────────

/// **Contract:** at boot, Windlass's qBit auth flow succeeds against
/// the real qBittorrent web API.  We don't have a Windlass-side
/// endpoint that exposes "auth ok" directly, so we assert it via the
/// torrents API (which can only return 200 once qBit has been
/// authed and torrent reconciliation has run at least once).
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn boot_authenticates_qbit() {
    reset::reset_stack().await.expect("reset_stack");
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{WINDLASS_BASE}/api/v1/torrents"))
        .send()
        .await
        .expect("/api/v1/torrents reachable");
    assert!(
        resp.status().is_success(),
        "/api/v1/torrents status: {}",
        resp.status()
    );
    let body: Value = resp.json().await.expect("/api/v1/torrents json");
    assert!(body.is_array(), "expected JSON array, got: {body}");
}

// ── #3 — Boot syncs port 51820 to qBit preferences ───────────────────────────

/// **Contract:** after a clean restart Windlass reads Gluetun's port
/// file (51820 by default per `reset_stack()`) and pushes that into
/// qBit's preferences within the boot window.
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn boot_syncs_default_port_to_qbit_preferences() {
    reset::reset_stack().await.expect("reset_stack");

    wait_for(
        "qbit listen_port becomes 51820 after boot",
        Duration::from_secs(30),
        || async { qbit_listen_port().await == Some(51_820) },
    )
    .await;
}

// ── #4 — Boot updates MAM seedbox ────────────────────────────────────────────

/// **Contract:** at boot, Windlass POSTs / GETs the MAM
/// `/json/dynamicSeedbox.php` endpoint with the IP it observed from
/// the Gluetun file.  Asserted via the fake-MAM request journal.
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn boot_updates_mam_seedbox() {
    reset::reset_stack().await.expect("reset_stack");
    let fake = mam::FakeMam::new(MAM_BASE);

    wait_for(
        "fake-mam sees /json/dynamicSeedbox.php at boot",
        Duration::from_secs(30),
        || async {
            fake.journal()
                .await
                .is_ok_and(|entries| entries.iter().any(|e| e.path == "/json/dynamicSeedbox.php"))
        },
    )
    .await;
}

// ── #5 — Boot writes a system snapshot to Postgres ───────────────────────────

/// **Contract:** the DB-core path writes a row to `system_snapshots`
/// during boot.  This is the wire we depend on for the dashboard's
/// "latest known state" snapshot.
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn boot_writes_system_snapshot_to_db() {
    reset::reset_stack().await.expect("reset_stack");

    wait_for(
        "system_snapshots row appears",
        Duration::from_secs(30),
        || async { system_snapshot_count().await > 0 },
    )
    .await;
}

async fn system_snapshot_count() -> i64 {
    let pool = sqlx::PgPool::connect(DATABASE_URL)
        .await
        .expect("connect to postgres");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM system_snapshots")
        .fetch_one(&pool)
        .await
        .expect("count system_snapshots");
    pool.close().await;
    count
}
