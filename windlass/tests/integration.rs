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
    wait_for("qBit auth at boot", 30, || {
        let client = client.clone();
        async move { count_requests(&client, QBIT_ADMIN, "/api/v2/auth/login").await >= 1 }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn boot_sequence_syncs_port_to_51820() {
    let client = Client::new();
    wait_for("port sync to 51820 at boot", 30, || {
        let client = client.clone();
        async move {
            count_requests_with_body(&client, QBIT_ADMIN, "/api/v2/app/setPreferences", "51820")
                .await
                >= 1
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn boot_sequence_sends_gotify_alert() {
    let client = Client::new();
    wait_for("Gotify alert at boot", 30, || {
        let client = client.clone();
        async move { count_requests(&client, GOTIFY_ADMIN, "/message").await >= 1 }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn boot_sequence_updates_mam_seedbox() {
    let client = Client::new();
    wait_for("MAM seedbox update at boot", 30, || {
        let client = client.clone();
        async move { count_requests(&client, MAM_ADMIN, "/json/dynamicSeedbox.php").await >= 1 }
    })
    .await;
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

    assert!(
        resp["state"].get("vpn").is_some(),
        "state missing 'vpn' field"
    );
    assert!(
        resp["state"].get("qbit").is_some(),
        "state missing 'qbit' field"
    );
    assert!(
        resp["state"].get("mam").is_some(),
        "state missing 'mam' field"
    );
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn mam_rate_limit_scenario_does_not_break_recovery() {
    // Apply the mam-rate-limit scenario (MAM returns 429).
    // Windlass should continue operating normally without crashing.
    let client = Client::new();
    reset(&client).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    client
        .post(format!("{CHAOS}/scenario/mam-rate-limit"))
        .send()
        .await
        .expect("scenario request failed");

    // Wait and confirm Windlass is still alive
    tokio::time::sleep(Duration::from_secs(8)).await;

    let resp = client
        .get(format!("{WINDLASS}/api/v1/health"))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(
        resp.status(),
        200,
        "Windlass should stay alive under MAM 429s"
    );
}
