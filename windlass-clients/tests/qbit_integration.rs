//! Integration tests for `QbitClient` against a real qBittorrent instance.
//!
//! Requires the qBit integration stack:
//! ```text
//! docker compose -f docker-compose.qbit-integration.yml up -d
//! ```
//! Run with: `cargo test --test qbit_integration -- --ignored`

use std::sync::Arc;

use windlass_clients::qbit::QbitClient;
use windlass_types::{AuthCookie, MamTorrentId};

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

async fn authenticated_clean_client() -> (QbitClient, AuthCookie) {
    let client = make_client();
    let event = client.authenticate().await;
    let windlass_core::events::Event::QbitAuthSuccess { cookie, .. } = event else {
        panic!("auth failed");
    };
    clear_torrents(&client, &cookie).await;
    (client, cookie)
}

async fn clear_torrents(client: &QbitClient, cookie: &AuthCookie) {
    let details = client.list_torrent_details(cookie).await;
    for torrent in details {
        client.delete_torrent(cookie, &torrent.hash).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
    let (client, cookie) = authenticated_clean_client().await;
    let details = client.list_torrent_details(&cookie).await;
    assert!(details.is_empty(), "expected empty list on fresh qBit");
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn add_torrent_then_list_returns_record_with_mam_id() {
    let (client, cookie) = authenticated_clean_client().await;

    // add_torrent is now async and real — await the result.
    let hash = client.add_torrent(&cookie, TORRENT_FIXTURE.to_vec()).await;
    assert!(hash.is_some(), "expected add_torrent to return a hash");

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
    let (client, cookie) = authenticated_clean_client().await;
    let prefs = client.get_preferences(&cookie).await;
    assert!(prefs.is_some(), "expected Some preferences");
    let p = prefs.unwrap();
    assert!(p.torrents > 0);
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn pause_then_resume_torrent() {
    let (client, cookie) = authenticated_clean_client().await;

    let hash = client.add_torrent(&cookie, TORRENT_FIXTURE.to_vec()).await;
    let Some(hash) = hash else {
        panic!("add_torrent returned None");
    };
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    client.pause_torrent(&cookie, &hash).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let details = client.list_torrent_details(&cookie).await;
    let found = details.iter().find(|d| d.hash == hash);
    assert!(found.is_some(), "torrent not found after pause");
    // qBit 5.2 can report no-peer torrents as stalled immediately after stop,
    // even though the stop endpoint accepted the request.
    assert!(
        matches!(
            found.unwrap().state,
            windlass_clients::qbit::QbitTorrentState::PausedDownloading
                | windlass_clients::qbit::QbitTorrentState::PausedUploading
                | windlass_clients::qbit::QbitTorrentState::StalledDownloading
                | windlass_clients::qbit::QbitTorrentState::StalledUploading
        ),
        "expected paused or stalled state, got {:?}",
        found.unwrap().state
    );

    client.resume_torrent(&cookie, &hash).await;
    // Don't assert state after resume — no peers means it will stall immediately
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn set_all_files_priority_succeeds() {
    let (client, cookie) = authenticated_clean_client().await;

    let hash = client.add_torrent(&cookie, TORRENT_FIXTURE.to_vec()).await;
    let Some(hash) = hash else {
        panic!("add_torrent returned None");
    };
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Should not panic
    client.set_all_files_priority(&cookie, &hash).await;
}

#[tokio::test]
#[ignore = "requires qbit integration stack"]
async fn delete_torrent_removes_it_from_list() {
    let (client, cookie) = authenticated_clean_client().await;

    let hash = client.add_torrent(&cookie, TORRENT_FIXTURE.to_vec()).await;
    let Some(hash) = hash else {
        panic!("add_torrent returned None");
    };
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    client.delete_torrent(&cookie, &hash).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let details = client.list_torrent_details(&cookie).await;
    let still_there = details.iter().any(|d| d.hash == hash);
    assert!(!still_there, "torrent should be gone after delete");
}
