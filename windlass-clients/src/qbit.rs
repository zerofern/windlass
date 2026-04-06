use serde::Deserialize;
use tracing::{debug, warn};

use windlass_core::Observation;
use windlass_core::events::Event;
use windlass_debug::DebugController;
use windlass_types::{AuthCookie, HttpStatusCode, TorrentName, VpnPort};

#[derive(Deserialize)]
struct TorrentInfo {
    name: String,
}

/// Wraps a `reqwest::Client` together with the qBittorrent connection details.
/// All qBittorrent operations are methods so call sites only pass `&self`.
#[derive(Clone)]
pub struct QbitClient {
    client: reqwest::Client,
    base_url: String,
    user: String,
    pass: String,
    debug_ctrl: DebugController,
}

impl QbitClient {
    #[must_use]
    // DebugController wraps an Arc — cannot be const.
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        user: String,
        pass: String,
        debug_ctrl: DebugController,
    ) -> Self {
        Self {
            client,
            base_url,
            user,
            pass,
            debug_ctrl,
        }
    }

    fn emit_http(
        &self,
        method: &str,
        url: &str,
        request_body: Option<String>,
        response_status: u16,
        response_body: &str,
    ) {
        if let Some(tx) = self.debug_ctrl.obs_sender() {
            let _ = tx.send(Observation::HttpExchange {
                module: "qbit".into(),
                method: method.into(),
                url: url.into(),
                request_body,
                response_status,
                response_body: response_body.into(),
            });
        }
    }

    /// Authenticates with qBittorrent and returns the SID cookie on success.
    pub async fn authenticate(&self) -> Event {
        let url = format!("{}/api/v2/auth/login", self.base_url);
        match self
            .client
            .post(&url)
            .form(&[
                ("username", self.user.as_str()),
                ("password", self.pass.as_str()),
            ])
            .send()
            .await
        {
            Err(e) => {
                // Connection refused is normal during container startup — report as
                // ConnectionRefused so the Core can retry silently without alerting.
                debug!("qBit auth request failed (connection): {e}");
                Event::QbitConnectionRefused
            }
            Ok(resp) => {
                let status = resp.status();
                let sid = extract_sid_cookie(&resp);
                let body = resp.text().await.unwrap_or_default();
                self.emit_http("POST", &url, None, status.as_u16(), &body);

                if status.is_success() && body.trim() == "Ok." {
                    let Some(cookie) = sid else {
                        warn!("qBit auth: ok status but no SID cookie in response");
                        return Event::QbitAuthFailed;
                    };
                    debug!("qBit auth success");
                    return Event::QbitAuthSuccess(AuthCookie(cookie));
                }
                if body.trim() == "Fails." {
                    warn!("qBit auth: credentials rejected (Fails.)");
                    return Event::QbitAuthFailed;
                }
                warn!("qBit auth unexpected response: status={status}, body={body:?}");
                Event::QbitApiError(HttpStatusCode(status.as_u16()))
            }
        }
    }

    /// Updates qBittorrent's listen port via the preferences API.
    pub async fn sync_port(&self, cookie: &AuthCookie, port: VpnPort) -> Event {
        let url = format!("{}/api/v2/app/setPreferences", self.base_url);
        let req_body = format!(r#"{{"listen_port":"{}"}}"#, port.into_inner());
        match self
            .client
            .post(&url)
            .header(reqwest::header::COOKIE, format!("SID={}", cookie.0))
            .form(&[("json", &req_body)])
            .send()
            .await
        {
            Err(e) => {
                warn!("qBit port sync request failed: {e}");
                Event::QbitPortSyncFailed(HttpStatusCode(0))
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                self.emit_http("POST", &url, Some(req_body), status.as_u16(), &body);
                if status.is_success() {
                    debug!("qBit port sync success");
                    Event::QbitPortSyncSuccess
                } else {
                    warn!("qBit port sync failed: status={status}");
                    Event::QbitPortSyncFailed(HttpStatusCode(status.as_u16()))
                }
            }
        }
    }

    /// Fetches the current list of torrent names from qBittorrent.
    /// Returns an empty vec on error rather than propagating — the torrent
    /// checker treats an empty result as "no new torrents" and reschedules.
    pub async fn list_torrents(&self, cookie: &AuthCookie) -> Vec<TorrentName> {
        let url = format!("{}/api/v2/torrents/info", self.base_url);
        match self
            .client
            .get(&url)
            .header(reqwest::header::COOKIE, format!("SID={}", cookie.0))
            .send()
            .await
        {
            Err(e) => {
                warn!("Failed to list torrents: {e}");
                vec![]
            }
            Ok(resp) => match resp.json::<Vec<TorrentInfo>>().await {
                Ok(torrents) => torrents.into_iter().map(|t| TorrentName(t.name)).collect(),
                Err(e) => {
                    warn!("Failed to parse torrent list: {e}");
                    vec![]
                }
            },
        }
    }
}

fn extract_sid_cookie(resp: &reqwest::Response) -> Option<String> {
    for value in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = value.to_str() {
            for part in s.split(';') {
                if let Some(sid) = part.trim().strip_prefix("SID=") {
                    return Some(sid.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_debug::DebugController;
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
            DebugController::new(),
        );
        let event = qbit.authenticate().await;
        assert!(
            matches!(&event, Event::QbitAuthSuccess(AuthCookie(s)) if s == "abc123"),
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
            DebugController::new(),
        );
        let event = qbit.authenticate().await;
        assert!(matches!(event, Event::QbitAuthFailed));
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
            DebugController::new(),
        );
        let event = qbit.authenticate().await;
        assert!(matches!(event, Event::QbitAuthFailed));
    }

    #[tokio::test]
    async fn authenticate_network_error_returns_connection_refused() {
        // Port 1 is privileged — guaranteed to refuse unprivileged connections.
        let qbit = QbitClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1".into(),
            "admin".into(),
            "password".into(),
            DebugController::new(),
        );
        let event = qbit.authenticate().await;
        assert!(matches!(event, Event::QbitConnectionRefused));
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
            DebugController::new(),
        );
        let cookie = AuthCookie("abc123".into());
        let port = VpnPort::try_new(51820).unwrap();
        let event = qbit.sync_port(&cookie, port).await;
        assert!(matches!(event, Event::QbitPortSyncSuccess));
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
            DebugController::new(),
        );
        let cookie = AuthCookie("abc123".into());
        let port = VpnPort::try_new(51820).unwrap();
        let event = qbit.sync_port(&cookie, port).await;
        assert!(matches!(
            event,
            Event::QbitPortSyncFailed(HttpStatusCode(403))
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
            DebugController::new(),
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
            DebugController::new(),
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
            DebugController::new(),
        );
        let cookie = AuthCookie("abc123".into());
        let names = qbit.list_torrents(&cookie).await;
        assert!(names.is_empty());
    }
}
