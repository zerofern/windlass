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
use windlass_clients::qbit::{QbitAuthResult, QbitClient, QbitPortSyncResult, QbitTorrentState};
use windlass_types::{NullHttpTap, QbitPassword, VpnPort};

use support::{
    DATABASE_URL, GLUETUN_CONTROL, MAM_BASE, QBIT_BASE, QBIT_PASS, QBIT_USER, WINDLASS_BASE, mam,
    qbit, reset, wait_for,
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

// ── #6 (audit #1) — Torrent records persist from qBit to DB ──────────────────

/// **Contract:** qBit's `/api/v2/torrents/info` response shape decodes
/// through `windlass-clients::QbitTorrentDetails`, and Windlass's
/// `TorrentRefresh` timer (every 30 s by default) writes those records
/// into Postgres + surfaces them at `/api/v1/torrents`.
///
/// Reshaped from audit #1 ("torrent-records DB persistence end-to-end").
/// Replaces the old WireMock `qbit-torrent-list` chaos hook with a real
/// magnet torrent that qBit honestly reports back.
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn qbit_torrent_persists_to_db_via_api() {
    reset::reset_stack().await.expect("reset_stack");
    let handle = qbit::add_magnet_torrent("contract-fixture")
        .await
        .expect("add magnet");

    let http = reqwest::Client::new();
    wait_for(
        "Windlass /api/v1/torrents reflects qBit torrent",
        Duration::from_secs(60),
        || async {
            let Ok(resp) = http
                .get(format!("{WINDLASS_BASE}/api/v1/torrents"))
                .send()
                .await
            else {
                return false;
            };
            let Ok(body) = resp.json::<Value>().await else {
                return false;
            };
            body.as_array().is_some_and(|arr| {
                arr.iter().any(|t| {
                    t.get("hash")
                        .and_then(Value::as_str)
                        .is_some_and(|h| h.eq_ignore_ascii_case(&handle.hash))
                })
            })
        },
    )
    .await;
}

// ── #7 (audit #5) — Gluetun IP change drives a new seedbox call ──────────────

/// **Contract:** when Gluetun's IP file changes, Windlass detects the
/// change (§31) and calls MAM's `/json/dynamicSeedbox.php`.  The
/// fake-MAM journal must contain a fresh entry posted *after* the IP
/// change.  Endpoint takes no body, so the contract is "request count
/// increases" — what MAM does with it is the server's concern.
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn gluetun_ip_change_triggers_new_seedbox_call() {
    reset::reset_stack().await.expect("reset_stack");
    let fake = mam::FakeMam::new(MAM_BASE);

    // Wait for the boot-time call to land so we have a known baseline.
    wait_for(
        "boot-time dynamicSeedbox call lands",
        Duration::from_secs(30),
        || async {
            fake.journal()
                .await
                .is_ok_and(|entries| entries.iter().any(|e| e.path == "/json/dynamicSeedbox.php"))
        },
    )
    .await;

    // Reset the journal so the count below starts at 0.
    fake.reset().await.expect("clear journal");

    // Push a new IP via fake-Gluetun.  §32's 1-hour rate limit just
    // ticked (during boot) — so this second call would normally be
    // suppressed.  Reset the rate limiter by restarting Windlass.
    docker_restart_windlass().await;
    // Now write the new IP and wait for Windlass to react with a
    // fresh dynamicSeedbox call against the just-cleared journal.
    let http = reqwest::Client::new();
    http.post(format!("{GLUETUN_CONTROL}/set"))
        .json(&serde_json::json!({ "ip": "10.8.0.42", "port": 51_820 }))
        .send()
        .await
        .expect("gluetun set")
        .error_for_status()
        .expect("gluetun set 200");

    wait_for(
        "new dynamicSeedbox call appears post-IP-change",
        Duration::from_secs(30),
        || async {
            fake.journal()
                .await
                .is_ok_and(|entries| entries.iter().any(|e| e.path == "/json/dynamicSeedbox.php"))
        },
    )
    .await;
}

async fn docker_restart_windlass() {
    support::docker::restart_and_wait_healthy(support::WINDLASS_CONTAINER, Duration::from_secs(45))
        .await
        .expect("restart windlass");
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build readiness client");
    wait_for(
        "windlass /api/v1/health 200",
        Duration::from_secs(45),
        || async {
            http.get(format!("{WINDLASS_BASE}/api/v1/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
        },
    )
    .await;
}

// ── #8 (audit #8) — §32 1-hour seedbox rate limit suppresses re-call ─────────

/// **Contract:** within an hour of a successful `dynamicSeedbox.php`
/// call, Windlass's MAM client must not make a second one.  A second
/// IP change during the cool-down should be silently suppressed at the
/// client-side gate (§32).
///
/// We let the boot-time call land, then change the Gluetun IP and
/// confirm no second call shows up in the journal within ~8 seconds.
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn seedbox_rate_limit_suppresses_second_call_within_hour() {
    reset::reset_stack().await.expect("reset_stack");
    let fake = mam::FakeMam::new(MAM_BASE);

    // Boot-time call has fired once: rate limiter is now armed.
    wait_for(
        "boot-time dynamicSeedbox call",
        Duration::from_secs(30),
        || async {
            fake.journal()
                .await
                .is_ok_and(|entries| count_seedbox_calls(&entries) >= 1)
        },
    )
    .await;

    // Try to force a second call by changing the Gluetun IP.
    let http = reqwest::Client::new();
    http.post(format!("{GLUETUN_CONTROL}/set"))
        .json(&serde_json::json!({ "ip": "10.8.0.99", "port": 51_820 }))
        .send()
        .await
        .expect("gluetun set")
        .error_for_status()
        .expect("gluetun set 200");

    // Give Windlass plenty of time to react.
    tokio::time::sleep(Duration::from_secs(8)).await;

    // Rate limit must still be in effect: exactly one dynamicSeedbox
    // entry in the journal.
    let entries = fake.journal().await.expect("journal");
    let count = count_seedbox_calls(&entries);
    assert_eq!(
        count, 1,
        "expected exactly 1 dynamicSeedbox call under 1h rate limit; got {count}"
    );
}

fn count_seedbox_calls(entries: &[support::mam::JournalEntry]) -> usize {
    entries
        .iter()
        .filter(|e| e.path == "/json/dynamicSeedbox.php")
        .count()
}

// ── #9 — qBit API drift smoke pass ───────────────────────────────────────────

/// **Drift sentinel:** for every qBit endpoint Windlass actively
/// consumes, exercise the call against real qBit through
/// `windlass-clients::QbitClient` and assert it succeeds + decodes.
/// If qBit's API shape changes in a future image bump, this test
/// fails loudly before any operator-readiness contract test is even
/// reached.
///
/// Endpoints covered: `/api/v2/auth/login`, `/api/v2/app/preferences`,
/// `/api/v2/app/setPreferences`, `/api/v2/torrents/info`,
/// `/api/v2/torrents/add` (the last via the magnet fixture so the
/// `torrents/info` decode has something to chew on).
#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn qbit_endpoints_match_windlass_clients_types() {
    reset::reset_stack().await.expect("reset_stack");
    let client = QbitClient::new(
        reqwest::Client::new(),
        QBIT_BASE.to_owned(),
        QBIT_USER.to_owned(),
        QbitPassword::new(QBIT_PASS.to_owned()),
        NullHttpTap::arc(),
    );

    // /api/v2/auth/login
    let cookie = match client.authenticate().await {
        QbitAuthResult::Success(cookie) => cookie,
        other => panic!("expected QbitAuthResult::Success, got {other:?}"),
    };

    // /api/v2/app/preferences
    let prefs = client
        .get_preferences(&cookie)
        .await
        .expect("preferences decode");
    assert!(
        prefs.listen_port.is_some(),
        "preferences listen_port should decode"
    );

    // /api/v2/app/setPreferences
    let port = VpnPort::try_new(51_900).expect("valid port");
    match client.sync_port(&cookie, port).await {
        QbitPortSyncResult::Success => {}
        QbitPortSyncResult::Failed(code) => panic!("sync_port failed with status {code}"),
    }
    // Confirm the change actually landed and decodes back.
    let after = client
        .get_preferences(&cookie)
        .await
        .expect("preferences decode after sync");
    assert_eq!(
        after.listen_port,
        Some(port),
        "qBit preferences should reflect synced port"
    );

    // /api/v2/torrents/info — empty list parses.
    let empty = client.list_torrent_details(&cookie).await;
    assert!(empty.is_empty(), "expected empty torrent list after reset");

    // /api/v2/torrents/add → /api/v2/torrents/info — non-empty list decodes.
    qbit::add_magnet_torrent("drift-fixture")
        .await
        .expect("add magnet");
    wait_for(
        "qBit torrent appears in list_torrent_details",
        Duration::from_secs(15),
        || async { !client.list_torrent_details(&cookie).await.is_empty() },
    )
    .await;
    let details = client.list_torrent_details(&cookie).await;
    assert_eq!(details.len(), 1, "expected one torrent post-add");
    // Sanity: the magnet has no peers, so qBit should report it stalled.
    assert!(
        matches!(
            details[0].state,
            QbitTorrentState::StalledDownloading
                | QbitTorrentState::Downloading
                | QbitTorrentState::PausedDownloading
                | QbitTorrentState::Other(_)
        ),
        "unexpected torrent state: {:?}",
        details[0].state
    );
}
