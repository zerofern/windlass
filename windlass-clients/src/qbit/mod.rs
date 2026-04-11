#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

mod client;
mod types;

pub use client::QbitClient;
pub use types::{QbitPreferences, QbitTorrentDetails, QbitTorrentState, parse_mam_id};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{QbitClient, QbitTorrentState};
    use windlass_core::events::Event;
    use windlass_types::{AuthCookie, HttpStatusCode, MamTorrentId, TorrentName, VpnPort};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── authenticate ──────────────────────────────────────────────────────────

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
        assert!(
            matches!(&event, Event::QbitAuthSuccess { cookie: AuthCookie(s), .. } if s == "abc123"),
            "Expected QbitAuthSuccess(abc123), got {event:?}"
        );
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
        assert!(matches!(event, Event::QbitAuthFailed { .. }));
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
        assert!(matches!(event, Event::QbitAuthFailed { .. }));
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
        assert!(matches!(event, Event::QbitConnectionRefused { .. }));
    }

    // ── sync_port ─────────────────────────────────────────────────────────────

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
        let cookie = AuthCookie("abc123".into());
        let port = VpnPort::try_new(51820).unwrap();
        let event = qbit.sync_port(&cookie, port).await;
        assert!(matches!(event, Event::QbitPortSyncSuccess { .. }));
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
        let cookie = AuthCookie("abc123".into());
        let port = VpnPort::try_new(51820).unwrap();
        let event = qbit.sync_port(&cookie, port).await;
        assert!(matches!(
            event,
            Event::QbitPortSyncFailed {
                code: HttpStatusCode(403),
                ..
            }
        ));
    }

    // ── list_torrents ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_torrents_returns_names_from_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "Album A", "hash": "aaa"},
                {"name": "Album B", "hash": "bbb"}
            ])))
            .mount(&server)
            .await;

        let qbit = QbitClient::new(
            reqwest::Client::new(),
            server.uri(),
            "admin".into(),
            "password".into(),
            Arc::new(|_| {}),
        );
        let cookie = AuthCookie("abc123".into());
        let names = qbit.list_torrents(&cookie).await;
        assert_eq!(
            names,
            vec![TorrentName("Album A".into()), TorrentName("Album B".into())]
        );
    }

    #[tokio::test]
    async fn list_torrents_returns_empty_on_empty_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let qbit = QbitClient::new(
            reqwest::Client::new(),
            server.uri(),
            "admin".into(),
            "password".into(),
            Arc::new(|_| {}),
        );
        let cookie = AuthCookie("abc123".into());
        let names = qbit.list_torrents(&cookie).await;
        assert!(names.is_empty());
    }

    #[tokio::test]
    async fn list_torrents_returns_empty_on_bad_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let qbit = QbitClient::new(
            reqwest::Client::new(),
            server.uri(),
            "admin".into(),
            "password".into(),
            Arc::new(|_| {}),
        );
        let cookie = AuthCookie("abc123".into());
        let names = qbit.list_torrents(&cookie).await;
        assert!(names.is_empty());
    }

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
            matches!(
                event,
                Event::QbitApiError {
                    code: HttpStatusCode(503),
                    ..
                }
            ),
            "Expected QbitApiError(503), got {event:?}"
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
        let cookie = AuthCookie("abc123".into());
        let port = VpnPort::try_new(51820).unwrap();
        let event = qbit.sync_port(&cookie, port).await;
        assert!(
            matches!(
                event,
                Event::QbitPortSyncFailed {
                    code: HttpStatusCode(0),
                    ..
                }
            ),
            "Expected QbitPortSyncFailed(0), got {event:?}"
        );
    }

    #[tokio::test]
    async fn list_torrents_network_error_returns_empty() {
        let qbit = QbitClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1".into(),
            "admin".into(),
            "password".into(),
            Arc::new(|_| {}),
        );
        let cookie = AuthCookie("abc123".into());
        let names = qbit.list_torrents(&cookie).await;
        assert!(names.is_empty());
    }

    // ── list_torrent_details ──────────────────────────────────────────────────

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
                    "downloaded": 1048576u64,
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
        let cookie = AuthCookie("sid".into());
        let details = qbit.list_torrent_details(&cookie).await;
        assert_eq!(details.len(), 2);
        assert_eq!(
            details[0].hash.0,
            "abc123def456abc123def456abc123def456abc1"
        );
        assert_eq!(details[0].state, QbitTorrentState::Uploading);
        assert_eq!(details[0].seeding_time_secs, 7200);
        assert_eq!(details[0].downloaded_bytes, 1_048_576);
        assert_eq!(details[0].mam_id, Some(MamTorrentId(99999)));
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
            ("pausedUP", QbitTorrentState::PausedUploading),
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
            let details = qbit.list_torrent_details(&AuthCookie("s".into())).await;
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
        let details = qbit.list_torrent_details(&AuthCookie("s".into())).await;
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
        let details = qbit.list_torrent_details(&AuthCookie("s".into())).await;
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
                "max_active_uploads": 5i64
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
        let prefs = qbit.get_preferences(&AuthCookie("s".into())).await;
        assert!(prefs.is_some());
        let p = prefs.unwrap();
        assert_eq!(p.torrents, 10);
        assert_eq!(p.downloads, 3);
        assert_eq!(p.uploads, 5);
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
        let prefs = qbit.get_preferences(&AuthCookie("s".into())).await;
        assert!(prefs.is_none());
    }
}
