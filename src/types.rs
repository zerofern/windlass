use nutype::nutype;
use secrecy::SecretString;
use std::net::Ipv4Addr;
use std::time::Duration;
pub use uom::si::f64::Information;

// ── IPs ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VpnIp(pub Ipv4Addr);

// ── Ports ────────────────────────────────────────────────────────────────────

#[nutype(
    validate(greater = 0, less_or_equal = 65535),
    derive(Debug, Clone, Copy, PartialEq, Eq)
)]
pub struct VpnPort(u16);

// ── HTTP ─────────────────────────────────────────────────────────────────────

/// An HTTP status code returned by an external service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpStatusCode(pub u16);

// ── Torrents ─────────────────────────────────────────────────────────────────

/// The display name of a torrent as reported by qBittorrent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TorrentName(pub String);

// ── Auth ─────────────────────────────────────────────────────────────────────

/// The SID cookie returned by qBittorrent on successful login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCookie(pub String);

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RetryCount(pub u8);

impl RetryCount {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WakeupId {
    Heartbeat,
    DiskCheck,
    TorrentCheck,
    QbitAuthRetry,
    QbitSyncRetry,
    RetryPortRead,
}

// ── Alert priority ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertPriority {
    Info,
    Warning,
    Critical,
}

// ── MAM connectivity ─────────────────────────────────────────────────────────

/// The result of a MAM connectivity heartbeat check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MamStatus {
    /// MAM reached and qBit is listed as connectable (accepts incoming connections).
    Connectable,
    /// MAM reached but qBit is not connectable — port forward or firewall issue.
    NotConnectable,
    /// Network failure, HTTP error, or parse failure reaching MAM.
    Unreachable,
}
