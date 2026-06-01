use std::sync::{Arc, Mutex};

use anyhow::bail;
use serde::Deserialize;
use tracing::{debug, info, warn};

use windlass_types::{HttpExchange, HttpObserver, VpnIp};

/// §36 step 9a: typed result for `MamClient::update_seedbox`.  Replaces
/// the legacy `windlass_core::Event::Mam*` shape so the shell can map
/// to `MamEvent` without depending on legacy core types.
#[derive(Debug, Clone)]
pub enum MamSeedboxResult {
    Success {
        registered_ip: Option<VpnIp>,
        registered_asn: Option<u32>,
        registered_as: Option<String>,
    },
    /// MAM rejected the update with an ASN mismatch (§30).
    AsnMismatch { ip: VpnIp },
    /// Transport-level failure — DNS / TCP / TLS / timeout.
    Unreachable { reason: String },
    /// MAM's documented 1-hour rolling rate limit, or the operator's
    /// 400ms inter-request guard.
    RateLimited,
    /// MAM responded with an error (non-1.0 IP/port, etc.).
    Failed { reason: String },
}

#[derive(Deserialize)]
struct DynamicSeedboxResponse {
    #[serde(rename = "Success")]
    success: bool,
    msg: String,
    ip: String,
    /// §32: ASN number MAM has recorded for this IP.  Mousehole's
    /// dedup compares against this.  Present in every documented
    /// dynamic-seedbox response (success or error).
    #[serde(rename = "ASN", default)]
    asn: u32,
    /// §32: AS organization name (e.g. "Mullvad AB").  Carried for
    /// logging and as input to future ASN-aware dedup work.
    #[serde(rename = "AS", default)]
    as_org: String,
}

/// §32: typed enumeration of the known `msg` values MAM returns from the
/// dynamic-seedbox endpoint.  Captured from the official API docs to avoid
/// brittle substring matching across the call sites.
///
/// `Other(String)` carries any future or undocumented message verbatim so
/// the operator can still see what MAM said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DynamicSeedboxMsg {
    Completed,
    NoChange,
    LastChangeTooRecent,
    NoSessionCookie,
    InvalidSession,
    InvalidSessionIpMismatch,
    InvalidSessionAsnMismatch,
    InvalidSessionInvalidCookie,
    InvalidSessionOther,
    IncorrectSessionTypeNotAllowed,
    IncorrectSessionTypeNonApi,
    Other(String),
}

impl DynamicSeedboxMsg {
    fn from_msg(raw: &str) -> Self {
        // MAM's casing is inconsistent ("Completed" vs "No change" vs
        // "Last change too recent"); normalise to lowercase for matching.
        match raw.trim().to_ascii_lowercase().as_str() {
            "completed" => Self::Completed,
            "no change" => Self::NoChange,
            "last change too recent" => Self::LastChangeTooRecent,
            "no session cookie" => Self::NoSessionCookie,
            "invalid session" => Self::InvalidSession,
            "invalid session - ip mismatch" => Self::InvalidSessionIpMismatch,
            "invalid session - asn mismatch" => Self::InvalidSessionAsnMismatch,
            "invalid session - invalid cookie" => Self::InvalidSessionInvalidCookie,
            "invalid session - other" => Self::InvalidSessionOther,
            "incorrect session type - not allowed this function" => {
                Self::IncorrectSessionTypeNotAllowed
            }
            "incorrect session type - non-api session" => Self::IncorrectSessionTypeNonApi,
            _ => Self::Other(raw.to_string()),
        }
    }
}

/// §32: typed seedbox-update outcome.
///
/// Carries the registered IP/ASN/AS that MAM reports back on every call —
/// these are the source of truth for "what MAM has on file" and feed the
/// Mousehole-style dedup logic.  The shell forwards these fields to the
/// MAM core via the extended `Event::MamUpdateSuccess`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicSeedboxOutcome {
    pub msg: DynamicSeedboxMsg,
    pub success: bool,
    pub ip: Option<VpnIp>,
    pub asn: Option<u32>,
    pub as_org: Option<String>,
}

/// §32: typed result of `fetch_mam_ip()` — MAM's view of our current IP
/// and ASN, returned from `/json/jsonIp.php`.  Used by §31's verification
/// path to cross-check Gluetun's file against what MAM sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MamIpInfo {
    pub ip: VpnIp,
    pub asn: u32,
    pub as_org: String,
}

#[derive(Deserialize)]
struct JsonIpResponse {
    ip: String,
    #[serde(rename = "ASN", default)]
    asn: u32,
    #[serde(rename = "AS", default)]
    as_org: String,
}

/// §28: typed failure surface for `fetch_mam_status`.
///
/// Distinguishes the three retryable failure shapes:
///
/// - `Unreachable` — we could not reach a MAM endpoint at all (DNS, TCP
///   connect, TLS, request timeout).  Routed to `MamEvent::Unreachable` by
///   the shell, which the MAM core publishes as `MamPublish::Unreachable`
///   on the `Connectability` topic.  Distinct from `NotConnectable`, which
///   means MAM was reachable but reports our client is not connectable.
/// - `LocalRateLimit` — the operator's own 400 ms inter-request guard
///   triggered before the request was sent.  Distinct from MAM's
///   server-side rate limit (which would arrive as a non-`Ok` HTTP
///   response).  Mapped to `MamEvent::RateLimited` today.
/// - `StatusFailed` — MAM was reached but responded with an unrecognised
///   HTTP status, a non-`Success` body, or an unparseable response.
///   Mapped to `MamEvent::StatusFailed`.
///
/// Previously `fetch_mam_status` returned `Option<MamStatusResult>` and the
/// `None` branch was a lie: it collapsed network errors, parse failures, and
/// the local rate-limit guard into the same shape, so the shell could not
/// tell a DNS failure from a 429.  This enum is the honest surface.
#[derive(Debug, Clone)]
pub enum MamFetchError {
    Unreachable(String),
    LocalRateLimit,
    StatusFailed(String),
}

/// Typed result of a successful MAM status fetch.
///
/// **JSON field choices (§26):**
/// - `ratio`: the standard MAM `ratio` field (a JSON number, e.g. `2.5`).
///   Absent ⇒ defaults to `0.0` (fail-closed: the gate fires when the field
///   is missing, which is the correct behaviour per §26).
/// - `upload_credit_bytes`: MAM does not expose a dedicated upload-credit-buffer
///   field in the `/json/load.php` response.  The closest available proxy is
///   `seedbonus`, which is the site's "seed bonus" point balance.  This field
///   is not measured in bytes; it is used here as a bytes-equivalent proxy
///   because it is the only available upload-health signal in the response.
///   Operators who need a precise byte figure should update this mapping once
///   the correct MAM endpoint or field is identified.  Absent ⇒ defaults to
///   `0` (fail-closed).
#[derive(Debug, Clone)]
pub struct MamStatusResult {
    /// `true` iff MAM reports the seedbox as connectable.
    pub connectable: bool,
    /// Global upload ratio as reported by MAM (`ratio` JSON field).
    /// Defaults to `0.0` when the field is absent (fail-closed).
    pub ratio: f64,
    /// Upload-credit proxy: the `seedbonus` field from MAM's JSON response,
    /// interpreted as a bytes-equivalent for the upload-health gate (§26).
    /// Defaults to `0` when the field is absent (fail-closed).
    pub upload_credit_bytes: u64,
}

#[derive(Deserialize)]
struct JsonLoadResponse {
    connectable: Option<String>,
    #[serde(rename = "unsat")]
    unsat: Option<UnsatSummary>,
    /// MAM global upload ratio.  Absent ⇒ 0.0 (fail-closed for §26 gate).
    #[serde(default)]
    ratio: f64,
    /// MAM seed-bonus balance, used as upload-credit proxy (§26).
    /// Absent ⇒ 0 (fail-closed).
    #[serde(default)]
    seedbonus: f64,
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
    /// §32: `/json/jsonIp.php` endpoint MAM uses to report what *it* sees as
    /// our IP/ASN/AS.  Used by the VPN-core verification path as a second
    /// source alongside `ifconfig.co`.
    json_ip_url: String,
    torrent_base_url: String,
    last_request_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// §32: timestamp of the last successful (or attempted) call to the
    /// dynamic-seedbox endpoint.  Enforces MAM's documented 1-hour rolling
    /// limit on top of the existing 400 ms inter-request guard.
    last_seedbox_call_at: Arc<Mutex<Option<std::time::Instant>>>,
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
            // §32: switch to `?clientStats=` so MAM actually returns the
            // `connectable` field.  Without this our §28 NotConnectable
            // publish fires in steady state because the field is absent.
            // We accept the 30-min server-side cache as the trade-off.
            load_url: ensure_client_stats(load_url),
            json_ip_url: "https://t.myanonamouse.net/json/jsonIp.php".into(),
            torrent_base_url: "https://www.myanonamouse.net".into(),
            last_request_at: Arc::new(Mutex::new(None)),
            last_seedbox_call_at: Arc::new(Mutex::new(None)),
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

    /// §32: returns MAM's view of our current IP/ASN via `/json/jsonIp.php`.
    ///
    /// Unlike `fetch_mam_status`, this endpoint requires no session and is
    /// rate-limited at 1/minute server-side.  Used by the §31 verification
    /// path as a second source alongside `ifconfig.co/json` — the two
    /// together catch a wider class of routing/proxy edge cases.
    ///
    /// # Errors
    /// Returns `MamFetchError::Unreachable` on transport failure or
    /// `MamFetchError::StatusFailed` on a non-success HTTP response or a
    /// parse error.
    pub async fn fetch_mam_ip(&self) -> Result<MamIpInfo, MamFetchError> {
        if !self.check_rate_limit() {
            return Err(MamFetchError::LocalRateLimit);
        }
        let result = self.client.get(&self.json_ip_url).send().await;
        match result {
            Err(e) => Err(MamFetchError::Unreachable(e.to_string())),
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return Err(MamFetchError::StatusFailed(format!("HTTP {status}")));
                }
                let raw = resp.text().await.unwrap_or_default();
                self.emit_http(&self.json_ip_url, status.as_u16(), &raw);
                match serde_json::from_str::<JsonIpResponse>(&raw) {
                    Err(e) => Err(MamFetchError::StatusFailed(format!("parse: {e}"))),
                    Ok(body) => match body.ip.trim().parse::<std::net::Ipv4Addr>() {
                        Err(e) => Err(MamFetchError::StatusFailed(format!("ip parse: {e}"))),
                        Ok(ip) => Ok(MamIpInfo {
                            ip: VpnIp(ip),
                            asn: body.asn,
                            as_org: body.as_org,
                        }),
                    },
                }
            }
        }
    }

    /// Registers the current VPN IP with MAM via the dynamic seedbox endpoint.
    /// §36 step 9a: returns typed `MamSeedboxResult`.
    ///
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    pub async fn update_seedbox(&self) -> MamSeedboxResult {
        // §32: enforce MAM's documented 1-hour rolling rate limit
        // client-side.  Without this guard our retry/heartbeat paths can
        // hit the dynamic-seedbox endpoint repeatedly during normal
        // operation and accumulate `Last change too recent` rejections.
        if !self.check_seedbox_call_rate_limit() {
            warn!("MAM dynamic-seedbox 1h client-side rate limit triggered");
            return MamSeedboxResult::RateLimited;
        }
        if !self.check_rate_limit() {
            return MamSeedboxResult::RateLimited;
        }
        let current = self.session.lock().unwrap().clone();
        let (result, new_session) = self.do_update_seedbox(&current).await;
        if let Some(rotated) = new_session {
            *self.session.lock().unwrap() = rotated;
        }
        result
    }

    /// Fetches the MAM status and returns a typed result carrying connectivity,
    /// upload ratio, and upload-credit proxy (§26).
    ///
    /// §28: returns a typed `Result` distinguishing the three retryable
    /// failure shapes.  See [`MamFetchError`] for the meaning of each.
    ///
    /// # Errors
    /// - `MamFetchError::LocalRateLimit` — the operator's 400 ms inter-request
    ///   guard triggered.
    /// - `MamFetchError::Unreachable` — the request failed at the transport
    ///   layer (DNS, TCP connect, TLS, request timeout).
    /// - `MamFetchError::StatusFailed` — MAM responded but the response was
    ///   not a successful, well-formed status payload.
    ///
    /// # Panics
    /// Panics if the internal session mutex is poisoned.
    pub async fn fetch_mam_status(&self) -> Result<MamStatusResult, MamFetchError> {
        if !self.check_rate_limit() {
            return Err(MamFetchError::LocalRateLimit);
        }
        let current = self.session.lock().unwrap().clone();
        let (result, new_session) = self.do_fetch_mam_status(&current).await;
        if let Some(rotated) = new_session {
            *self.session.lock().unwrap() = rotated;
        }
        result
    }

    async fn do_fetch_mam_status(
        &self,
        session: &str,
    ) -> (Result<MamStatusResult, MamFetchError>, Option<String>) {
        let result = self
            .client
            .get(&self.load_url)
            .header(reqwest::header::COOKIE, format!("mam_id={session}"))
            .send()
            .await;
        let new_session = result.as_ref().ok().and_then(extract_mam_cookie);
        match result {
            Err(e) => {
                let reason = e.to_string();
                warn!("MAM status fetch request failed: {reason}");
                (Err(MamFetchError::Unreachable(reason)), new_session)
            }
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let code = status.as_u16();
                    warn!("MAM status fetch HTTP {status}");
                    self.emit_http(&self.load_url, code, "");
                    return (
                        Err(MamFetchError::StatusFailed(format!("HTTP {code}"))),
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
                        debug!(
                            "MAM fetch_mam_status: connectable={connectable} ratio={} seedbonus={}",
                            body.ratio, body.seedbonus
                        );
                        if let Some(ref unsat) = body.unsat {
                            debug!("MAM unsat: {}/{}", unsat.count, unsat.limit);
                        }
                        (
                            Ok(MamStatusResult {
                                connectable,
                                ratio: body.ratio,
                                // seedbonus is a non-negative floating-point point balance.
                                // We clamp to 0.0 before truncating to avoid sign-loss on
                                // pathological negative values, and use floor() so the cast
                                // is exact. The cast from a clamped, floored f64 to u64 is
                                // intentional (bytes-equivalent proxy per §26 docs).
                                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                                upload_credit_bytes: body.seedbonus.max(0.0).floor() as u64,
                            }),
                            new_session,
                        )
                    }
                    Err(e) => {
                        let reason = format!("parse: {e}");
                        warn!("MAM status fetch parse failed: {e}");
                        (Err(MamFetchError::StatusFailed(reason)), new_session)
                    }
                }
            }
        }
    }

    /// Returns `true` if the request can proceed (≥400ms since last request).
    /// Returns `false` if the guard triggers — caller surfaces the rate-
    /// limit hit via the typed result.
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

    /// §32: enforces the documented 1-hour rolling rate limit on
    /// `dynamicSeedbox.php` client-side.  Returns `true` if the call may
    /// proceed.  Increments the timestamp on a successful gate.
    fn check_seedbox_call_rate_limit(&self) -> bool {
        let mut last = self.last_seedbox_call_at.lock().unwrap();
        if let Some(t) = *last
            && t.elapsed() < std::time::Duration::from_hours(1)
        {
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

    async fn do_update_seedbox(&self, session: &str) -> (MamSeedboxResult, Option<String>) {
        let result = self
            .client
            .get(&self.seedbox_url)
            .header(reqwest::header::COOKIE, format!("mam_id={session}"))
            .send()
            .await;

        let new_session = result.as_ref().ok().and_then(extract_mam_cookie);

        match result {
            // §28: a transport-level failure means we did not reach MAM at
            // all.
            Err(e) => {
                let reason = e.to_string();
                warn!("MAM seedbox update request failed: {reason}");
                (MamSeedboxResult::Unreachable { reason }, new_session)
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                let raw = resp.text().await.unwrap_or_default();
                self.emit_http(&self.seedbox_url, status, &raw);
                match serde_json::from_str::<DynamicSeedboxResponse>(&raw) {
                    Err(e) => {
                        warn!("MAM seedbox response parse failed: {e}");
                        // Parse failure is treated as a generic Failed
                        // (pre-§36 the legacy event was an unhelpful
                        // success — the typed path now surfaces it
                        // honestly).
                        (
                            MamSeedboxResult::Failed {
                                reason: format!("parse error: {e}"),
                            },
                            new_session,
                        )
                    }
                    Ok(body) => {
                        let msg = DynamicSeedboxMsg::from_msg(&body.msg);
                        let registered_ip =
                            body.ip.trim().parse::<std::net::Ipv4Addr>().ok().map(VpnIp);
                        let registered_asn = (body.asn != 0).then_some(body.asn);
                        let registered_as = (!body.as_org.is_empty()).then(|| body.as_org.clone());
                        // §30: ASN mismatch is a distinct compliance signal.
                        if msg == DynamicSeedboxMsg::InvalidSessionAsnMismatch {
                            let ip =
                                registered_ip.unwrap_or(VpnIp(std::net::Ipv4Addr::UNSPECIFIED));
                            warn!("MAM ASN mismatch: ip={}", ip.0);
                            return (MamSeedboxResult::AsnMismatch { ip }, new_session);
                        }
                        if body.success {
                            info!(
                                "MAM seedbox {}: ip={:?} asn={:?} as={:?}",
                                body.msg, registered_ip, registered_asn, registered_as
                            );
                        } else {
                            warn!(
                                "MAM seedbox non-success {}: ip={:?} asn={:?}",
                                body.msg, registered_ip, registered_asn
                            );
                        }
                        // §32: regardless of `Success`, MAM returns the IP
                        // it currently has registered — carry it through so
                        // the MAM core can dedup against it.
                        (
                            MamSeedboxResult::Success {
                                registered_ip,
                                registered_asn,
                                registered_as,
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
            self.torrent_base_url,
            mam_id.into_inner()
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

/// §32: appends `?clientStats=` to a `/jsonLoad.php` URL if not already
/// present.  Per MAM's API docs the `connectable` field is only returned
/// when this query parameter is set; without it our §28 `NotConnectable`
/// publish fires in steady state because the field is absent.
fn ensure_client_stats(url: String) -> String {
    if url.contains("clientStats") {
        return url;
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}clientStats=")
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
        assert!(matches!(event, MamSeedboxResult::Success { .. }));
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
            matches!(&event, MamSeedboxResult::AsnMismatch { ip } if ip.0.to_string() == "79.127.184.201")
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
        assert!(matches!(event, MamSeedboxResult::Success { .. }));
    }

    // §36 step 9a: `check_connectability` deleted (was dead code); its
    // tests went with it.  Connectability is owned by `fetch_mam_status`
    // and surfaced through `MamFetchError`.

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
        assert!(matches!(event, MamSeedboxResult::RateLimited));
    }

    // ── do_update_seedbox error paths ─────────────────────────────────────────

    #[tokio::test]
    async fn update_seedbox_network_error_returns_unreachable() {
        // §28: a transport-level failure now surfaces as Event::MamUnreachable
        // (was Event::MamUpdateSuccess — the historical lie that masked DNS
        // and TLS failures as healthy operator state).
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
        assert!(matches!(event, MamSeedboxResult::Unreachable { .. }));
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
        assert!(matches!(event, MamSeedboxResult::Success { .. }));
    }

    #[tokio::test]
    async fn update_seedbox_unparseable_body_returns_failed() {
        // §36 step 9a: parse failure is now surfaced honestly as
        // `MamSeedboxResult::Failed` instead of the pre-§36 lie that
        // returned `Success` with empty registered fields.
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
        assert!(matches!(event, MamSeedboxResult::Failed { .. }));
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
        let result = mam
            .fetch_torrent(windlass_types::MamTorrentId::try_new(12345).unwrap())
            .await;
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
        let result = mam
            .fetch_torrent(windlass_types::MamTorrentId::try_new(99).unwrap())
            .await;
        assert!(result.is_none());
    }
}
