#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use async_trait::async_trait;
use nutype::nutype;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

pub use uom::si::f64::Information;

// ── CoreId ────────────────────────────────────────────────────────────────────

/// Identifies a per-system service runtime.  Lives here so HTTP clients
/// (which don't depend on `windlass-machine`) can tag their tap calls
/// with the owning core.  `windlass-machine::tap` re-exports this so
/// runtime-side code can keep using `windlass_machine::CoreId`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoreId {
    Vpn,
    Qbit,
    Mam,
    Db,
    Disk,
    Docker,
    Domain,
}

impl CoreId {
    /// All seven cores in cores-rail display order.
    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::Vpn,
            Self::Qbit,
            Self::Mam,
            Self::Db,
            Self::Disk,
            Self::Docker,
            Self::Domain,
        ]
    }
}

impl std::fmt::Display for CoreId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Vpn => "vpn",
            Self::Qbit => "qbit",
            Self::Mam => "mam",
            Self::Db => "db",
            Self::Disk => "disk",
            Self::Docker => "docker",
            Self::Domain => "domain",
        };
        f.write_str(name)
    }
}

// ── HttpTap ───────────────────────────────────────────────────────────────────

/// Borrowed view of an HTTP request, handed to [`HttpTap::gate_request`]
/// just before `client.execute(req)` is called.  Constructed from the
/// typed inputs the client used to build the request — never from the
/// built `reqwest::Request` (whose body is not always inspectable).
/// See `docs/observability-redesign.md` "HTTP request capture rule".
pub struct HttpRequestView<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub body: Option<&'a serde_json::Value>,
}

/// Anomalies a client may signal to the tap.  The live tap impl
/// translates these into per-core pause flips so the *next*
/// [`HttpTap::gate_request`] call parks the offending request before
/// it is sent.
#[derive(Debug, Clone)]
pub enum HttpAnomaly {
    /// The client detected a rate-limit violation would occur if it
    /// proceeded.  Wires the MAM rate-limit guardrail through to a
    /// per-core pause.
    RateLimitViolation { reason: String },
}

/// Observability hook for HTTP clients.  See
/// `docs/observability-redesign.md` "Architecture / `HttpTap`".
///
/// Implementations:
/// - [`NullHttpTap`]: every method is a no-op.  Use when observability
///   isn't attached.
/// - `windlass_observability::ObservabilityController`: the live impl.
///   `gate_request` parks on the per-core pause flag;
///   `signal_anomaly` flips the flag so the *current* request parks
///   when `gate_request` is awaited.
#[async_trait]
pub trait HttpTap: Send + Sync {
    /// Park until released.  Returns immediately when this core's
    /// pause flag is not set.  Called between building the request
    /// and `client.execute(req)`.
    async fn gate_request(&self, core: CoreId, view: &HttpRequestView<'_>);

    /// Fire-and-forget post-response capture.  Must not block.
    fn observed_exchange(&self, core: CoreId, exchange: &HttpExchange);

    /// Flag an anomaly the client detected.  The live tap impl uses
    /// this to flip the per-core pause flag *before* the next
    /// `gate_request` is awaited, so the offending request parks
    /// rather than being sent.
    fn signal_anomaly(&self, core: CoreId, anomaly: HttpAnomaly);
}

/// No-op `HttpTap` used when observability is not attached.
pub struct NullHttpTap;

#[async_trait]
impl HttpTap for NullHttpTap {
    async fn gate_request(&self, _core: CoreId, _view: &HttpRequestView<'_>) {}
    fn observed_exchange(&self, _core: CoreId, _exchange: &HttpExchange) {}
    fn signal_anomaly(&self, _core: CoreId, _anomaly: HttpAnomaly) {}
}

impl NullHttpTap {
    /// `Arc<dyn HttpTap>` slot for client constructors that take a tap.
    #[must_use]
    pub fn arc() -> Arc<dyn HttpTap> {
        Arc::new(Self)
    }
}

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
/// Carries raw header pairs; the observability controller decides which
/// header values are secret-bearing (Authorization / Cookie / Set-Cookie)
/// at capture time and wraps those in `ServerSecretSlot`.
#[derive(Debug, Clone, Serialize)]
pub struct HttpExchange {
    /// Which client emitted this: `"qbit"` or `"mam"`.
    pub module: String,
    pub method: String,
    pub url: String,
    /// Request headers as `(name, value)` pairs in send order.  The
    /// controller decides at capture time which values get redacted
    /// (Decision 14 — known classes redacted at capture without
    /// case-by-case opt-in).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_headers: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    pub response_status: u16,
    /// Response headers in received order.  Same redaction rules as
    /// request headers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_headers: Vec<(String, String)>,
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

/// Operator's MAM session cookie (`mam_id` value).
///
/// Wraps `SecretString` so `Debug` formats as `[REDACTED]` and the type
/// carries no default `Serialize` impl (cleartext never reaches a generic
/// JSON encoder). Call `expose_secret()` at the HTTP boundary where the
/// raw value is actually needed.
#[derive(Debug, Clone)]
pub struct MamSessionId(SecretString);

impl MamSessionId {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(SecretString::from(value))
    }

    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

/// qBittorrent admin password. Same redaction discipline as
/// [`MamSessionId`]: `Debug` emits `[REDACTED]`, no default `Serialize`
/// impl, cleartext only available via `expose_secret()`.
#[derive(Debug, Clone)]
pub struct QbitPassword(SecretString);

impl QbitPassword {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(SecretString::from(value))
    }

    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

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
    fn auth_cookie_debug_does_not_leak_cleartext() {
        let c = AuthCookie::new("super-sekrit".to_string());
        let dbg = format!("{c:?}");
        assert!(
            !dbg.contains("super-sekrit"),
            "Debug leaked cleartext: {dbg}"
        );
    }

    #[test]
    fn mam_session_id_debug_does_not_leak_cleartext() {
        let id = MamSessionId::new("very-secret-session".to_string());
        let dbg = format!("{id:?}");
        assert!(
            !dbg.contains("very-secret-session"),
            "Debug leaked cleartext: {dbg}"
        );
        assert!(dbg.contains("REDACTED"), "expected REDACTED marker: {dbg}");
    }

    #[test]
    fn qbit_password_debug_does_not_leak_cleartext() {
        let p = QbitPassword::new("hunter2".to_string());
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("hunter2"), "Debug leaked cleartext: {dbg}");
        assert!(dbg.contains("REDACTED"), "expected REDACTED marker: {dbg}");
    }

    #[test]
    fn mam_session_id_exposes_cleartext_only_via_expose_secret() {
        let id = MamSessionId::new("session-xyz".to_string());
        assert_eq!(id.expose_secret(), "session-xyz");
    }

    #[test]
    fn qbit_password_exposes_cleartext_only_via_expose_secret() {
        let p = QbitPassword::new("pw-xyz".to_string());
        assert_eq!(p.expose_secret(), "pw-xyz");
    }

    #[test]
    fn tracing_capture_of_secrets_does_not_leak_cleartext() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct MemWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for MemWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for MemWriter {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = MemWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .finish();

        let cookie = AuthCookie::new("auth-cleartext-xyz".to_string());
        let session = MamSessionId::new("mam-cleartext-xyz".to_string());
        let password = QbitPassword::new("pw-cleartext-xyz".to_string());

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(?cookie, ?session, ?password, "captured");
        });

        let bytes = buf.0.lock().unwrap().clone();
        let logged = String::from_utf8(bytes).unwrap();
        assert!(
            !logged.contains("auth-cleartext-xyz"),
            "AuthCookie cleartext leaked: {logged}"
        );
        assert!(
            !logged.contains("mam-cleartext-xyz"),
            "MamSessionId cleartext leaked: {logged}"
        );
        assert!(
            !logged.contains("pw-cleartext-xyz"),
            "QbitPassword cleartext leaked: {logged}"
        );
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
