use std::sync::{Arc, Mutex};

use anyhow::bail;
use chrono::Utc;
use serde::Deserialize;
use tracing::{debug, info, warn};

use windlass_core::HttpObserver;
use windlass_core::events::Event;
use windlass_types::{HttpExchange, MamStatus, VpnIp};

#[derive(Deserialize)]
struct DynamicSeedboxResponse {
    #[serde(rename = "Success")]
    success: bool,
    msg: String,
    ip: String,
}

#[derive(Deserialize)]
struct JsonLoadResponse {
    connectable: Option<String>,
    #[serde(rename = "unsat")]
    unsat: Option<UnsatSummary>,
}

#[derive(Deserialize, Debug)]
struct UnsatSummary {
    pub count: u64,
    pub limit: u64,
}

/// Wraps a VPN-routed `reqwest::Client` together with the MAM connection
/// details and a rotating session cookie. All MAM operations are methods
/// so call sites only pass `&self`.
#[derive(Clone)]
pub struct MamClient {
    client: reqwest::Client,
    session: Arc<Mutex<String>>,
    check_session_url: String,
    seedbox_url: String,
    load_url: String,
    torrent_base_url: String,
    last_request_at: Arc<Mutex<Option<std::time::Instant>>>,
    on_http: HttpObserver,
}

impl MamClient {
    /// # Errors
    /// Returns an error if the reqwest client cannot be built (e.g. invalid proxy URL).
    pub fn new(
        proxy_url: Option<&str>,
        session: String,
        seedbox_url: String,
        load_url: String,
        user_agent: &str,
        on_http: HttpObserver,
    ) -> anyhow::Result<Self> {
        let builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(user_agent);
        let builder = if let Some(url) = proxy_url {
            builder.proxy(reqwest::Proxy::all(url)?)
        } else {
            builder
        };
        let client = builder.build()?;
        Ok(Self {
            client,
            session: Arc::new(Mutex::new(session)),
            check_session_url: "https://www.myanonamouse.net/json/checkCookie.php".into(),
            seedbox_url,
            load_url,
            torrent_base_url: "https://www.myanonamouse.net".into(),
            last_request_at: Arc::new(Mutex::new(None)),
            on_http,
        })
    }

    /// Validates the `mam_id` session against MAM's checkCookie endpoint.
    /// Returns `Ok(())` if valid, `Err` if the session is rejected or unreachable.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response indicates an
    /// invalid session.
    ///
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    pub async fn check_session(&self) -> anyhow::Result<()> {
        let current = self.session.lock().unwrap().clone();
        let resp = self
            .client
            .get(&self.check_session_url)
            .header(reqwest::header::COOKIE, format!("mam_id={current}"))
            .send()
            .await?;
        if resp.status().is_client_error() || resp.status().is_server_error() {
            bail!("MAM session check failed: HTTP {}", resp.status());
        }
        if let Some(rotated) = extract_mam_cookie(&resp) {
            *self.session.lock().unwrap() = rotated;
        }
        info!("MAM session valid");
        Ok(())
    }

    /// Registers the current VPN IP with MAM via the dynamic seedbox endpoint.
    ///
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    pub async fn update_seedbox(&self) -> Event {
        if !self.check_rate_limit() {
            return Event::MamRateLimitViolation { at: Utc::now() };
        }
        let current = self.session.lock().unwrap().clone();
        let (event, new_session) = self.do_update_seedbox(&current).await;
        if let Some(rotated) = new_session {
            *self.session.lock().unwrap() = rotated;
        }
        event
    }

    /// Checks whether MAM reports the seedbox as connectable.
    ///
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    pub async fn check_connectability(&self) -> Event {
        if !self.check_rate_limit() {
            return Event::MamRateLimitViolation { at: Utc::now() };
        }
        let current = self.session.lock().unwrap().clone();
        let (event, new_session) = self.do_check_connectability(&current).await;
        if let Some(rotated) = new_session {
            *self.session.lock().unwrap() = rotated;
        }
        event
    }

    /// Returns `true` if the request can proceed (≥400ms since last request).
    /// Returns `false` if the guard triggers — a `MamRateLimitViolation` event
    /// will be emitted by the caller.
    fn check_rate_limit(&self) -> bool {
        let mut last = self.last_request_at.lock().unwrap();
        if let Some(t) = *last
            && t.elapsed() < std::time::Duration::from_millis(400)
        {
            warn!("MAM rate limit guard triggered");
            return false;
        }
        *last = Some(std::time::Instant::now());
        true
    }

    fn emit_http(&self, url: &str, response_status: u16, response_body: &str) {
        (self.on_http)(HttpExchange {
            module: "mam".into(),
            method: "GET".into(),
            url: url.into(),
            request_body: None,
            response_status,
            response_body: response_body.into(),
        });
    }

    async fn do_update_seedbox(&self, session: &str) -> (Event, Option<String>) {
        let result = self
            .client
            .get(&self.seedbox_url)
            .header(reqwest::header::COOKIE, format!("mam_id={session}"))
            .send()
            .await;

        let new_session = result.as_ref().ok().and_then(extract_mam_cookie);

        match result {
            Err(e) => {
                warn!("MAM seedbox update request failed: {e}");
                (Event::MamUpdateSuccess { at: Utc::now() }, new_session)
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                let raw = resp.text().await.unwrap_or_default();
                self.emit_http(&self.seedbox_url, status, &raw);
                match serde_json::from_str::<DynamicSeedboxResponse>(&raw) {
                    Ok(body) if body.success => {
                        info!("MAM seedbox: {}", body.msg);
                        (Event::MamUpdateSuccess { at: Utc::now() }, new_session)
                    }
                    Ok(body) if body.msg.contains("ASN mismatch") => {
                        let ip = body
                            .ip
                            .trim()
                            .parse()
                            .map(VpnIp)
                            .unwrap_or(VpnIp(std::net::Ipv4Addr::UNSPECIFIED));
                        warn!("MAM ASN mismatch: ip={}", ip.0);
                        (Event::MamAsnMismatch { at: Utc::now(), ip }, new_session)
                    }
                    Ok(body) => {
                        warn!("MAM seedbox non-success: {}", body.msg);
                        (Event::MamUpdateSuccess { at: Utc::now() }, new_session)
                    }
                    Err(e) => {
                        warn!("MAM seedbox response parse failed: {e}");
                        (Event::MamUpdateSuccess { at: Utc::now() }, new_session)
                    }
                }
            }
        }
    }

    async fn do_check_connectability(&self, session: &str) -> (Event, Option<String>) {
        let result = self
            .client
            .get(&self.load_url)
            .header(reqwest::header::COOKIE, format!("mam_id={session}"))
            .send()
            .await;

        let new_session = result.as_ref().ok().and_then(extract_mam_cookie);

        match result {
            Err(e) => {
                warn!("MAM connectivity check request failed: {e}");
                (
                    Event::MamStatusObserved {
                        at: Utc::now(),
                        status: MamStatus::Unreachable,
                    },
                    new_session,
                )
            }
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    warn!("MAM connectivity check HTTP {}", status);
                    self.emit_http(&self.load_url, status.as_u16(), "");
                    return (
                        Event::MamStatusObserved {
                            at: Utc::now(),
                            status: MamStatus::Unreachable,
                        },
                        new_session,
                    );
                }
                let raw = resp.text().await.unwrap_or_default();
                self.emit_http(&self.load_url, status.as_u16(), &raw);
                match serde_json::from_str::<JsonLoadResponse>(&raw) {
                    Ok(body) => {
                        let connectable = body
                            .connectable
                            .as_deref()
                            .is_some_and(|s| s.eq_ignore_ascii_case("yes"));
                        debug!("MAM connectable={connectable}");
                        if let Some(ref unsat) = body.unsat {
                            debug!("MAM unsat: {}/{}", unsat.count, unsat.limit);
                        }
                        let mam_status = if connectable {
                            MamStatus::Connectable
                        } else {
                            MamStatus::NotConnectable
                        };
                        (
                            Event::MamStatusObserved {
                                at: Utc::now(),
                                status: mam_status,
                            },
                            new_session,
                        )
                    }
                    Err(e) => {
                        warn!("MAM connectivity parse failed: {e}");
                        (
                            Event::MamStatusObserved {
                                at: Utc::now(),
                                status: MamStatus::Unreachable,
                            },
                            new_session,
                        )
                    }
                }
            }
        }
    }

    #[cfg(test)]
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    #[must_use]
    pub fn session_value(&self) -> String {
        self.session.lock().unwrap().clone()
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_check_session_url(mut self, url: String) -> Self {
        self.check_session_url = url;
        self
    }

    /// Downloads the `.torrent` file bytes for a given MAM torrent ID.
    ///
    /// URL: `{torrent_base_url}/tor/download.php?tid={mam_id}`
    /// Returns `None` on any network or HTTP error.
    ///
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    pub async fn fetch_torrent(&self, mam_id: windlass_types::MamTorrentId) -> Option<Vec<u8>> {
        let current = self.session.lock().unwrap().clone();
        let url = format!(
            "{}/tor/download.php?tid={}",
            self.torrent_base_url, mam_id.0
        );
        let resp = self
            .client
            .get(&url)
            .header(reqwest::header::COOKIE, format!("mam_id={current}"))
            .send()
            .await
            .ok()?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            self.emit_http(&url, status, "");
            return None;
        }
        let bytes = resp.bytes().await.ok()?;
        self.emit_http(&url, status, "<binary torrent data>");
        Some(bytes.to_vec())
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_torrent_base_url(mut self, url: String) -> Self {
        self.torrent_base_url = url;
        self
    }
}

fn extract_mam_cookie(resp: &reqwest::Response) -> Option<String> {
    for value in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = value.to_str() {
            for part in s.split(';') {
                if let Some(val) = part.trim().strip_prefix("mam_id=") {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use wiremock::matchers::{header_exists, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── update_seedbox ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_seedbox_success_returns_mam_update_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Success": true,
                "msg": "No change",
                "ip": "79.127.184.201"
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamUpdateSuccess { .. }));
    }

    #[tokio::test]
    async fn update_seedbox_asn_mismatch_returns_event_with_ip() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Success": false,
                "msg": "Invalid session - ASN mismatch",
                "ip": "79.127.184.201"
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.update_seedbox().await;
        assert!(
            matches!(event, Event::MamAsnMismatch { ip, .. } if ip.0.to_string() == "79.127.184.201")
        );
    }

    #[tokio::test]
    async fn update_seedbox_rotates_cookie_from_set_cookie_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Set-Cookie", "mam_id=rotated_cookie; Path=/; HttpOnly")
                    .set_body_json(serde_json::json!({
                        "Success": true,
                        "msg": "No change",
                        "ip": "79.127.184.201"
                    })),
            )
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "old_cookie".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        mam.update_seedbox().await;
        assert_eq!(mam.session_value(), "rotated_cookie");
    }

    #[tokio::test]
    async fn update_seedbox_sends_cookie_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header_exists("cookie"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Success": true,
                "msg": "No change",
                "ip": "79.127.184.201"
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamUpdateSuccess { .. }));
    }

    // ── check_connectability ──────────────────────────────────────────────────

    #[tokio::test]
    async fn check_connectability_returns_true_when_connectable_yes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "connectable": "yes",
                "username": "BrightVoyage"
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved {
                status: MamStatus::Connectable,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn check_connectability_returns_false_when_connectable_no() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "connectable": "no",
                "username": "BrightVoyage"
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved {
                status: MamStatus::NotConnectable,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn check_connectability_returns_false_when_field_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "username": "BrightVoyage" })),
            )
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved {
                status: MamStatus::NotConnectable,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn check_connectability_rotates_cookie() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Set-Cookie", "mam_id=new_cookie; Path=/; HttpOnly")
                    .set_body_json(serde_json::json!({ "connectable": "yes" })),
            )
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "old_cookie".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        mam.check_connectability().await;
        assert_eq!(mam.session_value(), "new_cookie");
    }

    #[tokio::test]
    async fn check_connectability_returns_false_on_http_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved {
                status: MamStatus::Unreachable,
                ..
            }
        ));
    }

    // ── check_session ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn check_session_ok_returns_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap()
        .with_check_session_url(server.uri());
        assert!(mam.check_session().await.is_ok());
    }

    #[tokio::test]
    async fn check_session_error_status_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap()
        .with_check_session_url(server.uri());
        assert!(mam.check_session().await.is_err());
    }

    #[tokio::test]
    async fn check_session_rotates_cookie() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Set-Cookie", "mam_id=rotated; Path=/; HttpOnly"),
            )
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "old_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap()
        .with_check_session_url(server.uri());
        mam.check_session().await.unwrap();
        assert_eq!(mam.session_value(), "rotated");
    }

    // ── rate limiting ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_seedbox_rate_limit_returns_violation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Success": true, "msg": "ok", "ip": "1.2.3.4"
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        // First call consumes the rate limit slot.
        mam.update_seedbox().await;
        // Second call immediately after should be rate-limited.
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamRateLimitViolation { .. }));
    }

    #[tokio::test]
    async fn check_connectability_rate_limit_returns_violation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "connectable": "yes" })),
            )
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        mam.check_connectability().await;
        let event = mam.check_connectability().await;
        assert!(matches!(event, Event::MamRateLimitViolation { .. }));
    }

    // ── do_update_seedbox error paths ─────────────────────────────────────────

    #[tokio::test]
    async fn update_seedbox_network_error_returns_success() {
        // Network failure is treated as "no-op success" — the Core retries on a wakeup.
        let mam = MamClient::new(
            None,
            "my_session".into(),
            "http://127.0.0.1:1".into(),
            "http://127.0.0.1:1".into(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamUpdateSuccess { .. }));
    }

    #[tokio::test]
    async fn update_seedbox_non_success_non_asn_returns_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Success": false,
                "msg": "Some other error",
                "ip": "1.2.3.4"
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamUpdateSuccess { .. }));
    }

    #[tokio::test]
    async fn update_seedbox_unparseable_body_returns_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamUpdateSuccess { .. }));
    }

    // ── do_check_connectability error paths ───────────────────────────────────

    #[tokio::test]
    async fn check_connectability_network_error_returns_unreachable() {
        let mam = MamClient::new(
            None,
            "my_session".into(),
            "http://127.0.0.1:1".into(),
            "http://127.0.0.1:1".into(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved {
                status: MamStatus::Unreachable,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn check_connectability_unparseable_body_returns_unreachable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved {
                status: MamStatus::Unreachable,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn check_connectability_with_unsat_field_returns_connectable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "connectable": "yes",
                "unsat": { "count": 2, "limit": 10 }
            })))
            .mount(&server)
            .await;

        let mam = MamClient::new(
            None,
            "my_session".into(),
            server.uri(),
            server.uri(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap();
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved {
                status: MamStatus::Connectable,
                ..
            }
        ));
    }

    // ── constructor ───────────────────────────────────────────────────────────

    #[test]
    fn new_with_proxy_url_builds_client() {
        // A local socks5 proxy address — client builds without error.
        let result = MamClient::new(
            Some("socks5://127.0.0.1:1080"),
            "session".into(),
            "http://example.com".into(),
            "http://example.com".into(),
            "windlass",
            Arc::new(|_| {}),
        );
        assert!(result.is_ok());
    }

    // ── fetch_torrent ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_torrent_returns_bytes_on_success() {
        use wiremock::matchers::{path_regex, query_param};
        let server = MockServer::start().await;
        let torrent_bytes = b"d8:announce...e".to_vec();
        Mock::given(method("GET"))
            .and(path_regex("/tor/download.php"))
            .and(query_param("tid", "12345"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(torrent_bytes.clone()))
            .mount(&server)
            .await;

        let base = server.uri();
        let mam = MamClient::new(
            None,
            "my_session".into(),
            base.clone(),
            base.clone(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap()
        .with_torrent_base_url(base);
        let result = mam.fetch_torrent(windlass_types::MamTorrentId(12345)).await;
        assert_eq!(result, Some(torrent_bytes));
    }

    #[tokio::test]
    async fn fetch_torrent_returns_none_on_403() {
        use wiremock::matchers::path_regex;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/tor/download.php"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let base = server.uri();
        let mam = MamClient::new(
            None,
            "my_session".into(),
            base.clone(),
            base.clone(),
            "windlass",
            Arc::new(|_| {}),
        )
        .unwrap()
        .with_torrent_base_url(base);
        let result = mam.fetch_torrent(windlass_types::MamTorrentId(99)).await;
        assert!(result.is_none());
    }
}
