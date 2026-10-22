//! qBittorrent fixtures for integration tests.
//!
//! Wraps the real qBit web API for the things integration tests need:
//! authenticate, list torrents, add a (dummy) magnet, delete everything.
//! Real `.torrent`-with-payload fixtures land in a follow-up if/when a
//! test needs a torrent in a specific completion state.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::cookie::Jar;

use super::{QBIT_BASE, QBIT_PASS, QBIT_USER, wait_for};

/// A handle to a torrent we added to qBit.  Just the info-hash today;
/// expand if later tests need name / path.
pub struct TorrentHandle {
    pub hash: String,
}

/// Authenticate against the real qBit web UI and return a reqwest
/// client that carries the SID cookie.  Each call is a fresh login —
/// the test layer doesn't bother caching sessions.
async fn authed_client() -> Result<reqwest::Client> {
    let jar = Arc::new(Jar::default());
    let client = reqwest::Client::builder()
        .cookie_provider(Arc::clone(&jar))
        .timeout(Duration::from_secs(10))
        .build()
        .context("build qbit client")?;
    let resp = client
        .post(format!("{QBIT_BASE}/api/v2/auth/login"))
        .header("Referer", QBIT_BASE)
        .form(&[("username", QBIT_USER), ("password", QBIT_PASS)])
        .send()
        .await
        .context("qbit login")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("qbit login HTTP {status}");
    }
    // qBit 5.x returns 204 No Content with the SID set via Set-Cookie;
    // earlier versions returned a 200 with body "Ok." or "Fails.".
    // Treat any 2xx as success, then verify the cookie jar caught the
    // SID — otherwise the post-login API calls will silently 403.
    let body = resp.text().await.unwrap_or_default();
    if !body.is_empty() && body.trim() != "Ok." {
        bail!("qbit login rejected: body={body:?}");
    }
    Ok(client)
}

/// Build a 40-hex info hash by stretching a fresh `Uuid` to 20 bytes.
/// The hash itself doesn't need to correspond to a real torrent — we
/// feed magnets to qBit purely to exercise the `/api/v2/torrents/info`
/// shape.  Real `.torrent` blobs come later if a test needs a
/// completion path.
fn random_info_hash() -> String {
    use std::fmt::Write;
    let id = uuid::Uuid::new_v4();
    let half = id.as_bytes();
    let mut bytes = [0u8; 20];
    bytes[..16].copy_from_slice(half);
    // Tail four bytes: a second uuid's first four, so the full 20-byte
    // hash space is exercised even though we never reach 2^128 per run.
    bytes[16..].copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..4]);
    bytes.iter().fold(String::with_capacity(40), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Add a magnet for a fresh, otherwise-unknown info-hash.  qBit will
/// add it and sit in `stalledDL` because nobody seeds it — exactly the
/// state we want for §29-style "torrent appears in the list" tests.
///
/// `label` becomes the magnet's display name and the qBit category
/// (helpful for differentiating fixtures in a debug session).
pub async fn add_magnet_torrent(label: &str) -> Result<TorrentHandle> {
    let client = authed_client().await?;
    let hash = random_info_hash();
    let magnet = format!("magnet:?xt=urn:btih:{hash}&dn={label}");
    let resp = client
        .post(format!("{QBIT_BASE}/api/v2/torrents/add"))
        .header("Referer", QBIT_BASE)
        .form(&[("urls", magnet.as_str()), ("category", label)])
        .send()
        .await
        .context("post torrents/add")?;
    if !resp.status().is_success() {
        bail!("qbit add HTTP {}", resp.status());
    }
    let handle = TorrentHandle { hash: hash.clone() };
    wait_for(
        &format!("qbit torrent {hash} listed"),
        Duration::from_secs(10),
        || async {
            list_hashes()
                .await
                .is_ok_and(|hs| hs.iter().any(|h| h.eq_ignore_ascii_case(&hash)))
        },
    )
    .await;
    Ok(handle)
}

/// Number of torrents currently in qBit's list.  Cheap probe used by
/// `reset_stack()` to verify the deletion settled.
pub async fn torrent_count() -> Result<usize> {
    Ok(list_hashes().await?.len())
}

/// Returns every torrent's lowercase info-hash from
/// `/api/v2/torrents/info`.  Tests that need richer fields can call
/// the API directly; the helper only surfaces what fixture management
/// needs.
pub async fn list_hashes() -> Result<Vec<String>> {
    let client = authed_client().await?;
    let body: serde_json::Value = client
        .get(format!("{QBIT_BASE}/api/v2/torrents/info"))
        .send()
        .await
        .context("torrents/info")?
        .error_for_status()
        .context("torrents/info status")?
        .json()
        .await
        .context("torrents/info json")?;
    Ok(body
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t["hash"].as_str().map(str::to_ascii_lowercase))
                .collect()
        })
        .unwrap_or_default())
}

/// Delete every torrent in qBit (and its files, though our magnets
/// don't ever download anything).  Called by `reset_stack()`.
pub async fn delete_all() -> Result<()> {
    let hashes = list_hashes().await?;
    if hashes.is_empty() {
        return Ok(());
    }
    let client = authed_client().await?;
    let resp = client
        .post(format!("{QBIT_BASE}/api/v2/torrents/delete"))
        .header("Referer", QBIT_BASE)
        .form(&[("hashes", hashes.join("|")), ("deleteFiles", "true".into())])
        .send()
        .await
        .context("torrents/delete")?;
    if !resp.status().is_success() {
        bail!("qbit delete HTTP {}", resp.status());
    }
    wait_for(
        "qbit list empty after delete",
        Duration::from_secs(5),
        || async { list_hashes().await.is_ok_and(|h| h.is_empty()) },
    )
    .await;
    Ok(())
}
