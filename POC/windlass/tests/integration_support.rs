//! Smoke tests for the §34 integration support helpers.
//!
//! Every test here is `#[ignore]`'d so `cargo test` skips it.  They
//! exist to (a) compile-check the helpers and (b) act as the first
//! consumer of `reset_stack()` against the live stack — the same
//! plumbing PR 4's ported tests will use.
//!
//! Run with the dev stack up:
//!
//!     just stack-up
//!     cargo test --test integration_support -- --ignored --test-threads=1 --nocapture

mod support;

use support::{MAM_BASE, WINDLASS_BASE, docker, mam, qbit, reset};

#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn reset_stack_returns_clean_slate() {
    reset::reset_stack().await.expect("reset_stack");

    // Windlass back up and responsive.
    let http = reqwest::Client::new();
    let resp = http
        .get(format!("{WINDLASS_BASE}/api/v1/health"))
        .send()
        .await
        .expect("windlass /api/v1/health reachable");
    assert!(
        resp.status().is_success(),
        "health probe: {}",
        resp.status()
    );

    // qBit has no torrents.
    let count = qbit::torrent_count().await.expect("qbit list");
    assert_eq!(count, 0, "qbit torrent list should be empty after reset");

    // Journal will have boot-time calls (checkCookie + jsonLoad +
    // dynamicSeedbox); reset_stack() deliberately preserves them so
    // contract tests can assert on what Windlass did at boot.  Just
    // confirm the endpoint is responsive.
    let _ = mam::FakeMam::new(MAM_BASE)
        .journal()
        .await
        .expect("mam journal reachable");
}

#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn docker_helpers_inspect_and_restart_windlass() {
    let info = docker::inspect(support::WINDLASS_CONTAINER)
        .await
        .expect("inspect windlass");
    assert!(info.is_ready() || info.state == "running");

    docker::restart_and_wait_healthy(
        support::WINDLASS_CONTAINER,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("restart windlass-test");

    // The image-level healthcheck probes /api/v1/health, so a restarted
    // container must return to Docker-ready before tests continue.
    let after = docker::inspect(support::WINDLASS_CONTAINER)
        .await
        .expect("inspect windlass post-restart");
    assert_eq!(after.state, "running");
}

#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn qbit_fixture_adds_torrent_and_lists_it() {
    reset::reset_stack().await.expect("reset");

    let handle = qbit::add_magnet_torrent("integration-fixture")
        .await
        .expect("add magnet torrent");

    let count = qbit::torrent_count().await.expect("list");
    assert_eq!(count, 1, "expected one torrent in qbit list");

    let hashes = qbit::list_hashes().await.expect("list hashes");
    assert!(
        hashes.contains(&handle.hash),
        "torrent hash {} not in list {hashes:?}",
        handle.hash
    );
}

#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn fake_mam_control_plane_round_trips_through_helper() {
    reset::reset_stack().await.expect("reset");

    let fake = mam::FakeMam::new(MAM_BASE);
    fake.set_seedbox(serde_json::json!({
        "status": 403,
        "success": false,
        "msg": "Invalid session - ASN mismatch",
        "ip": "1.2.3.4",
        "asn": 99_999,
        "as_org": "Some Other ISP",
    }))
    .await
    .expect("set seedbox");

    // Hit the fake directly to confirm the override stuck — a quick
    // sanity check before PR 4 ports tests that drive Windlass into
    // hitting this endpoint.
    let direct: serde_json::Value = reqwest::Client::new()
        .get(format!("{MAM_BASE}/json/dynamicSeedbox.php"))
        .send()
        .await
        .expect("dynamicSeedbox reachable")
        .json()
        .await
        .expect("json body");
    assert_eq!(direct["msg"], "Invalid session - ASN mismatch");
    assert_eq!(direct["ASN"], 99_999);
}
