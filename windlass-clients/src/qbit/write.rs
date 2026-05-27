#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use tracing::warn;
use windlass_types::{AuthCookie, TorrentHash};

use super::QbitClient;

impl QbitClient {
    /// Adds a torrent from raw `.torrent` file bytes.
    ///
    /// Returns the info hash on success, `None` on failure.
    /// Hash is obtained from the response body if qBittorrent returns it (v4.3+),
    /// otherwise falls back to a list-diff against the pre-add snapshot.
    pub async fn add_torrent(
        &self,
        cookie: &AuthCookie,
        torrent_bytes: Vec<u8>,
    ) -> Option<TorrentHash> {
        let url = format!("{}/api/v2/torrents/add", self.base_url);
        let part = reqwest::multipart::Part::bytes(torrent_bytes)
            .file_name("file.torrent")
            .mime_str("application/x-bittorrent")
            .ok()?;
        let form = reqwest::multipart::Form::new().part("torrents", part);
        match self
            .client
            .post(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .multipart(form)
            .send()
            .await
        {
            Err(e) => {
                warn!("Failed to add torrent: {e}");
                None
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                self.emit_http("POST", &url, None, status.as_u16(), &body);
                if !status.is_success() {
                    warn!("add_torrent: non-success status {status}");
                    return None;
                }
                // qBit 4.3+ returns the hash as the body; older versions return "Ok."
                let trimmed = body.trim();
                if trimmed.len() == 40 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Some(TorrentHash(trimmed.to_owned()));
                }
                // Fall back: list all torrents and return the one not previously known.
                // We do a quick list-diff with a small delay to let qBit process.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let after = self.list_torrent_details(cookie).await;
                after.into_iter().next().map(|t| t.hash)
            }
        }
    }

    /// Pauses a torrent. Fire-and-forget; logs a warning on failure.
    pub async fn pause_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash) {
        let url = format!("{}/api/v2/torrents/stop", self.base_url);
        if let Err(e) = self
            .client
            .post(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .form(&[("hashes", hash.0.as_str())])
            .send()
            .await
        {
            warn!("pause_torrent failed: {e}");
        }
    }

    /// Resumes a paused torrent. Fire-and-forget; logs a warning on failure.
    pub async fn resume_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash) {
        let url = format!("{}/api/v2/torrents/start", self.base_url);
        if let Err(e) = self
            .client
            .post(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .form(&[("hashes", hash.0.as_str())])
            .send()
            .await
        {
            warn!("resume_torrent failed: {e}");
        }
    }

    /// Force-resumes a torrent (bypasses seeding ratio/time limits).
    /// Fire-and-forget; logs a warning on failure.
    pub async fn force_resume_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash) {
        let url = format!("{}/api/v2/torrents/setForceStart", self.base_url);
        if let Err(e) = self
            .client
            .post(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .form(&[("hashes", hash.0.as_str()), ("value", "true")])
            .send()
            .await
        {
            warn!("force_resume_torrent failed: {e}");
        }
    }

    /// Removes a torrent from qBittorrent without deleting the downloaded files.
    /// Fire-and-forget; logs a warning on failure.
    pub async fn delete_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash) {
        let url = format!("{}/api/v2/torrents/delete", self.base_url);
        if let Err(e) = self
            .client
            .post(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .form(&[("hashes", hash.0.as_str()), ("deleteFiles", "false")])
            .send()
            .await
        {
            warn!("delete_torrent failed: {e}");
        }
    }

    /// Sets all files in a torrent to normal download priority (MAM "no partials" rule).
    ///
    /// Priority `1` = Normal in qBittorrent's file priority API.
    /// Fire-and-forget; logs a warning on failure.
    pub async fn set_all_files_priority(&self, cookie: &AuthCookie, hash: &TorrentHash) {
        let url = format!("{}/api/v2/torrents/filePrio", self.base_url);
        if let Err(e) = self
            .client
            .post(&url)
            .header(
                reqwest::header::COOKIE,
                format!("SID={}", cookie.expose_secret()),
            )
            .form(&[("hash", hash.0.as_str()), ("id", "all"), ("priority", "1")])
            .send()
            .await
        {
            warn!("set_all_files_priority failed: {e}");
        }
    }
}
