#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use nutype::nutype;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::time::Duration;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCookie(pub String);

impl serde::Serialize for AuthCookie {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("[redacted]")
    }
}

impl<'de> serde::Deserialize<'de> for AuthCookie {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self(String::deserialize(d)?))
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
        self.0 * 2u32.pow(u32::from(attempt.0))
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

/// A MAM torrent ID parsed from the torrent's comment field.
/// Comment URL format: `https://www.myanonamouse.net/t/12345`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MamTorrentId(pub u64);

impl MamTorrentId {
    /// Parses a MAM torrent ID from a URL (`https://www.myanonamouse.net/t/12345`)
    /// or a plain numeric string (`"12345"`). Returns `None` for invalid input or zero.
    #[must_use]
    pub fn from_url_or_id(s: &str) -> Option<Self> {
        let s = s.trim();
        // Try plain numeric first.
        if let Ok(id) = s.parse::<u64>() {
            return if id > 0 { Some(Self(id)) } else { None };
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
        if id > 0 { Some(Self(id)) } else { None }
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
        let c = AuthCookie("my-secret-cookie".to_string());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#""[redacted]""#);
    }

    #[test]
    fn auth_cookie_deserializes_from_string() {
        let c: AuthCookie = serde_json::from_str(r#""some-value""#).unwrap();
        assert_eq!(c.0, "some-value");
    }

    #[test]
    fn mam_torrent_id_from_numeric_string() {
        assert_eq!(
            MamTorrentId::from_url_or_id("12345"),
            Some(MamTorrentId(12345))
        );
    }

    #[test]
    fn mam_torrent_id_from_t_url() {
        let url = "https://www.myanonamouse.net/t/12345";
        assert_eq!(MamTorrentId::from_url_or_id(url), Some(MamTorrentId(12345)));
    }

    #[test]
    fn mam_torrent_id_from_tor_url() {
        let url = "https://www.myanonamouse.net/tor/12345";
        assert_eq!(MamTorrentId::from_url_or_id(url), Some(MamTorrentId(12345)));
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
