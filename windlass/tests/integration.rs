//! Integration tests. Require the dev stack to be running.
//! Run with: just integration
//!
//! These tests are ignored by default so `cargo test` doesn't require Docker.
//! The `just integration` recipe starts the stack, runs them, and tears down.

use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

const WINDLASS: &str = "http://localhost:5010";
const CHAOS: &str = "http://localhost:9000";
const GLUETUN_CTL: &str = "http://localhost:9001";
const QBIT_ADMIN: &str = "http://localhost:18080/__admin";
const GOTIFY_ADMIN: &str = "http://localhost:18081/__admin";
const MAM_ADMIN: &str = "http://localhost:18082/__admin";

async fn reset(client: &Client) {
    client
        .post(format!("{CHAOS}/reset"))
        .send()
        .await
        .expect("chaos reset failed");
    tokio::time::sleep(Duration::from_millis(500)).await;
}

async fn count_requests(client: &Client, admin: &str, url_fragment: &str) -> usize {
    let resp: Value = client
        .get(format!("{admin}/requests"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    resp["requests"].as_array().map_or(0, |arr| {
        arr.iter()
            .filter(|r| {
                r["request"]["url"]
                    .as_str()
                    .is_some_and(|u| u.contains(url_fragment))
            })
            .count()
    })
}

async fn count_requests_with_body(
    client: &Client,
    admin: &str,
    url_fragment: &str,
    body_fragment: &str,
) -> usize {
    let resp: Value = client
        .get(format!("{admin}/requests"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    resp["requests"].as_array().map_or(0, |arr| {
        arr.iter()
            .filter(|r| {
                let url_ok = r["request"]["url"]
                    .as_str()
                    .is_some_and(|u| u.contains(url_fragment));
                let body_ok = r["request"]["body"]
                    .as_str()
                    .is_some_and(|b| b.contains(body_fragment));
                url_ok && body_ok
            })
            .count()
    })
}

/// Wait for a condition with timeout.
async fn wait_for<F, Fut>(label: &str, timeout_secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if f().await {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Timed out waiting for: {label}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires dev stack"]
async fn windlass_health_endpoint_returns_ok() {
    let client = Client::new();
    let resp = client
        .get(format!("{WINDLASS}/api/v1/health"))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn boot_sequence_authenticates_qbit() {
    let client = Client::new();
    reset(&client).await;

    // Give Windlass time to complete boot (it's already running)
    tokio::time::sleep(Duration::from_secs(5)).await;

    let n = count_requests(&client, QBIT_ADMIN, "/api/v2/auth/login").await;
    assert!(n >= 1, "qBit auth was not called (got {n})");
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn boot_sequence_syncs_port_to_51820() {
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let n =
        count_requests_with_body(&client, QBIT_ADMIN, "/api/v2/app/setPreferences", "51820").await;
    assert!(n >= 1, "Port sync to 51820 not called (got {n})");
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn boot_sequence_sends_gotify_alert() {
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let n = count_requests(&client, GOTIFY_ADMIN, "/message").await;
    assert!(n >= 1, "Gotify received no alerts (got {n})");
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn boot_sequence_updates_mam_seedbox() {
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let n = count_requests(&client, MAM_ADMIN, "/json/dynamicSeedbox.php").await;
    assert!(n >= 1, "MAM seedbox not called (got {n})");
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn vpn_reconnect_resyncs_port() {
    let client = Client::new();
    reset(&client).await;

    // Simulate VPN reconnect with new port via gluetun control API
    client
        .post("http://localhost:9001/set")
        .json(&serde_json::json!({ "ip": "10.8.0.2", "port": 51821 }))
        .send()
        .await
        .expect("gluetun set failed");

    // Wait for Windlass to detect file change and re-sync
    wait_for("port re-sync to 51821", 30, || {
        let client = client.clone();
        async move {
            count_requests_with_body(&client, QBIT_ADMIN, "/api/v2/app/setPreferences", "51821")
                .await
                >= 1
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn windlass_state_endpoint_returns_system_state() {
    let client = Client::new();
    let resp: Value = client
        .get(format!("{WINDLASS}/api/v1/operator/state"))
        .send()
        .await
        .expect("state request failed")
        .json()
        .await
        .expect("state parse failed");

    assert!(resp.get("vpn").is_some(), "state missing 'vpn' field");
    assert!(resp.get("qbit").is_some(), "state missing 'qbit' field");
    assert!(resp.get("mam").is_some(), "state missing 'mam' field");
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn qbit_auth_fail_scenario_causes_retry() {
    let client = Client::new();
    reset(&client).await;

    // Apply qbit-auth-fail scenario
    client
        .post(format!("{CHAOS}/scenario/qbit-auth-fail"))
        .send()
        .await
        .expect("scenario request failed");

    // Wait for Windlass to attempt auth and fail/retry
    tokio::time::sleep(Duration::from_secs(10)).await;

    let n = count_requests(&client, QBIT_ADMIN, "/api/v2/auth/login").await;
    assert!(
        n >= 2,
        "Expected ≥2 auth attempts after auth-fail (got {n})"
    );
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn state_endpoint_includes_frozen_field() {
    let client = Client::new();
    let resp: Value = client
        .get(format!("{WINDLASS}/api/v1/operator/state"))
        .send()
        .await
        .expect("state request failed")
        .json()
        .await
        .expect("state parse failed");

    assert!(resp.get("frozen").is_some(), "state missing 'frozen' field");
    assert_eq!(resp["frozen"], false, "'frozen' should be false at rest");
    assert!(
        resp.get("state").is_some(),
        "state missing nested 'state' field"
    );
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn manual_reset_re_authenticates_qbit() {
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    let before = count_requests(&client, QBIT_ADMIN, "/api/v2/auth/login").await;

    // Trigger manual reset
    let status = client
        .post(format!("{WINDLASS}/api/v1/operator/reset"))
        .send()
        .await
        .expect("reset request failed")
        .status();
    assert_eq!(status.as_u16(), 202, "reset should return 202 Accepted");

    // Wait for re-authentication
    wait_for("qBit re-authentication after manual reset", 15, || {
        let client = client.clone();
        async move { count_requests(&client, QBIT_ADMIN, "/api/v2/auth/login").await > before }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn vpn_reconnect_updates_mam_with_new_ip() {
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    let before_mam = count_requests(&client, MAM_ADMIN, "/json/dynamicSeedbox.php").await;

    // Simulate VPN reconnect with a different IP
    client
        .post(format!("{GLUETUN_CTL}/set"))
        .json(&serde_json::json!({ "ip": "10.8.0.9", "port": 51829 }))
        .send()
        .await
        .expect("gluetun set failed");

    // Wait for MAM to be called after the IP change
    wait_for("MAM update after IP change to 10.8.0.9", 30, || {
        let client = client.clone();
        let before_count = before_mam;
        async move {
            count_requests(&client, MAM_ADMIN, "/json/dynamicSeedbox.php").await > before_count
        }
    })
    .await;

    // Restore original IP for subsequent tests
    client
        .post(format!("{GLUETUN_CTL}/set"))
        .json(&serde_json::json!({ "ip": "10.8.0.1", "port": 51820 }))
        .send()
        .await
        .expect("gluetun restore failed");
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn gluetun_death_triggers_recovery_to_ready() {
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Stop the gluetun container — Windlass should detect DockerGluetunDied
    // via the Docker event stream, dump logs, restart, and recover to Ready.
    std::process::Command::new("docker")
        .args(["stop", "gluetun"])
        .output()
        .expect("docker stop gluetun failed");

    // Wait for Windlass to reach Ready state again (full recovery cycle)
    wait_for("recovery to Ready after gluetun death", 60, || {
        let client = client.clone();
        async move {
            let Ok(resp) = client
                .get(format!("{WINDLASS}/api/v1/operator/state"))
                .send()
                .await
            else {
                return false;
            };
            let Ok(body): Result<Value, _> = resp.json().await else {
                return false;
            };
            body["state"]["qbit"].get("Ready").is_some()
                && body["state"]["mam"].get("Synced").is_some()
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn mam_rate_limit_scenario_does_not_break_recovery() {
    // The mam-rate-limit scenario makes MAM return 429. The MAM client treats
    // a non-parseable response as MamUpdateSuccess (silent degradation), so
    // Windlass should stay in Active mode and not crash.
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    client
        .post(format!("{CHAOS}/scenario/mam-rate-limit"))
        .send()
        .await
        .expect("scenario request failed");

    // Trigger a manual reset so MAM update runs
    client
        .post(format!("{WINDLASS}/api/v1/operator/reset"))
        .send()
        .await
        .expect("reset failed");

    // Wait and confirm Windlass remains in Active run_mode (not Fatal)
    tokio::time::sleep(Duration::from_secs(8)).await;

    let resp: Value = client
        .get(format!("{WINDLASS}/api/v1/operator/state"))
        .send()
        .await
        .expect("state request failed")
        .json()
        .await
        .expect("parse failed");

    assert_eq!(
        resp["state"]["run_mode"], "Active",
        "Windlass should stay Active under MAM 429s"
    );
}
