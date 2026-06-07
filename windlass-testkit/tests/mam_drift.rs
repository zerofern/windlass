//! MAM contract drift smoke test.
//!
//! Mounts the fake-MAM router on a random port and drives each endpoint
//! Windlass calls through `windlass-clients::MamClient`.  If the fake's
//! response shape stops matching what the client decodes, this test fails
//! loudly — pinning the contract `docs/mam-api.md` claims for both sides.
//!
//! Part of operator-readiness §34, PR 1.

use std::net::SocketAddr;
use std::sync::Arc;

use windlass_clients::mam::{MamClient, MamFetchError, MamSeedboxResult};
use windlass_testkit::mam::{MamState, router};
use windlass_types::{MamSessionId, NullHttpTap};

/// Spawn the fake-MAM router on a random local port; return the base URL.
async fn spawn_fake_mam() -> String {
    let state = Arc::new(MamState::default());
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind random port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("axum serve");
    });
    format!("http://{addr}")
}

fn build_client(base: &str) -> MamClient {
    MamClient::new(
        &MamSessionId::new("drift-test-session".to_owned()),
        format!("{base}/json/dynamicSeedbox.php"),
        format!("{base}/jsonLoad.php"),
        "windlass-mam-drift-test",
        NullHttpTap::arc(),
    )
    .expect("build MamClient")
    .with_check_session_url(format!("{base}/json/checkCookie.php"))
    .with_json_ip_url(format!("{base}/json/jsonIp.php"))
}

#[tokio::test]
async fn check_session_succeeds_against_fake_mam() {
    let base = spawn_fake_mam().await;
    let client = build_client(&base);
    client
        .check_session()
        .await
        .expect("checkCookie default response should parse + succeed");
}

#[tokio::test]
async fn fetch_mam_status_parses_default_jsonload() {
    let base = spawn_fake_mam().await;
    let client = build_client(&base);
    let status = client
        .fetch_mam_status()
        .await
        .expect("default /jsonLoad.php response should decode");
    // Default `JsonLoadResponse` claims `connectable: "yes"` only when
    // `?clientStats` is in the query — which `MamClient::new` injects
    // via `ensure_client_stats`.  Confirm the round-trip.
    assert!(status.connectable, "connectable should be true by default");
    assert!(status.ratio > 0.0, "default ratio should be > 0");
}

#[tokio::test]
async fn fetch_mam_ip_parses_default_jsonip() {
    let base = spawn_fake_mam().await;
    let client = build_client(&base);
    let info = client
        .fetch_mam_ip()
        .await
        .expect("default /json/jsonIp.php response should decode");
    assert_eq!(info.ip.0.to_string(), "10.8.0.1");
    assert_eq!(info.asn, 212_238);
    assert_eq!(info.as_org, "Datacamp Limited");
}

#[tokio::test]
async fn update_seedbox_parses_default_dynamic_seedbox() {
    let base = spawn_fake_mam().await;
    let client = build_client(&base);
    let result = client.update_seedbox().await;
    match result {
        MamSeedboxResult::Success {
            registered_ip,
            registered_asn,
            registered_as,
        } => {
            assert_eq!(
                registered_ip.map(|v| v.0.to_string()).as_deref(),
                Some("10.8.0.1")
            );
            assert_eq!(registered_asn, Some(212_238));
            assert_eq!(registered_as.as_deref(), Some("Datacamp Limited"));
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

#[tokio::test]
async fn update_seedbox_decodes_asn_mismatch_shape() {
    let base = spawn_fake_mam().await;
    let client = reqwest::Client::new();
    // Drive the fake's control plane to flip the seedbox response into
    // the §30 ASN-mismatch shape.  Verifies the fake-MAM speaks the
    // exact 403 / "Invalid session - ASN mismatch" combo documented in
    // `docs/mam-api.md`.
    client
        .post(format!("{base}/control/seedbox"))
        .json(&serde_json::json!({
            "status": 403,
            "success": false,
            "msg": "Invalid session - ASN mismatch",
            "ip": "1.2.3.4",
            "asn": 99_999,
            "as_org": "Some Other ISP",
        }))
        .send()
        .await
        .expect("control plane reachable")
        .error_for_status()
        .expect("control plane 200");

    let mam = build_client(&base);
    match mam.update_seedbox().await {
        MamSeedboxResult::AsnMismatch { ip } => {
            assert_eq!(ip.0.to_string(), "1.2.3.4");
        }
        other => panic!("expected AsnMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn fetch_mam_status_decodes_rate_limit_shape() {
    let base = spawn_fake_mam().await;
    let http = reqwest::Client::new();
    // Switch `/jsonLoad.php` to the documented 429 rate-limit shape.
    http.post(format!("{base}/control/json_load"))
        .json(&serde_json::json!({ "status": 429 }))
        .send()
        .await
        .expect("control plane reachable")
        .error_for_status()
        .expect("control plane 200");

    let mam = build_client(&base);
    match mam.fetch_mam_status().await {
        Err(MamFetchError::StatusFailed(reason)) => {
            assert!(reason.contains("429"), "expected 429 in reason: {reason}");
        }
        other => panic!("expected StatusFailed(429), got {other:?}"),
    }
}

#[tokio::test]
async fn journal_records_calls_for_assertions() {
    let base = spawn_fake_mam().await;
    let mam = build_client(&base);
    // Drive a few requests through MamClient.  The client's 400 ms
    // inter-request guard now *waits* instead of dropping the second
    // call (2026-06-06 fix in `wait_for_rate_limit`), so no manual
    // sleeps are needed — three sequential awaits will serialize at
    // ~400 ms intervals automatically.
    let _ = mam.check_session().await;
    let _ = mam.fetch_mam_status().await;
    let _ = mam.fetch_mam_ip().await;

    let http = reqwest::Client::new();
    let entries: serde_json::Value = http
        .get(format!("{base}/control/journal"))
        .send()
        .await
        .expect("journal reachable")
        .json()
        .await
        .expect("journal JSON");
    let arr = entries.as_array().expect("journal is an array");
    let paths: Vec<&str> = arr.iter().filter_map(|v| v["path"].as_str()).collect();
    assert!(paths.contains(&"/json/checkCookie.php"));
    assert!(paths.contains(&"/jsonLoad.php"));
    assert!(paths.contains(&"/json/jsonIp.php"));
}

#[tokio::test]
async fn reset_clears_journal_and_restores_defaults() {
    let base = spawn_fake_mam().await;
    let http = reqwest::Client::new();

    // Tweak the seedbox response away from default.
    http.post(format!("{base}/control/seedbox"))
        .json(&serde_json::json!({ "msg": "Last change too recent", "status": 429 }))
        .send()
        .await
        .expect("control plane reachable")
        .error_for_status()
        .expect("control plane 200");

    // Generate a journal entry.
    let _ = build_client(&base).fetch_mam_ip().await;

    // Reset.
    http.post(format!("{base}/control/reset"))
        .send()
        .await
        .expect("reset reachable")
        .error_for_status()
        .expect("reset 200");

    // Journal should be empty.
    let entries: serde_json::Value = http
        .get(format!("{base}/control/journal"))
        .send()
        .await
        .expect("journal reachable")
        .json()
        .await
        .expect("journal JSON");
    assert_eq!(entries.as_array().map(Vec::len), Some(0));

    // Defaults should be restored: seedbox call now succeeds again.
    let mam = build_client(&base);
    matches!(mam.update_seedbox().await, MamSeedboxResult::Success { .. });
}
