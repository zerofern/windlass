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

// ── Gluetun chaos route tests ─────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires dev stack"]
async fn chaos_gluetun_state_endpoint_returns_fields() {
    let client = Client::new();
    let resp: Value = client
        .get(format!("{CHAOS}/gluetun/state"))
        .send()
        .await
        .expect("gluetun state request failed")
        .json()
        .await
        .expect("gluetun state parse failed");

    assert!(resp["ip"].as_str().is_some(), "missing 'ip' field");
    assert!(resp["port"].as_u64().is_some(), "missing 'port' field");
    assert!(
        resp["healthy"].as_bool().is_some(),
        "missing 'healthy' field"
    );
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn chaos_gluetun_set_files_updates_state_and_resyncs_port() {
    let client = Client::new();
    reset(&client).await;

    // Update VPN files via chaos controller
    let resp = client
        .post(format!("{CHAOS}/gluetun/set-files"))
        .json(&serde_json::json!({ "ip": "10.8.0.2", "port": 51821 }))
        .send()
        .await
        .expect("set-files request failed");
    assert_eq!(resp.status(), 200, "set-files should succeed");

    // Chaos state should reflect new values
    let state: Value = client
        .get(format!("{CHAOS}/gluetun/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["ip"].as_str(), Some("10.8.0.2"));
    assert_eq!(state["port"].as_u64(), Some(51821));
    assert_eq!(state["healthy"].as_bool(), Some(true));

    // Windlass should detect the file change and re-sync the new port to qBit
    wait_for("port re-sync to 51821 via chaos controller", 30, || {
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
async fn chaos_gluetun_health_down_up_cycle_recovers() {
    let client = Client::new();
    reset(&client).await;

    // First verify VPN is connected
    wait_for("vpn connected before health test", 15, || {
        let client = client.clone();
        async move { vpn_is_connected(&client).await }
    })
    .await;

    // Take gluetun down via chaos controller
    let resp = client
        .post(format!("{CHAOS}/gluetun/health/down"))
        .send()
        .await
        .expect("health/down request failed");
    assert_eq!(resp.status(), 200);

    // Chaos state should show unhealthy
    let state: Value = client
        .get(format!("{CHAOS}/gluetun/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["healthy"].as_bool(), Some(false));

    // Wait for Docker to detect unhealthy (2s interval × 5 retries = ~10s) and
    // for Windlass to receive DockerGluetunDied
    wait_for("vpn leaves Connected after gluetun down", 30, || {
        let client = client.clone();
        async move { !vpn_is_connected(&client).await }
    })
    .await;

    // Restore gluetun via chaos controller
    let resp = client
        .post(format!("{CHAOS}/gluetun/health/up"))
        .send()
        .await
        .expect("health/up request failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["healthy"].as_bool(), Some(true));

    // Wait for Docker to detect healthy again and for Windlass to recover
    wait_for("vpn reconnects after gluetun up", 30, || {
        let client = client.clone();
        async move { vpn_is_connected(&client).await }
    })
    .await;
}

// ── New fault scenario survival tests ────────────────────────────────────────

#[tokio::test]
#[ignore = "requires dev stack"]
async fn qbit_connection_refused_windlass_stays_alive() {
    let client = Client::new();
    reset(&client).await;

    client
        .post(format!("{CHAOS}/scenario/qbit-connection-refused"))
        .send()
        .await
        .expect("scenario request failed");

    tokio::time::sleep(Duration::from_secs(8)).await;

    let resp = client
        .get(format!("{WINDLASS}/api/v1/health"))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(
        resp.status(),
        200,
        "Windlass should stay alive with qBit refusing connections"
    );
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn mam_not_connectable_windlass_stays_alive() {
    let client = Client::new();
    reset(&client).await;

    client
        .post(format!("{CHAOS}/scenario/mam-not-connectable"))
        .send()
        .await
        .expect("scenario request failed");

    tokio::time::sleep(Duration::from_secs(8)).await;

    let resp = client
        .get(format!("{WINDLASS}/api/v1/health"))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(
        resp.status(),
        200,
        "Windlass should stay alive when MAM reports not connectable"
    );
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn mam_asn_mismatch_windlass_stays_alive() {
    let client = Client::new();
    reset(&client).await;
    // mam-asn-mismatch stub returns ip "1.2.3.4"; VPN files contain "10.8.0.1" — mismatch
    client
        .post(format!("{CHAOS}/scenario/mam-asn-mismatch"))
        .send()
        .await
        .expect("scenario request failed");

    tokio::time::sleep(Duration::from_secs(8)).await;

    let resp = client
        .get(format!("{WINDLASS}/api/v1/health"))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(
        resp.status(),
        200,
        "Windlass should stay alive on MAM ASN mismatch"
    );
}

#[tokio::test]
#[ignore = "requires dev stack"]
async fn gotify_down_windlass_stays_alive() {
    let client = Client::new();
    reset(&client).await;

    client
        .post(format!("{CHAOS}/scenario/gotify-down"))
        .send()
        .await
        .expect("scenario request failed");

    tokio::time::sleep(Duration::from_secs(8)).await;

    let resp = client
        .get(format!("{WINDLASS}/api/v1/health"))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(
        resp.status(),
        200,
        "Windlass should stay alive when Gotify is down"
    );
}

// ── State helpers ─────────────────────────────────────────────────────────────

/// Returns true if the latest Windlass state shows VPN as Connected.
async fn vpn_is_connected(client: &Client) -> bool {
    let Ok(resp) = client.get(format!("{WINDLASS}/api/v1/debug")).send().await else {
        return false;
    };
    let Ok(body): Result<Value, _> = resp.json().await else {
        return false;
    };
    body["latest_state"]["vpn"]["Connected"].is_object()
}

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
