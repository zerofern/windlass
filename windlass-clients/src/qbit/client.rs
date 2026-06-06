#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::sync::Arc;

use tracing::{debug, warn};

use windlass_types::{
    AuthCookie, CoreId, HttpExchange, HttpRequestView, HttpTap, QbitPassword, VpnPort,
};

use super::types::{
    QbitAuthResult, QbitPortSyncResult, QbitPreferences, QbitTorrentDetails, QbitTorrentState,
};

pub struct HttpCapture<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub request_headers: Vec<(String, String)>,
    pub request_body: Option<String>,
    pub response_status: u16,
    pub response_headers: Vec<(String, String)>,
    pub response_body: &'a str,
}

/// Wraps a `reqwest::Client` together with the qBittorrent connection details.
/// All qBittorrent operations are methods so call sites only pass `&self`.
///
/// The password is held as a [`QbitPassword`] so the type's `Debug` impl
/// emits `[REDACTED]` and the cleartext only reaches the auth POST via an
/// explicit `expose_secret()` call.
#[derive(Clone)]
pub struct QbitClient {
    pub(super) client: reqwest::Client,
    pub(super) base_url: String,
    user: String,
    pass: QbitPassword,
    pub(super) hook: Arc<dyn HttpTap>,
}

impl QbitClient {
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        user: String,
        pass: QbitPassword,
        hook: Arc<dyn HttpTap>,
    ) -> Self {
        Self {
            client,
            base_url,
            user,
            pass,
            hook,
        }
    }

    pub(crate) fn emit_http(&self, capture: HttpCapture<'_>) {
        self.hook.observed_exchange(
            CoreId::Qbit,
            &HttpExchange {
                module: "qbit".into(),
                method: capture.method.into(),
                url: capture.url.into(),
                request_headers: capture.request_headers,
                request_body: capture.request_body,
                response_status: capture.response_status,
                response_headers: capture.response_headers,
                response_body: capture.response_body.into(),
            },
        );
    }

    /// Build a `Cookie: SID=…` header pair for a cookie-bearing
    /// request.  The observability redactor (Decision 14) recognises
    /// `Cookie` as a secret-bearing header name and wraps the value in
    /// a `ServerSecretSlot` at capture time.
    pub(crate) fn cookie_header(cookie: &AuthCookie) -> (String, String) {
        (
            "Cookie".to_string(),
            format!("SID={}", cookie.expose_secret()),
        )
    }

    /// §37e: per-send-site `gate_request` helper.  Parks on qBit's
    /// pause flag when set; returns immediately otherwise.
    pub(crate) async fn gate_request(&self, method: &str, url: &str) {
        self.gate_request_with_body(method, url, None).await;
    }

    /// Variant that carries the request body.  Used by POST sites
    /// (auth, port sync, file-priority, torrent add/pause/resume/etc.)
    /// so the operator can see what's about to be sent while parked
    /// at `ParkedAtHttp`.
    pub(crate) async fn gate_request_with_body(
        &self,
        method: &str,
        url: &str,
        body: Option<&serde_json::Value>,
    ) {
        self.hook
            .gate_request(CoreId::Qbit, &HttpRequestView { method, url, body })
            .await;
    }

    /// Authenticates with qBittorrent.  §36 step 9a: returns a typed
    /// `QbitAuthResult` so the shell can map to `QbitEvent::AuthSucceeded /
    /// AuthFailed / AuthRejected` without depending on legacy core types.
    pub async fn authenticate(&self) -> QbitAuthResult {
        let url = format!("{}/api/v2/auth/login", self.base_url);
        let body = serde_json::json!({
            "username": self.user.as_str(),
            // Cleartext password — wrapped by ServerSecretSlot at capture
            // time so it serializes to WireRedacted on the wire.
            "password": self.pass.expose_secret(),
        });
        self.gate_request_with_body("POST", &url, Some(&body)).await;
        match self
            .client
            .post(&url)
            .form(&[
                ("username", self.user.as_str()),
                ("password", self.pass.expose_secret()),
            ])
            .send()
            .await
        {
            Err(e) => {
                // Connection refused is normal during container startup —
                // shell retries silently without alerting.
                debug!("qBit auth request failed (connection): {e}");
                QbitAuthResult::ConnectionRefused
            }
            Ok(resp) => {
                let status = resp.status();
                let sid = extract_sid_cookie(&resp);
                let response_headers = response_headers(&resp);
                let body = resp.text().await.unwrap_or_default();
                self.emit_http(HttpCapture {
                    method: "POST",
                    url: &url,
                    request_headers: Vec::new(),
                    request_body: None,
                    response_status: status.as_u16(),
                    response_headers,
                    response_body: &body,
                });

                if status.is_success() && (body.trim() == "Ok." || body.trim().is_empty()) {
                    let Some(cookie) = sid else {
                        warn!("qBit auth: ok status but no SID cookie in response");
                        return QbitAuthResult::Rejected;
                    };
                    debug!("qBit auth success");
                    return QbitAuthResult::Success(AuthCookie::new(cookie));
                }
                if body.trim() == "Fails." {
                    warn!("qBit auth: credentials rejected (Fails.)");
                    return QbitAuthResult::Rejected;
                }
                warn!("qBit auth unexpected response: status={status}, body={body:?}");
                QbitAuthResult::ApiError(status.as_u16())
            }
        }
    }

    /// Updates qBittorrent's listen port via the preferences API.
    /// §36 step 9a: returns typed `QbitPortSyncResult`.
    pub async fn sync_port(&self, cookie: &AuthCookie, port: VpnPort) -> QbitPortSyncResult {
        let url = format!("{}/api/v2/app/setPreferences", self.base_url);
        let req_body = format!(r#"{{"listen_port":"{}"}}"#, port.into_inner());
        let body_view = serde_json::json!({ "json": &req_body });
        self.gate_request_with_body("POST", &url, Some(&body_view))
            .await;
        match self
            .client
            .post(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .form(&[("json", &req_body)])
            .send()
            .await
        {
            Err(e) => {
                warn!("qBit port sync request failed: {e}");
                QbitPortSyncResult::Failed(0)
            }
            Ok(resp) => {
                let status = resp.status();
                let response_headers = response_headers(&resp);
                let body = resp.text().await.unwrap_or_default();
                self.emit_http(HttpCapture {
                    method: "POST",
                    url: &url,
                    request_headers: vec![Self::cookie_header(cookie)],
                    request_body: Some(req_body),
                    response_status: status.as_u16(),
                    response_headers,
                    response_body: &body,
                });
                if status.is_success() {
                    debug!("qBit port sync success");
                    QbitPortSyncResult::Success
                } else {
                    warn!("qBit port sync failed: status={status}");
                    QbitPortSyncResult::Failed(status.as_u16())
                }
            }
        }
    }

    /// Fetches full torrent details from qBittorrent.
    ///
    /// Returns an empty vec on any error — callers must not rely on error
    /// propagation; the compliance monitor will retry on the next poll.
    pub async fn list_torrent_details(&self, cookie: &AuthCookie) -> Vec<QbitTorrentDetails> {
        use super::types::{TorrentInfoWire, parse_mam_id};
        let url = format!("{}/api/v2/torrents/info", self.base_url);
        self.gate_request("GET", &url).await;
        match self
            .client
            .get(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .send()
            .await
        {
            Err(e) => {
                warn!("Failed to list torrent details: {e}");
                vec![]
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                match resp.json::<Vec<TorrentInfoWire>>().await {
                    Ok(wires) => wires
                        .into_iter()
                        .map(|w| {
                            let mam_id = parse_mam_id(&w.comment);
                            QbitTorrentDetails {
                                hash: windlass_types::TorrentHash(w.hash),
                                name: windlass_types::TorrentName(w.name),
                                state: QbitTorrentState::from(w.state.as_str()),
                                seeding_time_secs: w.seeding_time,
                                downloaded_bytes: w.downloaded,
                                mam_id,
                            }
                        })
                        .collect(),
                    Err(e) => {
                        warn!("Failed to parse torrent details (status={status}): {e}");
                        vec![]
                    }
                }
            }
        }
    }

    /// Fetches qBittorrent application preferences.
    ///
    /// Returns `None` on any error.
    pub async fn get_preferences(&self, cookie: &AuthCookie) -> Option<QbitPreferences> {
        use super::types::PreferencesWire;
        let url = format!("{}/api/v2/app/preferences", self.base_url);
        self.gate_request("GET", &url).await;
        match self
            .client
            .get(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .send()
            .await
        {
            Err(e) => {
                warn!("Failed to fetch preferences: {e}");
                None
            }
            Ok(resp) => match resp.json::<PreferencesWire>().await {
                Ok(w) => Some(QbitPreferences {
                    torrents: u32::try_from(w.max_active_torrents).unwrap_or(5),
                    downloads: u32::try_from(w.max_active_downloads).unwrap_or(3),
                    uploads: u32::try_from(w.max_active_uploads).unwrap_or(3),
                    listen_port: w
                        .listen_port
                        .and_then(|port| u16::try_from(port).ok())
                        .and_then(|port| windlass_types::VpnPort::try_new(port).ok()),
                    dht: w.dht,
                    pex: w.pex,
                    lsd: w.lsd,
                    // A negative value in qBittorrent means "unlimited"; map to
                    // u32::MAX so downstream code can use a simple >= comparison.
                    max_active_torrents: u32::try_from(w.max_active_torrents).unwrap_or(u32::MAX),
                }),
                Err(e) => {
                    warn!("Failed to parse preferences: {e}");
                    None
                }
            },
        }
    }
}

/// Collect every response header into `(name, value)` pairs for
/// observability capture.  Non-UTF-8 values are replaced with a
/// placeholder so capture never panics.  The observability redactor
/// flips `Set-Cookie` (and a small fixed list) to `ServerSecretSlot`
/// at capture time.
pub fn response_headers(resp: &reqwest::Response) -> Vec<(String, String)> {
    resp.headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or("<non-utf8>").to_string(),
            )
        })
        .collect()
}

fn extract_sid_cookie(resp: &reqwest::Response) -> Option<String> {
    for value in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = value.to_str() {
            for part in s.split(';') {
                let part = part.trim();
                if let Some(sid) = part.strip_prefix("SID=") {
                    return Some(sid.to_string());
                }
                if let Some((name, sid)) = part.split_once('=')
                    && name.starts_with("QBT_SID")
                {
                    return Some(sid.to_string());
                }
            }
        }
    }
    None
}
