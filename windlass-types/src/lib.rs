#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use nutype::nutype;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

/// §36 step 9a: type-erased HTTP-observation callback used to feed the
/// debug exchange channel.  Moved here from `windlass_core::observation`
/// so the qBit + MAM clients don't need to depend on the legacy core
/// crate.
pub type HttpObserver = Arc<dyn Fn(HttpExchange) + Send + Sync>;
pub use uom::si::f64::Information;

// ── IPs ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnIp(pub Ipv4Addr);

// ── Ports ────────────────────────────────────────────────────────────────────

#[nutype(
    validate(greater = 0, less_or_equal = 65535),
    derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)
)]
pub struct VpnPort(u16);

// ── HTTP ─────────────────────────────────────────────────────────────────────

/// An HTTP status code returned by an external service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpStatusCode(pub u16);

/// A single HTTP request/response pair captured from a client call.
/// Stored per-action in `ActionEntry.http_exchanges` when debug mode is on.
#[derive(Debug, Clone, Serialize)]
pub struct HttpExchange {
    /// Which client emitted this: `"qbit"` or `"mam"`.
    pub module: String,
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    pub response_status: u16,
    pub response_body: String,
}

// ── Torrents ─────────────────────────────────────────────────────────────────

/// The display name of a torrent as reported by qBittorrent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TorrentName(pub String);

// ── Auth ─────────────────────────────────────────────────────────────────────

/// The SID cookie returned by qBittorrent on successful login.
/// Always serializes as `"[redacted]"` to prevent leaking credentials.
#[derive(Debug, Clone)]
pub struct AuthCookie(SecretString);

impl AuthCookie {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(SecretString::new(value.into()))
    }

    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl serde::Serialize for AuthCookie {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("[redacted]")
    }
}

impl PartialEq for AuthCookie {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret() == other.expose_secret()
    }
}

impl Eq for AuthCookie {}

impl<'de> serde::Deserialize<'de> for AuthCookie {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let value = String::deserialize(d)?;
        if value == "[redacted]" || value.is_empty() {
            return Err(D::Error::custom(
                "redacted or empty auth cookie is not usable",
            ));
        }
        Ok(Self::new(value))
    }
}

// ── Container identity ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContainerId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContainerName(pub String);

// ── Secrets ──────────────────────────────────────────────────────────────────

pub struct MamSessionId(pub SecretString);
pub struct QbitPassword(pub SecretString);

// ── Retry / recovery counts ───────────────────────────────────────────────────

/// A count of retry attempts or recovery cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RetryCount(pub u8);

impl RetryCount {
    #[must_use]
    pub const fn increment(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

// ── Typed durations ───────────────────────────────────────────────────────────

/// A recurring scheduled timer interval (e.g. heartbeat, disk check).
#[derive(Debug, Clone, Copy)]
pub struct Interval(pub Duration);

impl From<Interval> for Duration {
    fn from(i: Interval) -> Self {
        i.0
    }
}

/// A one-shot retry backoff delay.
#[derive(Debug, Clone, Copy)]
pub struct Backoff(pub Duration);

impl Backoff {
    /// Returns `self * 2^attempt` — exponential backoff with this as the base.
    #[must_use]
    pub fn exponential(self, attempt: RetryCount) -> Duration {
        self.0
            .saturating_mul(2u32.saturating_pow(u32::from(attempt.0)))
    }
}

impl From<Backoff> for Duration {
    fn from(b: Backoff) -> Self {
        b.0
    }
}

// ── Wakeup IDs ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WakeupId {
    Heartbeat,
    DiskCheck,
    TorrentCheck,
    QbitAuthRetry,
    QbitSyncRetry,
    RetryPortRead,
    CompliancePoll,
    DomainSnapshot,
}

// ── Alert priority ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertPriority {
    Info,
    Warning,
    Critical,
}

// ── MAM / Torrent IDs ────────────────────────────────────────────────────────

/// The info hash of a torrent as reported by qBittorrent (40-char hex string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TorrentHash(pub String);

// ── Torrent tracking ──────────────────────────────────────────────────────────

/// qBittorrent torrent state as reported by the API.
///
/// Mirrors `QbitTorrentState` from `windlass-clients` but lives in `windlass-types`
/// so it can be stored in the qBit core without a dependency on the clients crate.
/// The shell converts between the two at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TorrentState {
    Downloading,
    StalledDownloading,
    Uploading,
    StalledUploading,
    ForcedUpload,
    PausedDownloading,
    PausedUploading,
    Error,
    Other(String),
}

impl TorrentState {
    /// Returns `true` when the torrent is counted as active by qBittorrent's
    /// queue limit (`max_active_torrents`).
    ///
    /// Active states: `Downloading`, `Uploading`, `ForcedUpload`,
    /// `StalledDownloading`, `StalledUploading`.
    /// Inactive states: `PausedDownloading`, `PausedUploading`, `Error`, `Other`.
    ///
    /// Mirrors `windlass_core::torrent::TorrentState::is_active` (legacy reference).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Downloading
                | Self::Uploading
                | Self::ForcedUpload
                | Self::StalledDownloading
                | Self::StalledUploading
        )
    }
}

/// Per-torrent record stored in the qBit core.
///
/// Sourced from qBittorrent torrent listings and used by the `HnR` seed-time lock
/// and future compliance stories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentRecord {
    pub hash: TorrentHash,
    pub downloaded_bytes: u64,
    pub seed_time: Duration,
    pub state: TorrentState,
    pub mam_id: Option<MamTorrentId>,
    /// §36 step 6: human-readable torrent name from qBittorrent.  Used
    /// by the Torrent Monitor UI and persisted into the `torrents` DB
    /// table.  May be empty when the source bridge doesn't carry it
    /// (e.g. legacy `Event::QbitTorrentDetailsReceived` translation).
    pub name: TorrentName,
    /// §36 step 6: timestamp of the qBittorrent observation that
    /// produced this record.  Persisted into the `torrents` table for
    /// freshness-based UI sorting.
    pub seen_at: chrono::DateTime<chrono::Utc>,
}

/// A MAM torrent ID parsed from the torrent's comment field.
/// Comment URL format: `https://www.myanonamouse.net/t/12345`
#[nutype(
    validate(greater = 0),
    derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)
)]
pub struct MamTorrentId(u64);

impl MamTorrentId {
    /// Parses a MAM torrent ID from a URL (`https://www.myanonamouse.net/t/12345`)
    /// or a plain numeric string (`"12345"`). Returns `None` for invalid input or zero.
    #[must_use]
    pub fn from_url_or_id(s: &str) -> Option<Self> {
        let s = s.trim();
        // Try plain numeric first.
        if let Ok(id) = s.parse::<u64>() {
            return Self::try_new(id).ok();
        }
        // Try URL: strip scheme + host, then accept /t/ or /tor/ prefixes.
        let path = s
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.myanonamouse.net");
        let rest = path
            .strip_prefix("/t/")
            .or_else(|| path.strip_prefix("/tor/"))?;
        let id = rest.split('/').next()?.parse::<u64>().ok()?;
        Self::try_new(id).ok()
    }
}

// ── MAM connectivity ─────────────────────────────────────────────────────────

/// The result of a MAM connectivity heartbeat check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamStatus {
    /// MAM reached and qBit is listed as connectable (accepts incoming connections).
    Connectable,
    /// MAM reached but qBit is not connectable — port forward or firewall issue.
    NotConnectable,
    /// Network failure, HTTP error, or parse failure reaching MAM.
    Unreachable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn retry_count_increment_adds_one() {
        let r = RetryCount(3);
        assert_eq!(r.increment(), RetryCount(4));
    }

    #[test]
    fn retry_count_increment_saturates_at_u8_max() {
        let r = RetryCount(u8::MAX);
        assert_eq!(r.increment(), RetryCount(u8::MAX));
    }

    #[test]
    fn backoff_exponential_attempt_zero_returns_base() {
        let b = Backoff(Duration::from_secs(1));
        assert_eq!(b.exponential(RetryCount(0)), Duration::from_secs(1));
    }

    #[test]
    fn backoff_exponential_doubles_each_attempt() {
        let b = Backoff(Duration::from_secs(1));
        assert_eq!(b.exponential(RetryCount(1)), Duration::from_secs(2));
        assert_eq!(b.exponential(RetryCount(2)), Duration::from_secs(4));
        assert_eq!(b.exponential(RetryCount(3)), Duration::from_secs(8));
    }

    #[test]
    fn interval_into_duration() {
        let d = Duration::from_secs(60);
        let i = Interval(d);
        assert_eq!(Duration::from(i), d);
    }

    #[test]
    fn backoff_into_duration() {
        let d = Duration::from_secs(5);
        let b = Backoff(d);
        assert_eq!(Duration::from(b), d);
    }

    #[test]
    fn auth_cookie_serializes_as_redacted() {
        let c = AuthCookie::new("my-secret-cookie".to_string());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#""[redacted]""#);
    }

    #[test]
    fn auth_cookie_deserializes_from_string() {
        let c: AuthCookie = serde_json::from_str(r#""some-value""#).unwrap();
        assert_eq!(c.expose_secret(), "some-value");
    }

    #[test]
    fn mam_torrent_id_from_numeric_string() {
        assert_eq!(
            MamTorrentId::from_url_or_id("12345"),
            MamTorrentId::try_new(12345).ok()
        );
    }

    #[test]
    fn mam_torrent_id_from_t_url() {
        let url = "https://www.myanonamouse.net/t/12345";
        assert_eq!(
            MamTorrentId::from_url_or_id(url),
            MamTorrentId::try_new(12345).ok()
        );
    }

    #[test]
    fn mam_torrent_id_from_tor_url() {
        let url = "https://www.myanonamouse.net/tor/12345";
        assert_eq!(
            MamTorrentId::from_url_or_id(url),
            MamTorrentId::try_new(12345).ok()
        );
    }

    #[test]
    fn mam_torrent_id_rejects_zero() {
        assert_eq!(MamTorrentId::from_url_or_id("0"), None);
    }

    #[test]
    fn mam_torrent_id_rejects_non_mam_url() {
        assert_eq!(
            MamTorrentId::from_url_or_id("https://example.com/t/12345"),
            None
        );
    }

    #[test]
    fn mam_torrent_id_rejects_empty_string() {
        assert_eq!(MamTorrentId::from_url_or_id(""), None);
    }
}
