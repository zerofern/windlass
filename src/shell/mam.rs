use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::core::events::Event;
use crate::types::VpnIp;

const MAM_SEEDBOX_URL: &str = "https://t.myanonamouse.net/json/dynamicSeedbox.php";
const MAM_LOAD_URL: &str = "https://www.myanonamouse.net/jsonLoad.php?clientStats";

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

/// Registers the current VPN IP with MAM via the dynamic seedbox endpoint.
/// IP is determined server-side from the proxied TCP connection — we send
/// no IP ourselves. Returns an updated session cookie if MAM rotated it.
pub async fn update_seedbox(client: &Client, session: &str) -> (Event, Option<String>) {
    update_seedbox_at(client, session, MAM_SEEDBOX_URL).await
}

/// Checks whether MAM reports the seedbox as connectable.
/// Uses `?clientStats` which includes the connectable field with a 30-min cache.
pub async fn check_connectability(client: &Client, session: &str) -> (Event, Option<String>) {
    check_connectability_at(client, session, MAM_LOAD_URL).await
}

pub async fn update_seedbox_at(
    client: &Client,
    session: &str,
    url: &str,
) -> (Event, Option<String>) {
    let result = client
        .get(url)
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

pub async fn check_connectability_at(
    client: &Client,
    session: &str,
    url: &str,
) -> (Event, Option<String>) {
    let result = client
        .get(url)
        .header(reqwest::header::COOKIE, format!("mam_id={session}"))
        .send()
        .await;

    let new_session = result.as_ref().ok().and_then(extract_mam_cookie);

    match result {
        Err(e) => {
            warn!("MAM connectivity check request failed: {e}");
            (Event::MamConnectabilityObserved(false), new_session)
        }
        Ok(resp) => {
            if !resp.status().is_success() {
                warn!("MAM connectivity check HTTP {}", resp.status());
                return (Event::MamConnectabilityObserved(false), new_session);
            }
            match resp.json::<JsonLoadResponse>().await {
                Ok(body) => {
                    let connectable = body
                        .connectable
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case("yes"));
                    debug!("MAM connectable={connectable}");
                    (Event::MamConnectabilityObserved(connectable), new_session)
                }
                Err(e) => {
                    warn!("MAM connectivity parse failed: {e}");
                    (Event::MamConnectabilityObserved(false), new_session)
                }
            }
        }
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

    fn client() -> Client {
        reqwest::Client::new()
    }

    // ── update_seedbox ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_seedbox_success_returns_mam_update_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "Success": true,
                    "msg": "No change",
                    "ip": "79.127.184.201"
                })),
            )
            .mount(&server)
            .await;

        let (event, cookie) =
            update_seedbox_at(&client(), "my_session", &server.uri()).await;
        assert!(matches!(event, Event::MamUpdateSuccess));
        assert!(cookie.is_none());
    }

    #[tokio::test]
    async fn update_seedbox_asn_mismatch_returns_event_with_ip() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "Success": false,
                    "msg": "Invalid session - ASN mismatch",
                    "ip": "79.127.184.201"
                })),
            )
            .mount(&server)
            .await;

        let (event, _) =
            update_seedbox_at(&client(), "my_session", &server.uri()).await;
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

        let (_, cookie) =
            update_seedbox_at(&client(), "old_cookie", &server.uri()).await;
        assert_eq!(cookie.as_deref(), Some("rotated_cookie"));
    }

    #[tokio::test]
    async fn update_seedbox_sends_cookie_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header_exists("cookie"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "Success": true,
                    "msg": "No change",
                    "ip": "79.127.184.201"
                })),
            )
            .mount(&server)
            .await;

        let (event, _) =
            update_seedbox_at(&client(), "my_session", &server.uri()).await;
        assert!(matches!(event, Event::MamUpdateSuccess));
    }

    // ── check_connectability ──────────────────────────────────────────────────

    #[tokio::test]
    async fn check_connectability_returns_true_when_connectable_yes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "connectable": "yes",
                    "username": "BrightVoyage"
                })),
            )
            .mount(&server)
            .await;

        let (event, _) =
            check_connectability_at(&client(), "my_session", &server.uri()).await;
        assert!(matches!(event, Event::MamConnectabilityObserved(true)));
    }

    #[tokio::test]
    async fn check_connectability_returns_false_when_connectable_no() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "connectable": "no",
                    "username": "BrightVoyage"
                })),
            )
            .mount(&server)
            .await;

        let (event, _) =
            check_connectability_at(&client(), "my_session", &server.uri()).await;
        assert!(matches!(event, Event::MamConnectabilityObserved(false)));
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

        let (event, _) =
            check_connectability_at(&client(), "my_session", &server.uri()).await;
        assert!(matches!(event, Event::MamConnectabilityObserved(false)));
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

        let (_, cookie) =
            check_connectability_at(&client(), "old_cookie", &server.uri()).await;
        assert_eq!(cookie.as_deref(), Some("new_cookie"));
    }

    #[tokio::test]
    async fn check_connectability_returns_false_on_http_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let (event, _) =
            check_connectability_at(&client(), "my_session", &server.uri()).await;
        assert!(matches!(event, Event::MamConnectabilityObserved(false)));
    }
}
