use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tracing::{debug, info, warn};

use windlass_core::events::Event;
use windlass_types::{MamStatus, VpnIp};

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
}

impl MamClient {
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        session: String,
        seedbox_url: String,
        load_url: String,
    ) -> Self {
        Self {
            client,
            session: Arc::new(Mutex::new(session)),
            seedbox_url,
            load_url,
        }
    }

    /// Registers the current VPN IP with MAM via the dynamic seedbox endpoint.
    ///
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    pub async fn update_seedbox(&self) -> Event {
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
        let current = self.session.lock().unwrap().clone();
        let (event, new_session) = self.do_check_connectability(&current).await;
        if let Some(rotated) = new_session {
            *self.session.lock().unwrap() = rotated;
        }
        event
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
                (Event::MamUpdateSuccess, new_session)
            }
            Ok(resp) => match resp.json::<DynamicSeedboxResponse>().await {
                Ok(body) if body.success => {
                    info!("MAM seedbox: {}", body.msg);
                    (Event::MamUpdateSuccess, new_session)
                }
                Ok(body) if body.msg.contains("ASN mismatch") => {
                    let ip = body
                        .ip
                        .trim()
                        .parse()
                        .map(VpnIp)
                        .unwrap_or(VpnIp(std::net::Ipv4Addr::UNSPECIFIED));
                    warn!("MAM ASN mismatch: ip={}", ip.0);
                    (Event::MamAsnMismatch(ip), new_session)
                }
                Ok(body) => {
                    warn!("MAM seedbox non-success: {}", body.msg);
                    (Event::MamUpdateSuccess, new_session)
                }
                Err(e) => {
                    warn!("MAM seedbox response parse failed: {e}");
                    (Event::MamUpdateSuccess, new_session)
                }
            },
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
                    Event::MamStatusObserved(MamStatus::Unreachable),
                    new_session,
                )
            }
            Ok(resp) => {
                if !resp.status().is_success() {
                    warn!("MAM connectivity check HTTP {}", resp.status());
                    return (
                        Event::MamStatusObserved(MamStatus::Unreachable),
                        new_session,
                    );
                }
                match resp.json::<JsonLoadResponse>().await {
                    Ok(body) => {
                        let connectable = body
                            .connectable
                            .as_deref()
                            .is_some_and(|s| s.eq_ignore_ascii_case("yes"));
                        debug!("MAM connectable={connectable}");
                        let status = if connectable {
                            MamStatus::Connectable
                        } else {
                            MamStatus::NotConnectable
                        };
                        (Event::MamStatusObserved(status), new_session)
                    }
                    Err(e) => {
                        warn!("MAM connectivity parse failed: {e}");
                        (
                            Event::MamStatusObserved(MamStatus::Unreachable),
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
            reqwest::Client::new(),
            "my_session".into(),
            server.uri(),
            server.uri(),
        );
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamUpdateSuccess));
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
            reqwest::Client::new(),
            "my_session".into(),
            server.uri(),
            server.uri(),
        );
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamAsnMismatch(ip) if ip.0.to_string() == "79.127.184.201"));
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
            reqwest::Client::new(),
            "old_cookie".into(),
            server.uri(),
            server.uri(),
        );
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
            reqwest::Client::new(),
            "my_session".into(),
            server.uri(),
            server.uri(),
        );
        let event = mam.update_seedbox().await;
        assert!(matches!(event, Event::MamUpdateSuccess));
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
            reqwest::Client::new(),
            "my_session".into(),
            server.uri(),
            server.uri(),
        );
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved(MamStatus::Connectable)
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
            reqwest::Client::new(),
            "my_session".into(),
            server.uri(),
            server.uri(),
        );
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved(MamStatus::NotConnectable)
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
            reqwest::Client::new(),
            "my_session".into(),
            server.uri(),
            server.uri(),
        );
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved(MamStatus::NotConnectable)
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
            reqwest::Client::new(),
            "old_cookie".into(),
            server.uri(),
            server.uri(),
        );
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
            reqwest::Client::new(),
            "my_session".into(),
            server.uri(),
            server.uri(),
        );
        let event = mam.check_connectability().await;
        assert!(matches!(
            event,
            Event::MamStatusObserved(MamStatus::Unreachable)
        ));
    }
}
