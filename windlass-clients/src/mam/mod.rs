use std::sync::{Arc, Mutex};

use anyhow::bail;
use chrono::Utc;
use serde::Deserialize;
use tracing::{debug, info, warn};

use windlass_core::events::Event;
use windlass_core::HttpObserver;
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
    seedbox_url: String,
    load_url: String,
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
            seedbox_url,
            load_url,
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
            .get("https://www.myanonamouse.net/json/checkCookie.php")
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
    pub fn session_value(&self) -> String {
        self.session.lock().unwrap().clone()
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
}
