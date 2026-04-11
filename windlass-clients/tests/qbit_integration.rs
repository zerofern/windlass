//! Integration tests for `QbitClient` against a real qBittorrent instance.
//!
//! Requires the qBit integration stack:
//! ```text
//! docker compose -f docker-compose.qbit-integration.yml up -d
//! ```
//! Run with: `cargo test --test qbit_integration -- --ignored`

use std::sync::Arc;

use windlass_clients::qbit::QbitClient;
use windlass_types::MamTorrentId;

const QBIT_URL: &str = "http://localhost:18090";
const TORRENT_FIXTURE: &[u8] = include_bytes!("fixtures/test.torrent");

fn make_client() -> QbitClient {
    QbitClient::new(
        reqwest::Client::new(),
        QBIT_URL.to_owned(),
        "admin".to_owned(),
        "adminadmin".to_owned(),
        Arc::new(|_| {}),
    )
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn authenticate_succeeds() {
    let client = make_client();
    let event = client.authenticate().await;
    assert!(
        matches!(event, windlass_core::events::Event::QbitAuthSuccess { .. }),
        "expected auth success, got {event:?}"
    );
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn list_torrent_details_empty_on_fresh_qbit() {
    let client = make_client();
    let event = client.authenticate().await;
    let windlass_core::events::Event::QbitAuthSuccess { cookie, .. } = event else {
        panic!("auth failed");
    };
    let details = client.list_torrent_details(&cookie).await;
    assert!(details.is_empty(), "expected empty list on fresh qBit");
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn add_torrent_then_list_returns_record_with_mam_id() {
    let client = make_client();
    let event = client.authenticate().await;
    let windlass_core::events::Event::QbitAuthSuccess { cookie, .. } = event else {
        panic!("auth failed");
    };

    // add_torrent is a stub in Step 3 — full implementation in Step 4.
    let hash = client.add_torrent(&cookie, TORRENT_FIXTURE.to_vec());
    assert!(
        hash.is_some(),
        "expected add_torrent to return a hash (stub returns None — update in Step 4)"
    );

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let details = client.list_torrent_details(&cookie).await;
    assert!(!details.is_empty(), "expected at least one torrent");

    let found = details
        .iter()
        .find(|d| d.mam_id == Some(MamTorrentId(99999)));
    assert!(found.is_some(), "expected torrent with mam_id=99999");

    let t = found.unwrap();
    assert_eq!(
        t.seeding_time_secs, 0,
        "new torrent should have 0 seeding time"
    );
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn get_preferences_returns_non_zero_limits() {
    let client = make_client();
    let event = client.authenticate().await;
    let windlass_core::events::Event::QbitAuthSuccess { cookie, .. } = event else {
        panic!("auth failed");
    };
    let prefs = client.get_preferences(&cookie).await;
    assert!(prefs.is_some(), "expected Some preferences");
    let p = prefs.unwrap();
    assert!(p.torrents > 0);
}
