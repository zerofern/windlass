use std::sync::Arc;

use super::{QbitAuthResult, QbitClient, QbitPortSyncResult, QbitTorrentState};
use windlass_types::{AuthCookie, MamTorrentId, TorrentHash, VpnPort};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── authenticate ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn authenticate_success_extracts_sid_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "SID=abc123; Path=/; HttpOnly")
                .set_body_string("Ok."),
        )
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let event = qbit.authenticate().await;
    match &event {
        QbitAuthResult::Success(cookie) => assert_eq!(cookie.expose_secret(), "abc123"),
        _ => panic!("Expected QbitAuthResult::Success(abc123), got {event:?}"),
    }
}

#[tokio::test]
async fn authenticate_success_accepts_204_with_sid_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(
            ResponseTemplate::new(204).append_header("Set-Cookie", "SID=abc123; Path=/; HttpOnly"),
        )
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let event = qbit.authenticate().await;
    match &event {
        QbitAuthResult::Success(cookie) => assert_eq!(cookie.expose_secret(), "abc123"),
        _ => panic!("Expected QbitAuthResult::Success(abc123), got {event:?}"),
    }
}

#[tokio::test]
async fn authenticate_success_extracts_qbt_sid_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(
            ResponseTemplate::new(204)
                .append_header("Set-Cookie", "QBT_SID_8080=abc123; Path=/; HttpOnly"),
        )
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let event = qbit.authenticate().await;
    match &event {
        QbitAuthResult::Success(cookie) => assert_eq!(cookie.expose_secret(), "abc123"),
        _ => panic!("Expected QbitAuthResult::Success(abc123), got {event:?}"),
    }
}

#[tokio::test]
async fn authenticate_ok_body_without_sid_cookie_returns_auth_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let event = qbit.authenticate().await;
    assert!(matches!(event, QbitAuthResult::Rejected));
}

#[tokio::test]
async fn authenticate_fails_body_returns_auth_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "wrong_pass".into(),
        Arc::new(|_| {}),
    );
    let event = qbit.authenticate().await;
    assert!(matches!(event, QbitAuthResult::Rejected));
}

#[tokio::test]
async fn authenticate_network_error_returns_connection_refused() {
    // Port 1 is privileged — guaranteed to refuse unprivileged connections.
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let event = qbit.authenticate().await;
    assert!(matches!(event, QbitAuthResult::ConnectionRefused));
}

// ── sync_port ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sync_port_returns_success_on_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/app/setPreferences"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let cookie = AuthCookie::new("abc123".to_string());
    let port = VpnPort::try_new(51820).unwrap();
    let event = qbit.sync_port(&cookie, port).await;
    assert!(matches!(event, QbitPortSyncResult::Success));
}

#[tokio::test]
async fn sync_port_returns_failed_with_status_on_403() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/app/setPreferences"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let cookie = AuthCookie::new("abc123".to_string());
    let port = VpnPort::try_new(51820).unwrap();
    let event = qbit.sync_port(&cookie, port).await;
    assert!(matches!(event, QbitPortSyncResult::Failed(403)));
}

// §36 step 9a: `list_torrents` deleted (was dead code); its tests went
// with it.  `list_torrent_details` covers the live qBit listing path.

// §36 step 9a: `list_torrents` was dead code; its 4 tests were deleted
// with it.  `list_torrent_details` covers the live qBit listing path.

#[tokio::test]
async fn authenticate_unexpected_response_returns_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let event = qbit.authenticate().await;
    assert!(
        matches!(event, QbitAuthResult::ApiError(503)),
        "Expected QbitAuthResult::ApiError(503), got {event:?}"
    );
}

#[tokio::test]
async fn sync_port_network_error_returns_failed_with_code_zero() {
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "admin".into(),
        "password".into(),
        Arc::new(|_| {}),
    );
    let cookie = AuthCookie::new("abc123".to_string());
    let port = VpnPort::try_new(51820).unwrap();
    let event = qbit.sync_port(&cookie, port).await;
    assert!(
        matches!(event, QbitPortSyncResult::Failed(0)),
        "Expected QbitPortSyncResult::Failed(0), got {event:?}"
    );
}

// ── list_torrent_details ──────────────────────────────────────────────────────

#[tokio::test]
async fn list_torrent_details_returns_parsed_records() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "hash": "abc123def456abc123def456abc123def456abc1",
                "name": "My Audiobook",
                "state": "uploading",
                "seeding_time": 7200u64,
                "downloaded": 1_048_576_u64,
                "comment": "https://www.myanonamouse.net/t/99999"
            },
            {
                "hash": "bbb111bbb111bbb111bbb111bbb111bbb111bbb1",
                "name": "Other Book",
                "state": "downloading",
                "seeding_time": 0u64,
                "downloaded": 0u64,
                "comment": ""
            }
        ])))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    let cookie = AuthCookie::new("sid".to_string());
    let details = qbit.list_torrent_details(&cookie).await;
    assert_eq!(details.len(), 2);
    assert_eq!(
        details[0].hash.0,
        "abc123def456abc123def456abc123def456abc1"
    );
    assert_eq!(details[0].state, QbitTorrentState::Uploading);
    assert_eq!(details[0].seeding_time_secs, 7200);
    assert_eq!(details[0].downloaded_bytes, 1_048_576);
    assert_eq!(
        details[0].mam_id,
        Some(MamTorrentId::try_new(99999).unwrap())
    );
    assert_eq!(details[1].mam_id, None);
}

#[tokio::test]
async fn list_torrent_details_maps_all_state_strings() {
    let states = [
        ("downloading", QbitTorrentState::Downloading),
        ("stalledDL", QbitTorrentState::StalledDownloading),
        ("uploading", QbitTorrentState::Uploading),
        ("stalledUP", QbitTorrentState::StalledUploading),
        ("forcedUP", QbitTorrentState::ForcedUpload),
        ("pausedDL", QbitTorrentState::PausedDownloading),
        ("stoppedDL", QbitTorrentState::PausedDownloading),
        ("pausedUP", QbitTorrentState::PausedUploading),
        ("stoppedUP", QbitTorrentState::PausedUploading),
        ("error", QbitTorrentState::Error),
    ];
    for (state_str, expected) in &states {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "hash": "aaa", "name": "t", "state": state_str,
                    "seeding_time": 0u64, "downloaded": 0u64, "comment": ""
                }])),
            )
            .mount(&server)
            .await;
        let qbit = QbitClient::new(
            reqwest::Client::new(),
            server.uri(),
            "a".into(),
            "p".into(),
            Arc::new(|_| {}),
        );
        let details = qbit
            .list_torrent_details(&AuthCookie::new("s".to_string()))
            .await;
        assert_eq!(&details[0].state, expected, "state={state_str}");
    }
}

#[tokio::test]
async fn list_torrent_details_returns_empty_on_bad_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    let details = qbit
        .list_torrent_details(&AuthCookie::new("s".to_string()))
        .await;
    assert!(details.is_empty());
}

#[tokio::test]
async fn list_torrent_details_returns_empty_on_network_error() {
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    let details = qbit
        .list_torrent_details(&AuthCookie::new("s".to_string()))
        .await;
    assert!(details.is_empty());
}

#[tokio::test]
async fn get_preferences_returns_parsed_limits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "max_active_torrents": 10i64,
            "max_active_downloads": 3i64,
            "max_active_uploads": 5i64,
            "listen_port": 51_820i64
        })))
        .mount(&server)
        .await;
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    let prefs = qbit
        .get_preferences(&AuthCookie::new("s".to_string()))
        .await;
    assert!(prefs.is_some());
    let p = prefs.unwrap();
    assert_eq!(p.torrents, 10);
    assert_eq!(p.downloads, 3);
    assert_eq!(p.uploads, 5);
    assert_eq!(
        p.listen_port,
        Some(windlass_types::VpnPort::try_new(51_820).unwrap())
    );
}

#[tokio::test]
async fn get_preferences_returns_none_on_bad_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_string("bad"))
        .mount(&server)
        .await;
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    let prefs = qbit
        .get_preferences(&AuthCookie::new("s".to_string()))
        .await;
    assert!(prefs.is_none());
}

// ── add_torrent ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn add_torrent_returns_hash_from_response_body() {
    let server = MockServer::start().await;
    let expected_hash = "a".repeat(40);
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200).set_body_string(expected_hash.clone()))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    let cookie = AuthCookie::new("sid".to_string());
    let result = qbit.add_torrent(&cookie, vec![0u8; 16]).await;
    assert_eq!(result, Some(TorrentHash(expected_hash)));
}

#[tokio::test]
async fn add_torrent_returns_none_on_error_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    let cookie = AuthCookie::new("sid".to_string());
    // Mock returns no torrent list fallback since /torrents/info isn't mocked
    let result = qbit.add_torrent(&cookie, vec![0u8; 16]).await;
    assert!(result.is_none());
}

// ── pause_torrent ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn pause_torrent_posts_correct_form() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/stop"))
        .and(wiremock::matchers::body_string_contains("hashes=abc123"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    let cookie = AuthCookie::new("sid".to_string());
    qbit.pause_torrent(&cookie, &TorrentHash("abc123".into()))
        .await;
    server.verify().await;
}

#[tokio::test]
async fn pause_torrent_does_not_panic_on_network_error() {
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    qbit.pause_torrent(
        &AuthCookie::new("s".to_string()),
        &TorrentHash("abc".into()),
    )
    .await;
    // no panic = pass
}

// ── resume_torrent ────────────────────────────────────────────────────────────

#[tokio::test]
async fn resume_torrent_posts_correct_form() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/start"))
        .and(wiremock::matchers::body_string_contains("hashes=abc123"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    qbit.resume_torrent(
        &AuthCookie::new("sid".to_string()),
        &TorrentHash("abc123".into()),
    )
    .await;
    server.verify().await;
}

#[tokio::test]
async fn resume_torrent_does_not_panic_on_network_error() {
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    qbit.resume_torrent(
        &AuthCookie::new("s".to_string()),
        &TorrentHash("abc".into()),
    )
    .await;
}

// ── force_resume_torrent ──────────────────────────────────────────────────────

#[tokio::test]
async fn force_resume_torrent_posts_correct_form() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/setForceStart"))
        .and(wiremock::matchers::body_string_contains("hashes=abc123"))
        .and(wiremock::matchers::body_string_contains("value=true"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    qbit.force_resume_torrent(
        &AuthCookie::new("sid".to_string()),
        &TorrentHash("abc123".into()),
    )
    .await;
    server.verify().await;
}

#[tokio::test]
async fn force_resume_torrent_does_not_panic_on_network_error() {
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    qbit.force_resume_torrent(
        &AuthCookie::new("s".to_string()),
        &TorrentHash("abc".into()),
    )
    .await;
}

// ── delete_torrent ────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_torrent_posts_with_delete_files_false() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .and(wiremock::matchers::body_string_contains("hashes=abc123"))
        .and(wiremock::matchers::body_string_contains(
            "deleteFiles=false",
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    qbit.delete_torrent(
        &AuthCookie::new("sid".to_string()),
        &TorrentHash("abc123".into()),
    )
    .await;
    server.verify().await;
}

#[tokio::test]
async fn delete_torrent_does_not_panic_on_network_error() {
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    qbit.delete_torrent(
        &AuthCookie::new("s".to_string()),
        &TorrentHash("abc".into()),
    )
    .await;
}

// ── set_all_files_priority ────────────────────────────────────────────────────

#[tokio::test]
async fn set_all_files_priority_posts_correct_form() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/filePrio"))
        .and(wiremock::matchers::body_string_contains("hash=abc123"))
        .and(wiremock::matchers::body_string_contains("priority=1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let qbit = QbitClient::new(
        reqwest::Client::new(),
        server.uri(),
        "admin".into(),
        "pass".into(),
        Arc::new(|_| {}),
    );
    qbit.set_all_files_priority(
        &AuthCookie::new("sid".to_string()),
        &TorrentHash("abc123".into()),
    )
    .await;
    server.verify().await;
}

#[tokio::test]
async fn set_all_files_priority_does_not_panic_on_network_error() {
    let qbit = QbitClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".into(),
        "a".into(),
        "p".into(),
        Arc::new(|_| {}),
    );
    qbit.set_all_files_priority(
        &AuthCookie::new("s".to_string()),
        &TorrentHash("abc".into()),
    )
    .await;
}
