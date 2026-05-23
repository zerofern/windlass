#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use chrono::Utc;
use serde::Deserialize;
use tracing::{debug, warn};

use windlass_core::HttpObserver;
use windlass_core::events::Event;
use windlass_types::{AuthCookie, HttpExchange, HttpStatusCode, TorrentName, VpnPort};

use super::types::{QbitPreferences, QbitTorrentDetails, QbitTorrentState};

#[derive(Deserialize)]
struct TorrentInfo {
    name: String,
}

/// Wraps a `reqwest::Client` together with the qBittorrent connection details.
/// All qBittorrent operations are methods so call sites only pass `&self`.
#[derive(Clone)]
pub struct QbitClient {
    pub(super) client: reqwest::Client,
    pub(super) base_url: String,
    user: String,
    pass: String,
    pub(super) on_http: HttpObserver,
}

impl QbitClient {
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        user: String,
        pass: String,
        on_http: HttpObserver,
    ) -> Self {
        Self {
            client,
            base_url,
            user,
            pass,
            on_http,
        }
    }

    pub(crate) fn emit_http(
        &self,
        method: &str,
        url: &str,
        request_body: Option<String>,
        response_status: u16,
        response_body: &str,
    ) {
        (self.on_http)(HttpExchange {
            module: "qbit".into(),
            method: method.into(),
            url: url.into(),
            request_body,
            response_status,
            response_body: response_body.into(),
        });
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
                Event::QbitConnectionRefused { at: Utc::now() }
            }
            Ok(resp) => {
                let status = resp.status();
                let sid = extract_sid_cookie(&resp);
                let body = resp.text().await.unwrap_or_default();
                self.emit_http("POST", &url, None, status.as_u16(), &body);

                if status.is_success() && (body.trim() == "Ok." || body.trim().is_empty()) {
                    let Some(cookie) = sid else {
                        warn!("qBit auth: ok status but no SID cookie in response");
                        return Event::QbitAuthFailed { at: Utc::now() };
                    };
                    debug!("qBit auth success");
                    return Event::QbitAuthSuccess {
                        at: Utc::now(),
                        cookie: AuthCookie(cookie),
                    };
                }
                if body.trim() == "Fails." {
                    warn!("qBit auth: credentials rejected (Fails.)");
                    return Event::QbitAuthFailed { at: Utc::now() };
                }
                warn!("qBit auth unexpected response: status={status}, body={body:?}");
                Event::QbitApiError {
                    at: Utc::now(),
                    code: HttpStatusCode(status.as_u16()),
                }
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
                Event::QbitPortSyncFailed {
                    at: Utc::now(),
                    code: HttpStatusCode(0),
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                self.emit_http("POST", &url, Some(req_body), status.as_u16(), &body);
                if status.is_success() {
                    debug!("qBit port sync success");
                    Event::QbitPortSyncSuccess { at: Utc::now() }
                } else {
                    warn!("qBit port sync failed: status={status}");
                    Event::QbitPortSyncFailed {
                        at: Utc::now(),
                        code: HttpStatusCode(status.as_u16()),
                    }
                }
            }
        }
    }

    /// Fetches the current list of torrent names from qBittorrent.
    ///
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

    /// Fetches full torrent details from qBittorrent.
    ///
    /// Returns an empty vec on any error — callers must not rely on error
    /// propagation; the compliance monitor will retry on the next poll.
    pub async fn list_torrent_details(&self, cookie: &AuthCookie) -> Vec<QbitTorrentDetails> {
        use super::types::{TorrentInfoWire, parse_mam_id};
        let url = format!("{}/api/v2/torrents/info", self.base_url);
        match self
            .client
            .get(&url)
            .header(reqwest::header::COOKIE, format!("SID={}", cookie.0))
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
        match self
            .client
            .get(&url)
            .header(reqwest::header::COOKIE, format!("SID={}", cookie.0))
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
                }),
                Err(e) => {
                    warn!("Failed to parse preferences: {e}");
                    None
                }
            },
        }
    }
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
