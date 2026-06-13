#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::{MamTorrentId, VpnIp, VpnPort};

/// 25 GiB in bytes (binary GiB: 1024³ = 1 073 741 824).
///
/// This is the default upload-credit-buffer threshold per §26.  The binary GiB
/// choice mirrors how storage capacities are measured on the tracker and is
/// the conventionally understood meaning of "25 GB" in torrent-tracker contexts.
pub const DEFAULT_MIN_UPLOAD_BUFFER_BYTES: u64 = 25 * 1024 * 1024 * 1024;

/// Default keep-alive interval (§27).  Matches Mousehole's default check
/// cadence (5 minutes / 300 seconds).
pub const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_mins(5);

/// Default consecutive-failure threshold for `KeepAliveDegraded` (§27).
pub const DEFAULT_KEEP_ALIVE_FAILURE_THRESHOLD: u32 = 3;

/// §31: default stale-registration refresh interval.  Matches Mousehole's
/// `STALE_RESPONSE_SECONDS` (86 400 s = 1 day).
pub const DEFAULT_STALE_REGISTRATION_INTERVAL: Duration = Duration::from_hours(24);

/// §32: default machine-side spacing between `UpdateSeedbox` attempts.
///
/// One minute over MAM's documented 1-hour `dynamicSeedbox.php` limit
/// (and the HTTP client's belt-and-braces guard) so a machine-side
/// retry can never race the guard and produce a `RateLimited`.
pub const DEFAULT_SEEDBOX_UPDATE_MIN_INTERVAL: Duration = Duration::from_mins(61);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MamConfig {
    pub status_retry: Duration,
    /// Minimum global ratio required for non-freeleech downloads (§26).
    /// Default: `2.0`.
    pub min_global_ratio: f64,
    /// Minimum upload-credit buffer (bytes-equivalent) for all downloads (§26).
    /// Freeleech grabs also require the buffer even though they bypass the ratio
    /// (§7.4 spec: freeleech does not spend ratio, but upload health still matters).
    /// Default: 25 GiB (`DEFAULT_MIN_UPLOAD_BUFFER_BYTES`).
    pub min_upload_buffer_bytes: u64,
    /// Recurring `FetchStatus` cadence that keeps the MAM account alive
    /// (§27, MAM Rule 1.6).  Default: 300 s
    /// (`DEFAULT_KEEP_ALIVE_INTERVAL`, matches Mousehole).
    pub keep_alive_interval: Duration,
    /// Consecutive retryable failures required to publish
    /// `KeepAliveDegraded` (§27).  `0` disables the alert.
    /// Default: `3` (`DEFAULT_KEEP_ALIVE_FAILURE_THRESHOLD`).
    pub keep_alive_failure_threshold: u32,
    /// §31: cadence of the self-perpetuating stale-registration refresh
    /// (`UpdateSeedbox` forced even when the observed IP is stable).
    /// Default: 24 hours (`DEFAULT_STALE_REGISTRATION_INTERVAL`,
    /// mirrors Mousehole's `STALE_RESPONSE_SECONDS`).
    pub stale_registration_interval: Duration,
    /// §32: minimum spacing between `UpdateSeedbox` attempts.  This is
    /// the single authoritative throttle for MAM's documented 1-hour
    /// `dynamicSeedbox.php` limit — the gateway here is the only thing
    /// that emits `UpdateSeedbox`, so no command/timer path can storm
    /// the endpoint.  Callers inside the window get a deferral timer
    /// instead of a doomed request.  Default: 61 minutes (one minute of
    /// headroom over MAM's hour).
    pub seedbox_update_min_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamCommand {
    EnsureAuthenticated,
    EnsureSeedboxPort {
        port: VpnPort,
    },
    RefreshStatus,
    /// §31: the domain forwards this when the VPN core publishes a fresh
    /// `PublicIpObserved`.  On a real change, MAM emits `UpdateSeedbox`
    /// and arms the stale-registration chain.  No-op if the IP matches
    /// the last observed value.
    ObservedIpChanged {
        ip: VpnIp,
    },
    /// §36 step 5: fetch the raw `.torrent` bytes for a MAM torrent id.
    /// Domain dispatches this on `WindlassCommand::ManualDownload`; the
    /// shell calls `mam_client.fetch_torrent(mam_id)` and emits
    /// `TorrentBytesFetched` / `TorrentBytesFetchFailed`.
    FetchTorrent {
        mam_id: MamTorrentId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MamTimer {
    StatusRetry,
    RateLimitExpired,
    /// §27: self-perpetuating heartbeat that drives recurring `FetchStatus`.
    KeepAlive,
    /// §31: 24h refresh that re-runs `UpdateSeedbox` even when nothing
    /// changed, so the session cookie stays fresh.
    StaleRegistrationRefresh,
}

impl MamTimer {
    /// Static name used as the `ExternalCause::Timer { name }` tag when
    /// the shell forwards a fired timer back into the runtime.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::StatusRetry => "MamTimer::StatusRetry",
            Self::RateLimitExpired => "MamTimer::RateLimitExpired",
            Self::KeepAlive => "MamTimer::KeepAlive",
            Self::StaleRegistrationRefresh => "MamTimer::StaleRegistrationRefresh",
        }
    }
}

// `MamEvent` cannot derive `Eq` because `StatusFetched` carries `ratio: f64`,
// and `f64` only implements `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MamEvent {
    Init,
    AuthSucceeded,
    AuthFailed {
        reason: String,
    },
    StatusFetched {
        connectable: bool,
        seedbox_port: Option<VpnPort>,
        /// Global upload ratio from MAM (§26).  `0.0` when the field is absent
        /// (fail-closed: the upload-health gate fires on a missing ratio).
        ratio: f64,
        /// Upload-credit proxy in bytes-equivalent (§26).  `0` when absent
        /// (fail-closed).
        upload_credit_bytes: u64,
    },
    StatusFailed {
        reason: String,
    },
    /// §28: MAM could not be reached at all (DNS/TCP/TLS/timeout).  Distinct
    /// from `StatusFailed` (MAM responded but the response was wrong) and
    /// from `StatusFetched { connectable: false }` (MAM responded and
    /// reports we are unconnectable).  Routed by the shell from
    /// `MamFetchError::Unreachable` or `Event::MamUnreachable`.
    Unreachable {
        reason: String,
    },
    /// §30: MAM rejected the dynamic-seedbox update with an ASN mismatch —
    /// our current IP belongs to an autonomous system the account is not
    /// registered for.  Distinct from `SeedboxUpdateFailed` (other refusals)
    /// because §30 routes it as a `Critical` compliance signal that blocks
    /// download admission (DOM-20).
    AsnMismatch {
        ip: VpnIp,
    },
    /// §32: successful dynamic-seedbox response.  Carries the IP/ASN/AS MAM
    /// reports as currently registered for the account.  All three are
    /// optional because the legacy bridge cannot always populate them.
    SeedboxUpdated {
        registered_ip: Option<VpnIp>,
        registered_asn: Option<u32>,
        registered_as: Option<String>,
    },
    SeedboxUpdateFailed {
        reason: String,
    },
    RateLimited {
        retry_after: Duration,
    },
    TimerFired(MamTimer),
    /// §36 step 5: shell fetched the `.torrent` bytes for `mam_id`.
    TorrentBytesFetched {
        mam_id: MamTorrentId,
        bytes: Vec<u8>,
    },
    /// §36 step 5: shell failed to fetch the `.torrent` bytes (network
    /// error, 4xx, etc.).
    TorrentBytesFetchFailed {
        mam_id: MamTorrentId,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamAction {
    FetchStatus,
    UpdateSeedbox,
    ScheduleTimer {
        timer: MamTimer,
        after: Duration,
    },
    /// §36 step 5: ask the shell to fetch the `.torrent` bytes for a
    /// manual-download admission.  Result arrives as
    /// `TorrentBytesFetched` / `TorrentBytesFetchFailed`.
    FetchTorrentBytes {
        mam_id: MamTorrentId,
    },
}

// `MamPublish` cannot derive `Eq` because `UploadHealthDegraded` carries `f64`
// fields, and `f64` only implements `PartialEq`, not `Eq` (NaN ≠ NaN).
// The other variants are logically equatable; this is an acceptable trade-off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MamPublish {
    Ready,
    Unavailable {
        reason: String,
    },
    RateLimited {
        retry_after: Duration,
    },
    Connectable {
        seedbox_port: Option<VpnPort>,
    },
    NotConnectable {
        reason: String,
    },
    /// §28: MAM could not be reached at all.  Distinct from `NotConnectable`,
    /// which means MAM responded and reports our client is not connectable.
    /// `Unreachable` is transient; the operator alert path lives in the
    /// keep-alive degraded publish (§27) rather than a Critical/Warning here.
    Unreachable {
        reason: String,
    },
    SeedboxPortReady {
        port: VpnPort,
    },
    /// Published when the upload-health gate would block a non-freeleech download
    /// (§26).  Published on every `StatusFetched` where `!upload_health_ok(false)`.
    UploadHealthDegraded {
        ratio: f64,
        upload_credit_bytes: u64,
        /// `true` iff `ratio >= config.min_global_ratio`.
        ratio_ok: bool,
        /// `true` iff `upload_credit_bytes >= config.min_upload_buffer_bytes`.
        buffer_ok: bool,
    },
    /// §29: positive counterpart to `UploadHealthDegraded`.  Published on
    /// every `StatusFetched` where `upload_health_ok(false)` is true (both
    /// ratio and upload-credit buffer meet their minimums).  Gives the
    /// domain admission state a rising-edge positive signal.
    UploadHealthOk {
        ratio: f64,
        upload_credit_bytes: u64,
    },
    /// §30: rising-edge ASN-mismatch signal.  Published exactly once when
    /// `asn_state` transitions from any non-`Mismatched` value (Unknown or
    /// Accepted) to `Mismatched`.  Carries the offending IP so the alert
    /// body can name it.
    AsnMismatch {
        ip: VpnIp,
    },
    /// §30: rising-edge ASN-accepted signal.  Published exactly once when
    /// `asn_state` transitions from any non-`Accepted` value (Unknown or
    /// Mismatched) to `Accepted`, i.e. on the first `SeedboxUpdated` after
    /// a fresh start or after a prior mismatch.
    AsnAccepted,
    /// §27: published exactly once on the rising edge when
    /// `consecutive_status_failures` crosses `keep_alive_failure_threshold`.
    /// Carries the last retryable-failure reason for the alert body.
    KeepAliveDegraded {
        consecutive_failures: u32,
        last_reason: String,
    },
    /// §36 step 5: `.torrent` bytes for `mam_id` are available.  Domain
    /// forwards as `QbitCommand::AddTorrent { mam_id, bytes }`.
    TorrentBytesReady {
        mam_id: MamTorrentId,
        bytes: Vec<u8>,
    },
    /// §36 step 5: the shell failed to fetch the `.torrent` bytes.
    /// Domain fires a Warning "Download failed" alert.
    TorrentBytesFetchFailed {
        mam_id: MamTorrentId,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamTopic {
    Availability,
    Connectability,
    Seedbox,
    /// Upload-health alerts (§26).
    UploadHealth,
    /// Keep-alive heartbeat degradation alerts (§27).
    KeepAlive,
    /// MAM ASN-compliance signals (§30).
    Compliance,
    /// §36 step 5: manual-download torrent-bytes fetch results.
    TorrentFetch,
}

impl HasTopic<MamTopic> for MamPublish {
    fn topic(&self) -> MamTopic {
        match self {
            Self::Ready | Self::Unavailable { .. } | Self::RateLimited { .. } => {
                MamTopic::Availability
            }
            Self::Connectable { .. } | Self::NotConnectable { .. } | Self::Unreachable { .. } => {
                MamTopic::Connectability
            }
            Self::SeedboxPortReady { .. } => MamTopic::Seedbox,
            Self::UploadHealthDegraded { .. } | Self::UploadHealthOk { .. } => {
                MamTopic::UploadHealth
            }
            Self::KeepAliveDegraded { .. } => MamTopic::KeepAlive,
            Self::AsnMismatch { .. } | Self::AsnAccepted => MamTopic::Compliance,
            Self::TorrentBytesReady { .. } | Self::TorrentBytesFetchFailed { .. } => {
                MamTopic::TorrentFetch
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamResponse {
    Accepted,
}

/// §30: MAM ASN-compliance state.
///
/// `Unknown` is the boot/restart default — we have not yet observed a
/// successful or rejected seedbox update.  Admission stays blocked until
/// the state transitions to `Accepted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsnState {
    Unknown,
    Accepted,
    Mismatched,
}

// `MamMachine` cannot derive `Eq` because `MamConfig.min_global_ratio` and the
// `ratio` field are `f64`, which only implements `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MamMachine {
    config: MamConfig,
    authenticated: bool,
    seedbox_port: Option<VpnPort>,
    desired_seedbox_port: Option<VpnPort>,
    /// Last observed global upload ratio (§26).  Initialised to `0.0`
    /// (fail-closed: the gate fires until a real value is observed).
    ratio: f64,
    /// Last observed upload-credit proxy in bytes-equivalent (§26).
    /// Initialised to `0` (fail-closed).
    upload_credit_bytes: u64,
    /// §27: `true` once the `KeepAlive` self-perpetuating chain has been
    /// started.  Guards against a duplicate chain being launched by a second
    /// `AuthSucceeded` (MAM-8).
    keep_alive_scheduled: bool,
    /// §27: consecutive retryable-failure count.  Incremented by every
    /// `AuthFailed`/`StatusFailed`/`SeedboxUpdateFailed`; reset by
    /// `StatusFetched`.
    consecutive_status_failures: u32,
    /// §30: MAM ASN-compliance state.  See `AsnState` docs.  Initial
    /// value is `Unknown` — domain admission stays blocked until a
    /// `SeedboxUpdated` arrives or a `Mismatched` is observed.
    asn_state: AsnState,
    /// §31: most recent IP forwarded from the VPN core's
    /// `PublicIpObserved` publish via the `ObservedIpChanged` command.
    /// `None` before the first observation or after a disconnect.
    observed_ip: Option<VpnIp>,
    /// §31: chain-starts-once guard for the
    /// `StaleRegistrationRefresh` timer.  Set on the first
    /// `ObservedIpChanged`, never cleared.
    stale_chain_scheduled: bool,
    /// §32 dedup baseline: the exit IP we last successfully registered
    /// with MAM (our `observed_ip` at the time of the successful
    /// update), NOT MAM's echoed IP.  `ObservedIpChanged { ip }` and
    /// `needs_seedbox_update` skip `UpdateSeedbox` iff `registered_ip
    /// == observed_ip`.  Keying on what we sent (rather than MAM's
    /// echo) keeps the dedup convergent even if MAM normalizes the
    /// address it reports back.
    registered_ip: Option<VpnIp>,
    /// §32: ASN MAM reported on the last successful dynamic-seedbox call.
    /// Carried for logging and future ASN-aware dedup.
    registered_asn: Option<u32>,
    /// §32: AS organization name from the last successful dynamic-seedbox
    /// call.  Carried for logging.
    registered_as: Option<String>,
    /// §32: wall-clock instant an `UpdateSeedbox` action was emitted
    /// and not yet answered (single-flight gate).  Cleared by every
    /// completion event (`SeedboxUpdated` / `SeedboxUpdateFailed` /
    /// `AsnMismatch` / `Unreachable` / `RateLimited`); treated as
    /// stale after 5 minutes so a lost completion can't wedge updates
    /// forever.
    seedbox_update_in_flight_since: Option<chrono::DateTime<chrono::Utc>>,
    /// §32: wall-clock instant of the last `UpdateSeedbox` emission,
    /// driving the machine-side min-interval window.  Reset to `None`
    /// on `RateLimited` — that attempt never reached MAM, and the
    /// client's `retry_after` governs the next try instead.
    last_seedbox_update_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl MamMachine {
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    #[must_use]
    pub const fn seedbox_port(&self) -> Option<VpnPort> {
        self.seedbox_port
    }

    /// Returns the last observed global upload ratio (§26).
    #[must_use]
    pub const fn ratio(&self) -> f64 {
        self.ratio
    }

    /// Returns the last observed upload-credit proxy in bytes-equivalent (§26).
    #[must_use]
    pub const fn upload_credit_bytes(&self) -> u64 {
        self.upload_credit_bytes
    }

    /// Returns the current consecutive retryable-failure count (§27).
    #[must_use]
    pub const fn consecutive_status_failures(&self) -> u32 {
        self.consecutive_status_failures
    }

    /// Returns the current MAM ASN-compliance state (§30).
    #[must_use]
    pub const fn asn_state(&self) -> AsnState {
        self.asn_state
    }

    /// Returns the most recent observed VPN IP (§31).
    #[must_use]
    pub const fn observed_ip(&self) -> Option<VpnIp> {
        self.observed_ip
    }

    /// Returns the IP MAM has recorded on the last successful seedbox
    /// update (§32) — the primary Mousehole-style dedup target.
    #[must_use]
    pub const fn registered_ip(&self) -> Option<VpnIp> {
        self.registered_ip
    }

    /// Returns the ASN MAM has recorded on the last successful seedbox
    /// update (§32).
    #[must_use]
    pub const fn registered_asn(&self) -> Option<u32> {
        self.registered_asn
    }

    /// Returns `true` once the `KeepAlive` chain has been started (§27).
    #[must_use]
    pub const fn keep_alive_scheduled(&self) -> bool {
        self.keep_alive_scheduled
    }

    /// Increments the consecutive-failure count, returning `true` iff this
    /// bump crossed `keep_alive_failure_threshold` from below — the
    /// rising-edge predicate behind MAM-10.  When the threshold is `0` the
    /// gate is disabled and this always returns `false`.
    const fn bump_keep_alive_failures(&mut self) -> bool {
        let threshold = self.config.keep_alive_failure_threshold;
        if threshold == 0 {
            self.consecutive_status_failures = self.consecutive_status_failures.saturating_add(1);
            return false;
        }
        let before = self.consecutive_status_failures;
        let after = before.saturating_add(1);
        self.consecutive_status_failures = after;
        before < threshold && after >= threshold
    }

    /// Returns `true` when the upload-health gate would allow a new download.
    ///
    /// - When `freeleech == false`: both `ratio >= min_global_ratio` **and**
    ///   `upload_credit_bytes >= min_upload_buffer_bytes` must hold.
    /// - When `freeleech == true`: freeleech bypasses the ratio requirement
    ///   (§7.4 spec — freeleech does not spend ratio) but the buffer requirement
    ///   still applies.
    #[must_use]
    pub fn upload_health_ok(&self, freeleech: bool) -> bool {
        let buffer_ok = self.upload_credit_bytes >= self.config.min_upload_buffer_bytes;
        if freeleech {
            buffer_ok
        } else {
            self.ratio >= self.config.min_global_ratio && buffer_ok
        }
    }

    /// Whether MAM's registered state diverges from what we want it to
    /// be — either the desired forwarded port isn't registered yet, or
    /// the observed exit IP differs from what MAM last recorded
    /// (Mousehole's `getUpdateReason`: skip the call when nothing
    /// changed).  This is the single source of truth for "does an
    /// `UpdateSeedbox` need to happen", so a deferred retry can
    /// re-decide correctly regardless of whether the trigger was a
    /// port change or an IP change.
    fn needs_seedbox_update(&self) -> bool {
        let port_stale = self
            .desired_seedbox_port
            .is_some_and(|desired| self.seedbox_port != Some(desired));
        let ip_stale = self
            .observed_ip
            .is_some_and(|observed| self.registered_ip != Some(observed));
        port_stale || ip_stale
    }

    /// Picks the right action on a retry/rate-limit-expiry tick:
    /// re-attempt the update if anything still diverges, else fall
    /// back to a status poll (keep-alive).
    fn refresh_or_update_seedbox(
        &mut self,
        wall_now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<MamAction> {
        if self.needs_seedbox_update() {
            self.request_seedbox_update(wall_now)
        } else {
            vec![MamAction::FetchStatus]
        }
    }

    /// Re-attempt an update if the registered state still diverges,
    /// triggered by a completion (`StatusFetched` / `SeedboxUpdated`).
    /// Unlike [`Self::refresh_or_update_seedbox`] it stays silent when
    /// nothing is needed (no keep-alive poll on this path).
    fn converge_seedbox(&mut self, wall_now: chrono::DateTime<chrono::Utc>) -> Vec<MamAction> {
        if self.needs_seedbox_update() {
            self.request_seedbox_update(wall_now)
        } else {
            Vec::new()
        }
    }

    /// Single gateway for emitting `UpdateSeedbox`.
    ///
    /// Enforces single-flight and the machine-side min-interval window
    /// here — in the pure machine, where it is observable and testable
    /// — so no combination of commands, retries, and timer chains can
    /// storm `dynamicSeedbox.php`.  A caller that wants an update while
    /// one is in flight gets nothing (every completion handler
    /// re-converges); a caller inside the window gets a deferral timer
    /// for the window's remainder (`KeyedTimers` in the shell collapses
    /// duplicates).
    ///
    /// This does NOT gate on [`Self::needs_seedbox_update`]: the
    /// stale-registration refresh (§31) deliberately forces an update
    /// even when nothing diverged, to keep the MAM session cookie
    /// fresh.  Convergence callers gate themselves (the dedup in
    /// `ObservedIpChanged`, the port check in `EnsureSeedboxPort`, and
    /// the `needs_seedbox_update` check in the retry/converge paths).
    fn request_seedbox_update(
        &mut self,
        wall_now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<MamAction> {
        // A lost completion must not wedge updates forever.
        const IN_FLIGHT_STALE_SECONDS: i64 = 300;
        if let Some(since) = self.seedbox_update_in_flight_since
            && wall_now.signed_duration_since(since).num_seconds() < IN_FLIGHT_STALE_SECONDS
        {
            return Vec::new();
        }
        if let Some(last) = self.last_seedbox_update_attempt_at {
            let window = chrono::Duration::from_std(self.config.seedbox_update_min_interval)
                .unwrap_or_else(|_| chrono::Duration::hours(1));
            let elapsed = wall_now.signed_duration_since(last);
            if elapsed >= chrono::Duration::zero() && elapsed < window {
                let remaining = (window - elapsed)
                    .to_std()
                    .unwrap_or(Duration::from_secs(1));
                return vec![MamAction::ScheduleTimer {
                    timer: MamTimer::RateLimitExpired,
                    after: remaining,
                }];
            }
        }
        self.seedbox_update_in_flight_since = Some(wall_now);
        self.last_seedbox_update_attempt_at = Some(wall_now);
        vec![MamAction::UpdateSeedbox]
    }

    fn seedbox_publish(&self, seedbox_port: Option<VpnPort>) -> Vec<MamPublish> {
        seedbox_port
            .filter(|port| {
                self.desired_seedbox_port
                    .is_none_or(|desired_port| desired_port == *port)
            })
            .map(|port| MamPublish::SeedboxPortReady { port })
            .into_iter()
            .collect()
    }
}

impl Machine for MamMachine {
    type Config = MamConfig;
    type Event = MamEvent;
    type Action = MamAction;
    type Publish = MamPublish;
    type Topic = MamTopic;
    type Command = MamCommand;
    type Response = MamResponse;
    type StateSnapshot = Self;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            authenticated: false,
            seedbox_port: None,
            desired_seedbox_port: None,
            // Start at 0.0 / 0 so the upload-health gate fires until real
            // values are observed (fail-closed per §26).
            ratio: 0.0,
            upload_credit_bytes: 0,
            keep_alive_scheduled: false,
            consecutive_status_failures: 0,
            asn_state: AsnState::Unknown,
            observed_ip: None,
            stale_chain_scheduled: false,
            registered_ip: None,
            registered_asn: None,
            registered_as: None,
            seedbox_update_in_flight_since: None,
            last_seedbox_update_attempt_at: None,
        }
    }

    // Each event arm is a small, self-contained decision; the function is long
    // because the event set is large, not because any single arm is complex.
    #[allow(clippy::too_many_lines)]
    fn handle(
        &mut self,
        _now: Instant,
        wall_now: chrono::DateTime<chrono::Utc>,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            MamEvent::Init => Outcome {
                actions: vec![MamAction::FetchStatus],
                publishes: Vec::new(),
            },
            MamEvent::TimerFired(MamTimer::StatusRetry | MamTimer::RateLimitExpired) => Outcome {
                actions: self.refresh_or_update_seedbox(wall_now),
                publishes: Vec::new(),
            },
            // §27: the keep-alive timer always re-schedules itself before
            // emitting the FetchStatus action, so a dropped result or shell
            // error cannot kill the chain (MAM-9).
            MamEvent::TimerFired(MamTimer::KeepAlive) => Outcome {
                actions: vec![
                    MamAction::FetchStatus,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        after: self.config.keep_alive_interval,
                    },
                ],
                publishes: Vec::new(),
            },
            // §31 / MAM-17: stale-registration refresh.  Mousehole's
            // `STALE_RESPONSE_SECONDS` analog — force a `UpdateSeedbox`
            // once per `stale_registration_interval` even when the IP is
            // unchanged, so the MAM session cookie stays fresh.  Always
            // re-schedules itself.
            MamEvent::TimerFired(MamTimer::StaleRegistrationRefresh) => {
                let mut actions = self.request_seedbox_update(wall_now);
                actions.push(MamAction::ScheduleTimer {
                    timer: MamTimer::StaleRegistrationRefresh,
                    after: self.config.stale_registration_interval,
                });
                Outcome {
                    actions,
                    publishes: Vec::new(),
                }
            }
            MamEvent::AuthSucceeded => {
                self.authenticated = true;
                let mut actions = vec![MamAction::FetchStatus];
                // §27 / MAM-8: start the keep-alive chain at most once per
                // machine lifetime, on the first AuthSucceeded.
                if !self.keep_alive_scheduled {
                    self.keep_alive_scheduled = true;
                    actions.push(MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        after: self.config.keep_alive_interval,
                    });
                }
                Outcome {
                    actions,
                    publishes: vec![MamPublish::Ready],
                }
            }
            MamEvent::AuthFailed { reason }
            | MamEvent::StatusFailed { reason }
            | MamEvent::SeedboxUpdateFailed { reason } => {
                self.seedbox_update_in_flight_since = None;
                let mut publishes = vec![MamPublish::Unavailable {
                    reason: reason.clone(),
                }];
                // §27 / MAM-10: increment the consecutive-failure count, and
                // publish KeepAliveDegraded exactly once on the rising edge
                // when the count crosses the configured threshold.
                let crossed = self.bump_keep_alive_failures();
                if crossed {
                    publishes.push(MamPublish::KeepAliveDegraded {
                        consecutive_failures: self.consecutive_status_failures,
                        last_reason: reason,
                    });
                }
                Outcome {
                    actions: vec![MamAction::ScheduleTimer {
                        timer: MamTimer::StatusRetry,
                        after: self.config.status_retry,
                    }],
                    publishes,
                }
            }
            // §28 / MAM-11: a transport-level failure publishes
            // `Unreachable` on the Connectability topic — distinct from
            // `Unavailable` (which means "MAM responded but is broken for me
            // right now") and from `NotConnectable` (which means "MAM
            // responded and reports my client is unreachable from their
            // side").  Same StatusRetry + keep-alive-counter handling as the
            // other retryable failures.
            MamEvent::Unreachable { reason } => {
                self.seedbox_update_in_flight_since = None;
                let mut publishes = vec![MamPublish::Unreachable {
                    reason: reason.clone(),
                }];
                let crossed = self.bump_keep_alive_failures();
                if crossed {
                    publishes.push(MamPublish::KeepAliveDegraded {
                        consecutive_failures: self.consecutive_status_failures,
                        last_reason: reason,
                    });
                }
                Outcome {
                    actions: vec![MamAction::ScheduleTimer {
                        timer: MamTimer::StatusRetry,
                        after: self.config.status_retry,
                    }],
                    publishes,
                }
            }
            MamEvent::StatusFetched {
                connectable,
                seedbox_port,
                ratio,
                upload_credit_bytes,
            } => {
                self.seedbox_port = seedbox_port;
                self.ratio = ratio;
                self.upload_credit_bytes = upload_credit_bytes;
                // §27: a successful status read resets the consecutive-
                // failure count.  After a future failure burst, the rising
                // edge over the threshold republishes KeepAliveDegraded.
                self.consecutive_status_failures = 0;
                let mut publishes = vec![if connectable {
                    MamPublish::Connectable { seedbox_port }
                } else {
                    MamPublish::NotConnectable {
                        reason: "MAM reports not connectable".to_string(),
                    }
                }];
                if connectable {
                    publishes.extend(self.seedbox_publish(seedbox_port));
                }
                // §26: publish UploadHealthDegraded when the strictest
                // (non-freeleech) gate would block.  §29: publish
                // UploadHealthOk when both metrics meet the minimums, so
                // the domain admission state has a rising-edge positive
                // signal.  Either branch is exhaustive — exactly one fires.
                if self.upload_health_ok(false) {
                    publishes.push(MamPublish::UploadHealthOk {
                        ratio,
                        upload_credit_bytes,
                    });
                } else {
                    let ratio_ok = ratio >= self.config.min_global_ratio;
                    let buffer_ok = upload_credit_bytes >= self.config.min_upload_buffer_bytes;
                    publishes.push(MamPublish::UploadHealthDegraded {
                        ratio,
                        upload_credit_bytes,
                        ratio_ok,
                        buffer_ok,
                    });
                }
                Outcome {
                    actions: self.converge_seedbox(wall_now),
                    publishes,
                }
            }
            MamEvent::SeedboxUpdated {
                // MAM's echoed IP is intentionally ignored for dedup
                // (see below); only ASN/AS are recorded.
                registered_ip: _,
                registered_asn,
                registered_as,
            } => {
                self.seedbox_update_in_flight_since = None;
                let port = self.desired_seedbox_port;
                if let Some(p) = port {
                    self.seedbox_port = Some(p);
                }
                // §32 dedup baseline: record the exit IP *we just
                // registered* (our observed IP), not MAM's echoed
                // `registered_ip`.  The dedup question is "have I
                // already told MAM about this observed IP", so the
                // baseline must be what we sent.  Keying off MAM's echo
                // instead means any divergence between the two (MAM
                // normalizing the address, or — in tests — a fake that
                // can't see our tunnel egress) makes the dedup never
                // converge and re-calls `dynamicSeedbox.php` every
                // interval forever.  MAM's echoed ASN/AS are still
                // recorded for logging/compliance.
                self.registered_ip = self.observed_ip;
                if registered_asn.is_some() {
                    self.registered_asn = registered_asn;
                }
                if registered_as.is_some() {
                    self.registered_as = registered_as;
                }
                let mut publishes: Vec<MamPublish> = port
                    .map(|p| MamPublish::SeedboxPortReady { port: p })
                    .into_iter()
                    .collect();
                // §30 / MAM-15: rising-edge transition to ASN-accepted.  Only
                // publish when transitioning from Unknown or Mismatched.
                if self.asn_state != AsnState::Accepted {
                    self.asn_state = AsnState::Accepted;
                    publishes.push(MamPublish::AsnAccepted);
                }
                Outcome {
                    // Re-converge: desired state may have moved while
                    // this update was in flight (the single-flight
                    // gate swallowed those requests).
                    actions: self.converge_seedbox(wall_now),
                    publishes,
                }
            }
            // §30 / MAM-14: rising-edge ASN-mismatch.  Publishes
            // `AsnMismatch` only when transitioning from Unknown or Accepted
            // to Mismatched; subsequent mismatches before a successful
            // update do not re-publish.  Also schedules `StatusRetry` and
            // bumps the §27 keep-alive failure counter — a persistent ASN
            // mismatch shows up as a degraded heartbeat too.
            MamEvent::AsnMismatch { ip } => {
                self.seedbox_update_in_flight_since = None;
                let mut publishes = Vec::new();
                if self.asn_state != AsnState::Mismatched {
                    self.asn_state = AsnState::Mismatched;
                    publishes.push(MamPublish::AsnMismatch { ip });
                }
                let crossed = self.bump_keep_alive_failures();
                if crossed {
                    publishes.push(MamPublish::KeepAliveDegraded {
                        consecutive_failures: self.consecutive_status_failures,
                        last_reason: format!("ASN mismatch for {}", ip.0),
                    });
                }
                Outcome {
                    actions: vec![MamAction::ScheduleTimer {
                        timer: MamTimer::StatusRetry,
                        after: self.config.status_retry,
                    }],
                    publishes,
                }
            }
            MamEvent::RateLimited { retry_after } => {
                // The attempt never reached MAM; the guard's honest
                // retry_after governs the next try, so the machine
                // window must not double-penalize it.
                self.seedbox_update_in_flight_since = None;
                self.last_seedbox_update_attempt_at = None;
                Outcome {
                    actions: vec![MamAction::ScheduleTimer {
                        timer: MamTimer::RateLimitExpired,
                        after: retry_after,
                    }],
                    publishes: vec![MamPublish::RateLimited { retry_after }],
                }
            }
            // §36 step 5: forward the fetched bytes to subscribers (domain
            // routes them to QbitCommand::AddTorrent).
            MamEvent::TorrentBytesFetched { mam_id, bytes } => Outcome {
                actions: Vec::new(),
                publishes: vec![MamPublish::TorrentBytesReady { mam_id, bytes }],
            },
            MamEvent::TorrentBytesFetchFailed { mam_id, reason } => Outcome {
                actions: Vec::new(),
                publishes: vec![MamPublish::TorrentBytesFetchFailed { mam_id, reason }],
            },
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        wall_now: chrono::DateTime<chrono::Utc>,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            MamCommand::EnsureAuthenticated | MamCommand::RefreshStatus => {
                vec![MamAction::FetchStatus]
            }
            MamCommand::EnsureSeedboxPort { port } => {
                self.desired_seedbox_port = Some(port);
                if self.seedbox_port == Some(port) {
                    return Self::outcome_with_publish(
                        Vec::new(),
                        vec![MamPublish::SeedboxPortReady { port }],
                        MamResponse::Accepted,
                    );
                }
                self.request_seedbox_update(wall_now)
            }
            // §31 / MAM-16 + §32: Mousehole-style dedup.  Skip
            // `UpdateSeedbox` when MAM has already recorded this IP
            // (`registered_ip == Some(ip)`).  The `observed_ip` check is now
            // a fallback for the boot window before any successful seedbox
            // call has populated `registered_ip`.  Also arms the
            // self-perpetuating 24h stale-registration timer on the first
            // observation.
            MamCommand::ObservedIpChanged { ip } => {
                let already_registered = self.registered_ip == Some(ip);
                let already_observed = self.observed_ip == Some(ip);
                self.observed_ip = Some(ip);
                if already_registered || already_observed {
                    return Self::outcome(Vec::new(), MamResponse::Accepted);
                }
                let mut actions = self.request_seedbox_update(wall_now);
                if !self.stale_chain_scheduled {
                    self.stale_chain_scheduled = true;
                    actions.push(MamAction::ScheduleTimer {
                        timer: MamTimer::StaleRegistrationRefresh,
                        after: self.config.stale_registration_interval,
                    });
                }
                actions
            }
            // §36 step 5: route the manual-download fetch request to the
            // shell.  The machine does not change state — the result will
            // arrive as a `TorrentBytesFetched` / `TorrentBytesFetchFailed`
            // event and publish accordingly.
            MamCommand::FetchTorrent { mam_id } => {
                vec![MamAction::FetchTorrentBytes { mam_id }]
            }
        };
        Self::outcome(actions, MamResponse::Accepted)
    }

    fn state_snapshot(&self) -> Self::StateSnapshot {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{ExternalCause, Machine, Outcome, Timed};
    use windlass_types::VpnPort;

    use crate::{MamAction, MamCommand, MamConfig, MamEvent, MamMachine, MamPublish, MamTimer};

    fn machine() -> MamMachine {
        MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 3,
                stale_registration_interval: Duration::from_secs(86_400),
                seedbox_update_min_interval: Duration::ZERO,
            },
            Instant::now(),
        )
    }

    fn handle(machine: &mut MamMachine, event: MamEvent) -> Outcome<MamAction, MamPublish> {
        machine.handle(
            Instant::now(),
            chrono::Utc::now(),
            Timed::external(Instant::now(), ExternalCause::Unknown, event),
        )
    }

    #[test]
    fn state_snapshot_reflects_authentication() {
        // §37b: AuthSucceeded flips `authenticated`, and the snapshot
        // serializes it. AsnState defaults to Unknown until §30 wires
        // a real signal, so we also confirm the field is present.
        let mut machine = machine();
        let _ = handle(&mut machine, MamEvent::AuthSucceeded);
        let value =
            serde_json::to_value(machine.state_snapshot()).expect("snapshot should serialize");
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["asn_state"], "Unknown");
    }

    #[test]
    fn auth_success_publishes_ready_and_fetches_status() {
        let mut machine = machine();

        let out = handle(&mut machine, MamEvent::AuthSucceeded);

        assert!(machine.is_authenticated());
        // §27: AuthSucceeded triggers a status fetch *and* arms the
        // self-perpetuating KeepAlive timer.
        assert_eq!(
            out.actions,
            vec![
                MamAction::FetchStatus,
                MamAction::ScheduleTimer {
                    timer: MamTimer::KeepAlive,
                    after: Duration::from_secs(300),
                },
            ]
        );
        assert_eq!(out.publishes, vec![MamPublish::Ready]);
        assert!(machine.keep_alive_scheduled());
    }

    #[test]
    fn ensure_authenticated_command_fetches_status() {
        let mut machine = machine();

        let out = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureAuthenticated,
        );

        assert_eq!(out.actions, vec![MamAction::FetchStatus]);
    }

    #[test]
    fn rate_limit_schedules_expiry_timer() {
        let mut machine = machine();
        let retry_after = Duration::from_secs(30);

        let out = handle(&mut machine, MamEvent::RateLimited { retry_after });

        assert_eq!(
            out.actions,
            vec![MamAction::ScheduleTimer {
                timer: MamTimer::RateLimitExpired,
                after: retry_after,
            }]
        );
        assert_eq!(out.publishes, vec![MamPublish::RateLimited { retry_after }]);
    }

    #[test]
    fn seedbox_update_publishes_ready_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        // Set a desired port so the machine knows which port was converged.
        let _ = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port },
        );

        let out = handle(
            &mut machine,
            MamEvent::SeedboxUpdated {
                registered_ip: None,
                registered_asn: None,
                registered_as: None,
            },
        );

        assert_eq!(machine.seedbox_port(), Some(port));
        // §30: the first SeedboxUpdated also publishes AsnAccepted (rising
        // edge from Unknown).
        assert_eq!(
            out.publishes,
            vec![
                MamPublish::SeedboxPortReady { port },
                MamPublish::AsnAccepted,
            ]
        );
    }

    #[test]
    fn status_mismatch_updates_desired_seedbox_without_publishing_ready() {
        let mut machine = machine();
        let desired = VpnPort::try_new(51_820).unwrap();
        let observed = VpnPort::try_new(42_000).unwrap();
        // EnsureSeedboxPort fires the first UpdateSeedbox…
        let first = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port: desired },
        );
        assert_eq!(first.actions, vec![MamAction::UpdateSeedbox]);
        // …which the single-flight gate holds open: a StatusFetched
        // mismatch while it is in flight must NOT fire a second one
        // (the in-flight update registers the desired port already).
        let out = handle(
            &mut machine,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: Some(observed),
                // Healthy ratio/buffer so no UploadHealthDegraded publish.
                ratio: 3.0,
                upload_credit_bytes: 50 * 1024 * 1024 * 1024,
            },
        );
        assert!(
            out.actions.is_empty(),
            "single-flight must hold: {:?}",
            out.actions
        );
        assert_eq!(
            out.publishes,
            vec![
                MamPublish::Connectable {
                    seedbox_port: Some(observed),
                },
                // §29: healthy ratio/buffer now produces a positive
                // UploadHealthOk signal alongside Connectable.
                MamPublish::UploadHealthOk {
                    ratio: 3.0,
                    upload_credit_bytes: 50 * 1024 * 1024 * 1024,
                },
            ]
        );
        // Once the in-flight attempt fails, the next status mismatch
        // re-fires the update.
        let _ = handle(
            &mut machine,
            MamEvent::SeedboxUpdateFailed {
                reason: "boom".to_string(),
            },
        );
        let retry = handle(
            &mut machine,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: Some(observed),
                ratio: 3.0,
                upload_credit_bytes: 50 * 1024 * 1024 * 1024,
            },
        );
        assert_eq!(retry.actions, vec![MamAction::UpdateSeedbox]);
    }

    /// §32: the boot burst — port and IP arriving piecemeal within the
    /// same second — must produce exactly ONE `UpdateSeedbox`, not one
    /// per command (single-flight + window coalescing).
    #[test]
    fn piecemeal_boot_state_coalesces_into_one_update() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let ip = windlass_types::VpnIp(std::net::Ipv4Addr::new(203, 0, 113, 7));
        let first = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port },
        );
        let second = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::ObservedIpChanged { ip },
        );
        let updates = |a: &[MamAction]| {
            a.iter()
                .filter(|x| matches!(x, MamAction::UpdateSeedbox))
                .count()
        };
        assert_eq!(updates(&first.actions), 1);
        assert_eq!(
            updates(&second.actions),
            0,
            "second desired-state change must coalesce into the in-flight update"
        );
        // The in-flight completion re-converges; nothing is lost.
        let done = handle(
            &mut machine,
            MamEvent::SeedboxUpdated {
                registered_ip: Some(ip),
                registered_asn: Some(1),
                registered_as: Some("AS".to_string()),
            },
        );
        assert_eq!(
            updates(&done.actions),
            0,
            "desired state is satisfied; no further update"
        );
    }

    /// §32: a lost completion event must not wedge updates forever —
    /// after the 5-minute staleness horizon the single-flight gate
    /// yields and a new attempt goes out.
    #[test]
    fn stale_in_flight_update_does_not_wedge_the_gateway() {
        let mut machine = machine();
        let port_a = VpnPort::try_new(51_820).unwrap();
        let port_b = VpnPort::try_new(42_000).unwrap();
        let first = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port: port_a },
        );
        assert_eq!(first.actions, vec![MamAction::UpdateSeedbox]);
        // No completion ever arrives.  Pretend the attempt started
        // 10 minutes ago by backdating the in-flight timestamp.
        machine.seedbox_update_in_flight_since =
            Some(chrono::Utc::now() - chrono::Duration::minutes(10));
        machine.last_seedbox_update_attempt_at =
            Some(chrono::Utc::now() - chrono::Duration::minutes(10));
        let second = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port: port_b },
        );
        assert_eq!(
            second.actions,
            vec![MamAction::UpdateSeedbox],
            "a stale in-flight marker must not block updates forever"
        );
    }

    /// §32 regression (caught by `exit_ip_change_triggers_new_seedbox_call`):
    /// an exit-IP change that arrives inside the update window must
    /// defer and then, when the window-expiry timer fires, re-emit
    /// `UpdateSeedbox` — not collapse to `FetchStatus`.  The bug was a
    /// deferral retry that only re-checked the *port*, dropping
    /// IP-only changes on the floor.
    #[test]
    fn deferred_ip_change_retries_as_update_not_status() {
        let ip_old = windlass_types::VpnIp(std::net::Ipv4Addr::new(10, 2, 0, 2));
        let ip_new = windlass_types::VpnIp(std::net::Ipv4Addr::new(10, 8, 0, 42));
        let mut machine = MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 3,
                stale_registration_interval: Duration::from_secs(86_400),
                seedbox_update_min_interval: Duration::from_secs(3600),
            },
            Instant::now(),
        );
        // Controlled clock: the window is 1 h, so the timer fires an
        // hour after the boot update.  Unit tests can't sleep, so we
        // pass explicit `wall_now` values.
        let t0 = chrono::Utc::now();
        let cmd = |m: &mut MamMachine, at: chrono::DateTime<chrono::Utc>, c: MamCommand| {
            m.handle_command(Instant::now(), at, c)
        };
        let ev = |m: &mut MamMachine, at: chrono::DateTime<chrono::Utc>, e: MamEvent| {
            m.handle(
                Instant::now(),
                at,
                Timed::external(Instant::now(), ExternalCause::Unknown, e),
            )
        };

        // Register the first IP (boot path), arming the §32 window.
        let _ = cmd(
            &mut machine,
            t0,
            MamCommand::ObservedIpChanged { ip: ip_old },
        );
        let _ = ev(
            &mut machine,
            t0,
            MamEvent::SeedboxUpdated {
                registered_ip: Some(ip_old),
                registered_asn: Some(1),
                registered_as: Some("AS".to_string()),
            },
        );
        // A new exit IP one second later (inside the window): must
        // defer, not update.
        let deferred = cmd(
            &mut machine,
            t0 + chrono::Duration::seconds(1),
            MamCommand::ObservedIpChanged { ip: ip_new },
        );
        assert!(
            !deferred
                .actions
                .iter()
                .any(|a| matches!(a, MamAction::UpdateSeedbox)),
            "inside the window the IP change must defer: {:?}",
            deferred.actions
        );
        assert!(deferred.actions.iter().any(|a| matches!(
            a,
            MamAction::ScheduleTimer {
                timer: MamTimer::RateLimitExpired,
                ..
            }
        )));
        // When the window-expiry timer fires (an hour later), the IP
        // change must re-emit UpdateSeedbox (the bug returned
        // FetchStatus here).
        let retry = ev(
            &mut machine,
            t0 + chrono::Duration::seconds(3601),
            MamEvent::TimerFired(MamTimer::RateLimitExpired),
        );
        assert_eq!(retry.actions, vec![MamAction::UpdateSeedbox]);
    }

    /// §32: a `RateLimited` refusal never reached MAM, so it must not
    /// consume the machine-side window — when the guard's honest
    /// `retry_after` timer fires, the retry goes out immediately.
    #[test]
    fn rate_limited_does_not_consume_the_update_window() {
        let mut machine = MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 3,
                stale_registration_interval: Duration::from_secs(86_400),
                seedbox_update_min_interval: Duration::from_secs(3600),
            },
            Instant::now(),
        );
        let port = VpnPort::try_new(51_820).unwrap();
        let first = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port },
        );
        assert_eq!(first.actions, vec![MamAction::UpdateSeedbox]);
        // The client guard refuses the attempt.
        let limited = handle(
            &mut machine,
            MamEvent::RateLimited {
                retry_after: Duration::from_secs(120),
            },
        );
        assert!(limited.actions.iter().any(|a| matches!(
            a,
            MamAction::ScheduleTimer {
                timer: MamTimer::RateLimitExpired,
                ..
            }
        )));
        // When the retry timer fires, the update must go out — the
        // refused attempt must not have armed the 1h machine window.
        let retry = handle(
            &mut machine,
            MamEvent::TimerFired(MamTimer::RateLimitExpired),
        );
        assert_eq!(retry.actions, vec![MamAction::UpdateSeedbox]);
    }

    /// §31 + §32: the 24h stale-registration refresh goes through the
    /// gateway too — inside the window it defers instead of firing,
    /// but its self-perpetuating chain must survive the deferral.
    #[test]
    fn stale_refresh_defers_inside_window_but_keeps_its_chain() {
        let mut machine = MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 3,
                stale_registration_interval: Duration::from_secs(86_400),
                seedbox_update_min_interval: Duration::from_secs(3600),
            },
            Instant::now(),
        );
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port },
        );
        let _ = handle(
            &mut machine,
            MamEvent::SeedboxUpdated {
                registered_ip: None,
                registered_asn: None,
                registered_as: None,
            },
        );
        // Inside the window: no update, but the 24h chain re-arms and
        // the window-expiry timer is scheduled.
        let out = handle(
            &mut machine,
            MamEvent::TimerFired(MamTimer::StaleRegistrationRefresh),
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, MamAction::UpdateSeedbox)),
            "stale refresh inside the window must defer: {:?}",
            out.actions
        );
        assert!(out.actions.iter().any(|a| matches!(
            a,
            MamAction::ScheduleTimer {
                timer: MamTimer::StaleRegistrationRefresh,
                ..
            }
        )));
        assert!(out.actions.iter().any(|a| matches!(
            a,
            MamAction::ScheduleTimer {
                timer: MamTimer::RateLimitExpired,
                ..
            }
        )));
    }

    /// §32: inside the machine-side min-interval window, an update
    /// request defers (schedules the window-expiry timer) instead of
    /// firing a doomed attempt into the client guard.
    #[test]
    fn update_inside_window_defers_instead_of_firing() {
        let mut machine = MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 3,
                stale_registration_interval: Duration::from_secs(86_400),
                seedbox_update_min_interval: Duration::from_secs(3600),
            },
            Instant::now(),
        );
        let port_a = VpnPort::try_new(51_820).unwrap();
        let port_b = VpnPort::try_new(42_000).unwrap();
        let _ = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port: port_a },
        );
        // Complete the first update successfully.
        let _ = handle(
            &mut machine,
            MamEvent::SeedboxUpdated {
                registered_ip: None,
                registered_asn: None,
                registered_as: None,
            },
        );
        // A new desired port inside the window: deferral, not attempt.
        let out = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port: port_b },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, MamAction::UpdateSeedbox)),
            "window must defer the attempt: {:?}",
            out.actions
        );
        assert!(
            out.actions.iter().any(|a| matches!(
                a,
                MamAction::ScheduleTimer {
                    timer: MamTimer::RateLimitExpired,
                    ..
                }
            )),
            "deferral must schedule the window-expiry timer: {:?}",
            out.actions
        );
    }

    #[test]
    fn seedbox_update_failure_retries_desired_port_without_ready_publish() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port },
        );

        let failed = handle(
            &mut machine,
            MamEvent::SeedboxUpdateFailed {
                reason: "rate limited".to_string(),
            },
        );

        assert_eq!(
            failed.actions,
            vec![MamAction::ScheduleTimer {
                timer: MamTimer::StatusRetry,
                after: Duration::from_secs(5),
            }]
        );
        assert_eq!(
            failed.publishes,
            vec![MamPublish::Unavailable {
                reason: "rate limited".to_string(),
            }]
        );

        let retry = handle(&mut machine, MamEvent::TimerFired(MamTimer::StatusRetry));

        assert_eq!(retry.actions, vec![MamAction::UpdateSeedbox]);
    }

    #[test]
    fn ensure_seedbox_port_publishes_when_already_converged() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port },
        );
        let _ = handle(
            &mut machine,
            MamEvent::SeedboxUpdated {
                registered_ip: None,
                registered_asn: None,
                registered_as: None,
            },
        );

        let out = machine.handle_command(
            Instant::now(),
            chrono::Utc::now(),
            MamCommand::EnsureSeedboxPort { port },
        );

        assert!(out.actions.is_empty());
        assert_eq!(out.publishes, vec![MamPublish::SeedboxPortReady { port }]);
    }

    // ── upload_health_ok predicate tests (§26) ────────────────────────────────

    #[test]
    fn upload_health_ok_false_when_ratio_and_buffer_both_good() {
        let mut m = machine();
        m.ratio = 3.0;
        m.upload_credit_bytes = 30 * 1024 * 1024 * 1024;
        assert!(m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_false_when_ratio_bad() {
        let mut m = machine();
        m.ratio = 1.5;
        m.upload_credit_bytes = 30 * 1024 * 1024 * 1024;
        assert!(!m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_false_when_buffer_bad() {
        let mut m = machine();
        m.ratio = 3.0;
        m.upload_credit_bytes = 0;
        assert!(!m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_false_when_both_bad() {
        let mut m = machine();
        m.ratio = 0.5;
        m.upload_credit_bytes = 0;
        assert!(!m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_freeleech_true_ignores_ratio_when_buffer_ok() {
        let mut m = machine();
        m.ratio = 0.5; // below min_global_ratio
        m.upload_credit_bytes = 30 * 1024 * 1024 * 1024;
        // freeleech bypasses ratio requirement
        assert!(m.upload_health_ok(true));
    }

    #[test]
    fn upload_health_ok_freeleech_false_when_buffer_bad_even_with_good_ratio() {
        let mut m = machine();
        m.ratio = 5.0;
        m.upload_credit_bytes = 0; // below min_upload_buffer_bytes
        assert!(!m.upload_health_ok(true));
    }

    // ── StatusFetched upload-health publish tests (§26) ───────────────────────

    #[test]
    fn status_fetched_bad_ratio_emits_upload_health_degraded_with_ratio_ok_false() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 1.5,
                upload_credit_bytes: 30 * 1024 * 1024 * 1024,
            },
        );
        let degraded = out
            .publishes
            .iter()
            .find(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }));
        assert!(
            degraded.is_some(),
            "must emit UploadHealthDegraded when ratio is bad"
        );
        if let Some(MamPublish::UploadHealthDegraded {
            ratio_ok,
            buffer_ok,
            ..
        }) = degraded
        {
            assert!(!ratio_ok, "ratio_ok must be false when ratio < min");
            assert!(*buffer_ok, "buffer_ok must be true when buffer >= min");
        }
    }

    #[test]
    fn status_fetched_bad_buffer_emits_upload_health_degraded_with_buffer_ok_false() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 3.0,
                upload_credit_bytes: 0,
            },
        );
        let degraded = out
            .publishes
            .iter()
            .find(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }));
        assert!(
            degraded.is_some(),
            "must emit UploadHealthDegraded when buffer is bad"
        );
        if let Some(MamPublish::UploadHealthDegraded {
            ratio_ok,
            buffer_ok,
            ..
        }) = degraded
        {
            assert!(*ratio_ok, "ratio_ok must be true when ratio >= min");
            assert!(!buffer_ok, "buffer_ok must be false when buffer < min");
        }
    }

    #[test]
    fn status_fetched_good_health_emits_no_upload_health_degraded() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 3.0,
                upload_credit_bytes: 50 * 1024 * 1024 * 1024,
            },
        );
        let degraded_count = out
            .publishes
            .iter()
            .filter(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
            .count();
        assert_eq!(
            degraded_count, 0,
            "must not emit UploadHealthDegraded when health is ok"
        );
    }

    #[test]
    fn status_fetched_both_bad_emits_one_upload_health_degraded_with_both_flags_false() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 0.5,
                upload_credit_bytes: 0,
            },
        );
        let degraded_count = out
            .publishes
            .iter()
            .filter(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
            .count();
        assert_eq!(
            degraded_count, 1,
            "must emit exactly one UploadHealthDegraded when both bad"
        );
        if let Some(MamPublish::UploadHealthDegraded {
            ratio_ok,
            buffer_ok,
            ..
        }) = out
            .publishes
            .iter()
            .find(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
        {
            assert!(!ratio_ok, "ratio_ok must be false");
            assert!(!buffer_ok, "buffer_ok must be false");
        }
    }

    // ── KeepAlive heartbeat tests (§27) ───────────────────────────────────────

    #[test]
    fn keep_alive_timer_emits_fetch_and_reschedules() {
        let mut machine = machine();
        // Arm the chain via AuthSucceeded; consume its actions.
        let _ = handle(&mut machine, MamEvent::AuthSucceeded);

        let out = handle(&mut machine, MamEvent::TimerFired(MamTimer::KeepAlive));

        assert_eq!(
            out.actions,
            vec![
                MamAction::FetchStatus,
                MamAction::ScheduleTimer {
                    timer: MamTimer::KeepAlive,
                    after: Duration::from_secs(300),
                },
            ]
        );
        assert!(out.publishes.is_empty());
    }

    #[test]
    fn second_auth_success_does_not_arm_second_keep_alive_chain() {
        let mut machine = machine();

        let first = handle(&mut machine, MamEvent::AuthSucceeded);
        assert_eq!(
            first
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        ..
                    }
                ))
                .count(),
            1,
            "first AuthSucceeded must arm KeepAlive"
        );

        let second = handle(&mut machine, MamEvent::AuthSucceeded);
        assert_eq!(
            second
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        ..
                    }
                ))
                .count(),
            0,
            "second AuthSucceeded must NOT arm a second KeepAlive chain"
        );
    }

    #[test]
    fn keep_alive_degraded_fires_on_third_consecutive_failure_only() {
        let mut machine = machine();

        let first = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "boom".to_string(),
            },
        );
        let second = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "boom".to_string(),
            },
        );
        let third = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "third".to_string(),
            },
        );
        let fourth = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "fourth".to_string(),
            },
        );

        let degraded_count = |out: &Outcome<MamAction, MamPublish>| {
            out.publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
                .count()
        };

        assert_eq!(degraded_count(&first), 0);
        assert_eq!(degraded_count(&second), 0);
        assert_eq!(
            degraded_count(&third),
            1,
            "rising edge fires exactly once on threshold-crossing failure"
        );
        assert_eq!(
            degraded_count(&fourth),
            0,
            "no re-publish while still over threshold"
        );

        if let Some(MamPublish::KeepAliveDegraded {
            consecutive_failures,
            last_reason,
        }) = third
            .publishes
            .iter()
            .find(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
        {
            assert_eq!(*consecutive_failures, 3);
            assert_eq!(last_reason, "third");
        } else {
            panic!("third failure must publish KeepAliveDegraded");
        }
    }

    #[test]
    fn status_fetched_resets_failure_counter_and_rearms_rising_edge() {
        let mut machine = machine();
        for _ in 0..3 {
            let _ = handle(
                &mut machine,
                MamEvent::StatusFailed {
                    reason: "x".to_string(),
                },
            );
        }
        assert!(machine.consecutive_status_failures() >= 3);

        let _ = handle(
            &mut machine,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 3.0,
                upload_credit_bytes: 50 * 1024 * 1024 * 1024,
            },
        );
        assert_eq!(machine.consecutive_status_failures(), 0);

        // Burn down to the threshold again; rising edge must fire a second time.
        let _ = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "y".to_string(),
            },
        );
        let _ = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "y".to_string(),
            },
        );
        let third = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "y".to_string(),
            },
        );
        let degraded_count = third
            .publishes
            .iter()
            .filter(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
            .count();
        assert_eq!(
            degraded_count, 1,
            "rising edge must fire again after a reset"
        );
    }

    #[test]
    fn all_three_failure_kinds_count_toward_keep_alive_threshold() {
        let mut machine = machine();
        let _ = handle(
            &mut machine,
            MamEvent::AuthFailed {
                reason: "auth".to_string(),
            },
        );
        let _ = handle(
            &mut machine,
            MamEvent::SeedboxUpdateFailed {
                reason: "seedbox".to_string(),
            },
        );
        let third = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "status".to_string(),
            },
        );

        let degraded = third
            .publishes
            .iter()
            .find(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }));
        assert!(
            degraded.is_some(),
            "mixed failures must accumulate toward threshold"
        );
        if let Some(MamPublish::KeepAliveDegraded {
            consecutive_failures,
            last_reason,
        }) = degraded
        {
            assert_eq!(*consecutive_failures, 3);
            assert_eq!(last_reason, "status");
        }
    }

    #[test]
    fn keep_alive_threshold_zero_disables_degraded_publish() {
        let mut machine = MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 0,
                stale_registration_interval: Duration::from_secs(86_400),
                seedbox_update_min_interval: Duration::ZERO,
            },
            Instant::now(),
        );
        for _ in 0..10 {
            let out = handle(
                &mut machine,
                MamEvent::StatusFailed {
                    reason: "x".to_string(),
                },
            );
            assert!(
                !out.publishes
                    .iter()
                    .any(|p| matches!(p, MamPublish::KeepAliveDegraded { .. })),
                "threshold=0 must never publish KeepAliveDegraded"
            );
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{ExternalCause, Machine, Timed};
    use windlass_types::{VpnIp, VpnPort};

    use crate::{
        AsnState, MamAction, MamCommand, MamConfig, MamEvent, MamMachine, MamPublish, MamTimer,
    };

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    /// Ratio constrained to `0.0..=10.0` to avoid NaN/Infinity, which are
    /// pathological inputs the parse boundary already rejects.
    fn any_ratio() -> impl Strategy<Value = f64> {
        (0u32..=1000u32).prop_map(|n| f64::from(n) / 100.0)
    }

    /// Buffer constrained to `0..=(100 GiB)`.
    fn any_buffer() -> impl Strategy<Value = u64> {
        0u64..=(100 * 1024 * 1024 * 1024u64)
    }

    fn any_mam_config() -> impl Strategy<Value = MamConfig> {
        (
            any_ratio(),
            any_buffer(),
            // 1..=600s keep-alive cadence covers the realistic range without
            // making the timer constant explode in failure-burst proptests.
            1u64..=600u64,
            0u32..=10u32,
            1u64..=86_400u64,
        )
            .prop_map(
                |(
                    min_global_ratio,
                    min_upload_buffer_bytes,
                    keep_alive_secs,
                    keep_alive_failure_threshold,
                    stale_secs,
                )| MamConfig {
                    status_retry: Duration::from_secs(5),
                    min_global_ratio,
                    min_upload_buffer_bytes,
                    keep_alive_interval: Duration::from_secs(keep_alive_secs),
                    keep_alive_failure_threshold,
                    stale_registration_interval: Duration::from_secs(stale_secs),
                    seedbox_update_min_interval: Duration::ZERO,
                },
            )
    }

    // Fully-arbitrary state, including unreachable field combinations: the tested
    // invariants are total.
    fn any_asn_state() -> impl Strategy<Value = AsnState> {
        prop_oneof![
            Just(AsnState::Unknown),
            Just(AsnState::Accepted),
            Just(AsnState::Mismatched),
        ]
    }

    fn any_mam_machine() -> impl Strategy<Value = MamMachine> {
        (
            (
                any_mam_config(),
                any::<bool>(),
                proptest::option::of(any_vpn_port()),
                proptest::option::of(any_vpn_port()),
                any_ratio(),
                any_buffer(),
                any::<bool>(),
                0u32..=20u32,
                any_asn_state(),
            ),
            (proptest::option::of(any::<[u8; 4]>()), any::<bool>()),
        )
            .prop_map(
                |(
                    (
                        config,
                        authenticated,
                        seedbox_port,
                        desired_seedbox_port,
                        ratio,
                        upload_credit_bytes,
                        keep_alive_scheduled,
                        consecutive_status_failures,
                        asn_state,
                    ),
                    (observed_ip_bytes, stale_chain_scheduled),
                )| {
                    let mut machine = MamMachine::new(config, Instant::now());
                    machine.authenticated = authenticated;
                    machine.seedbox_port = seedbox_port;
                    machine.desired_seedbox_port = desired_seedbox_port;
                    machine.ratio = ratio;
                    machine.upload_credit_bytes = upload_credit_bytes;
                    machine.keep_alive_scheduled = keep_alive_scheduled;
                    machine.consecutive_status_failures = consecutive_status_failures;
                    machine.asn_state = asn_state;
                    machine.observed_ip =
                        observed_ip_bytes.map(|b| VpnIp(std::net::Ipv4Addr::from(b)));
                    machine.stale_chain_scheduled = stale_chain_scheduled;
                    machine
                },
            )
    }

    fn any_mam_event() -> impl Strategy<Value = MamEvent> {
        prop_oneof![
            Just(MamEvent::Init),
            Just(MamEvent::AuthSucceeded),
            any::<String>().prop_map(|reason| MamEvent::AuthFailed { reason }),
            (
                any::<bool>(),
                proptest::option::of(any_vpn_port()),
                any_ratio(),
                any_buffer(),
            )
                .prop_map(|(connectable, seedbox_port, ratio, upload_credit_bytes)| {
                    MamEvent::StatusFetched {
                        connectable,
                        seedbox_port,
                        ratio,
                        upload_credit_bytes,
                    }
                }),
            any::<String>().prop_map(|reason| MamEvent::StatusFailed { reason }),
            any::<String>().prop_map(|reason| MamEvent::Unreachable { reason }),
            any::<[u8; 4]>().prop_map(|b| MamEvent::AsnMismatch {
                ip: VpnIp(std::net::Ipv4Addr::from(b)),
            }),
            Just(MamEvent::SeedboxUpdated {
                registered_ip: None,
                registered_asn: None,
                registered_as: None,
            }),
            any::<String>().prop_map(|reason| MamEvent::SeedboxUpdateFailed { reason }),
            (0u64..=3600).prop_map(|s| MamEvent::RateLimited {
                retry_after: Duration::from_secs(s)
            }),
            Just(MamEvent::TimerFired(MamTimer::StatusRetry)),
            Just(MamEvent::TimerFired(MamTimer::RateLimitExpired)),
            Just(MamEvent::TimerFired(MamTimer::KeepAlive)),
            Just(MamEvent::TimerFired(MamTimer::StaleRegistrationRefresh)),
        ]
    }

    proptest! {
        // GLOBAL-1 (no panic).
        #[test]
        fn handle_never_panics(mut machine in any_mam_machine(), event in any_mam_event()) {
            let _ = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));
        }

        // MAM-1 (Guarantee C): every published SeedboxPortReady carries a port
        // that agrees with the desired target (or there is no desired target).
        #[test]
        fn seedbox_port_ready_matches_desired(
            mut machine in any_mam_machine(),
            event in any_mam_event(),
        ) {
            let out = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));
            for publish in &out.publishes {
                if let MamPublish::SeedboxPortReady { port } = publish {
                    prop_assert!(
                        machine.desired_seedbox_port.is_none()
                            || machine.desired_seedbox_port == Some(*port)
                    );
                }
            }
        }

        // MAM-2 (Guarantee F): a retryable failure schedules exactly one backed-off
        // StatusRetry and publishes its kind-specific publish — never an
        // immediate retry action.  §27 adds: failures may also publish
        // KeepAliveDegraded on the rising edge.  §28 generalises this from
        // "always Unavailable" to "Unavailable for Auth/Status/Seedbox
        // failures, Unreachable for transport-level Unreachable".
        #[test]
        fn failures_schedule_one_status_retry(
            mut machine in any_mam_machine(),
            reason in any::<String>(),
        ) {
            // Auth/Status/Seedbox failures publish Unavailable.
            for event in [
                MamEvent::AuthFailed { reason: reason.clone() },
                MamEvent::StatusFailed { reason: reason.clone() },
                MamEvent::SeedboxUpdateFailed { reason: reason.clone() },
            ] {
                let out = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));
                prop_assert_eq!(out.actions.len(), 1);
                let is_status_retry = matches!(
                    out.actions[0],
                    MamAction::ScheduleTimer { timer: MamTimer::StatusRetry, .. }
                );
                prop_assert!(is_status_retry);
                let unavailable_count = out
                    .publishes
                    .iter()
                    .filter(|p| matches!(p, MamPublish::Unavailable { .. }))
                    .count();
                prop_assert_eq!(unavailable_count, 1);
            }
            // §28 / MAM-11: Unreachable publishes Unreachable, not Unavailable,
            // but still schedules exactly one StatusRetry.
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::Unreachable { reason }),
            );
            prop_assert_eq!(out.actions.len(), 1);
            let is_status_retry = matches!(
                out.actions[0],
                MamAction::ScheduleTimer { timer: MamTimer::StatusRetry, .. }
            );
            prop_assert!(is_status_retry);
            let unreachable_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::Unreachable { .. }))
                .count();
            prop_assert_eq!(unreachable_count, 1);
            let unavailable_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::Unavailable { .. }))
                .count();
            prop_assert_eq!(
                unavailable_count, 0,
                "Unreachable must not also publish Unavailable"
            );
        }

        // MAM-7 [safety] (upload-health alert — §26):
        // `StatusFetched` publishes `UploadHealthDegraded` iff
        // `!upload_health_ok(freeleech=false)`.  The published `ratio_ok` and
        // `buffer_ok` flags are consistent with the configured thresholds.
        // Total invariant.
        #[test]
        fn upload_health_degraded_iff_not_upload_health_ok(
            mut machine in any_mam_machine(),
            connectable in any::<bool>(),
            seedbox_port in proptest::option::of(any_vpn_port()),
            ratio in any_ratio(),
            upload_credit_bytes in any_buffer(),
        ) {
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::StatusFetched {
                    connectable,
                    seedbox_port,
                    ratio,
                    upload_credit_bytes,
                }),
            );

            // After handle, self.ratio and self.upload_credit_bytes are updated.
            let expected_health_ok = machine.upload_health_ok(false);
            let degraded_publishes: Vec<_> = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
                .collect();

            if expected_health_ok {
                prop_assert!(
                    degraded_publishes.is_empty(),
                    "upload_health_ok(false)=true must produce no UploadHealthDegraded"
                );
            } else {
                prop_assert_eq!(
                    degraded_publishes.len(),
                    1,
                    "upload_health_ok(false)=false must produce exactly one UploadHealthDegraded"
                );
                // Check flag consistency.
                if let MamPublish::UploadHealthDegraded {
                    ratio: r,
                    upload_credit_bytes: b,
                    ratio_ok,
                    buffer_ok,
                } = degraded_publishes[0]
                {
                    prop_assert_eq!(
                        *ratio_ok,
                        *r >= machine.config.min_global_ratio,
                        "ratio_ok must be consistent with the threshold"
                    );
                    prop_assert_eq!(
                        *buffer_ok,
                        *b >= machine.config.min_upload_buffer_bytes,
                        "buffer_ok must be consistent with the threshold"
                    );
                }
            }
        }

        // MAM-13 [safety] (§29): the positive counterpart to MAM-7.
        // `StatusFetched` publishes exactly one `UploadHealthOk` iff
        // `upload_health_ok(false)` is true, and zero otherwise.  Combined
        // with MAM-7 the two are mutually exclusive: every StatusFetched
        // publishes exactly one of {UploadHealthOk, UploadHealthDegraded}.
        // Total invariant.
        #[test]
        fn upload_health_ok_iff_upload_health_predicate(
            mut machine in any_mam_machine(),
            connectable in any::<bool>(),
            seedbox_port in proptest::option::of(any_vpn_port()),
            ratio in any_ratio(),
            upload_credit_bytes in any_buffer(),
        ) {
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::StatusFetched {
                    connectable,
                    seedbox_port,
                    ratio,
                    upload_credit_bytes,
                }),
            );
            let expected_ok = machine.upload_health_ok(false);
            let ok_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::UploadHealthOk { .. }))
                .count();
            let degraded_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
                .count();
            if expected_ok {
                prop_assert_eq!(ok_count, 1,
                    "upload_health_ok=true must publish exactly one UploadHealthOk");
                prop_assert_eq!(degraded_count, 0,
                    "upload_health_ok=true must publish zero UploadHealthDegraded");
            } else {
                prop_assert_eq!(ok_count, 0,
                    "upload_health_ok=false must publish zero UploadHealthOk");
                prop_assert_eq!(degraded_count, 1,
                    "upload_health_ok=false must publish exactly one UploadHealthDegraded");
            }
        }

        // MAM-8 [safety] (§27): AuthSucceeded schedules KeepAlive at most once
        // per machine lifetime.  Repeated AuthSucceeded events never produce a
        // second KeepAlive ScheduleTimer.  Tested against a machine whose
        // keep_alive_scheduled flag is randomly true or false.
        #[test]
        fn keep_alive_chain_starts_at_most_once(mut machine in any_mam_machine()) {
            let was_scheduled = machine.keep_alive_scheduled();
            let out = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::AuthSucceeded));
            let scheduled = out
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer { timer: MamTimer::KeepAlive, .. }
                ))
                .count();
            if was_scheduled {
                prop_assert_eq!(scheduled, 0,
                    "no second KeepAlive ScheduleTimer when chain already armed");
            } else {
                prop_assert_eq!(scheduled, 1,
                    "AuthSucceeded must arm KeepAlive exactly once");
            }
            // After handling AuthSucceeded the chain is always considered armed.
            prop_assert!(machine.keep_alive_scheduled());
        }

        // MAM-9 [liveness] (§27): TimerFired(KeepAlive) always emits exactly
        // one FetchStatus action and exactly one KeepAlive re-schedule action,
        // for any machine state.  The chain cannot die from a single handler
        // step.
        #[test]
        fn keep_alive_timer_always_reschedules(mut machine in any_mam_machine()) {
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::TimerFired(MamTimer::KeepAlive)),
            );
            let fetch_count = out
                .actions
                .iter()
                .filter(|a| matches!(a, MamAction::FetchStatus))
                .count();
            let reschedule_count = out
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer { timer: MamTimer::KeepAlive, .. }
                ))
                .count();
            prop_assert_eq!(fetch_count, 1, "must emit exactly one FetchStatus");
            prop_assert_eq!(reschedule_count, 1, "must re-arm KeepAlive exactly once");
            prop_assert!(out.publishes.is_empty(),
                "KeepAlive timer is side-effect-free on publishes");
        }

        // MAM-10 [safety] (§27): a retryable failure publishes
        // KeepAliveDegraded iff this event's bump crosses the configured
        // threshold from below.  Total invariant — holds for any starting
        // counter, including ones already over the threshold.
        #[test]
        fn keep_alive_degraded_publishes_iff_rising_edge(
            mut machine in any_mam_machine(),
            reason in any::<String>(),
            which in 0u8..3,
        ) {
            let before = machine.consecutive_status_failures();
            let threshold = machine.config.keep_alive_failure_threshold;

            let event = match which {
                0 => MamEvent::AuthFailed { reason: reason.clone() },
                1 => MamEvent::StatusFailed { reason: reason.clone() },
                _ => MamEvent::SeedboxUpdateFailed { reason: reason.clone() },
            };
            let out = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));

            let after = machine.consecutive_status_failures();
            // The counter advances by exactly 1 unless saturated at u32::MAX.
            prop_assert!(after == before.saturating_add(1));

            let degraded_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
                .count();

            let expected_publish = threshold > 0 && before < threshold && after >= threshold;
            if expected_publish {
                prop_assert_eq!(degraded_count, 1,
                    "rising edge must publish exactly one KeepAliveDegraded");
                if let Some(MamPublish::KeepAliveDegraded {
                    consecutive_failures, last_reason,
                }) = out.publishes.iter().find(|p| matches!(p, MamPublish::KeepAliveDegraded { .. })) {
                    prop_assert_eq!(*consecutive_failures, after);
                    prop_assert_eq!(last_reason, &reason);
                }
            } else {
                prop_assert_eq!(degraded_count, 0,
                    "no KeepAliveDegraded outside the rising-edge transition");
            }
        }

        // MAM-11 [safety] (§28): the `Unreachable` event publishes exactly
        // one `MamPublish::Unreachable { reason }` and zero
        // `MamPublish::NotConnectable`.  Total invariant — `NotConnectable`
        // belongs strictly to `StatusFetched { connectable: false }`.
        #[test]
        fn unreachable_event_publishes_unreachable_not_notconnectable(
            mut machine in any_mam_machine(),
            reason in any::<String>(),
        ) {
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::Unreachable { reason: reason.clone() }),
            );
            let unreachable_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::Unreachable { .. }))
                .count();
            let notconnectable_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::NotConnectable { .. }))
                .count();
            prop_assert_eq!(unreachable_count, 1);
            prop_assert_eq!(notconnectable_count, 0);
            if let Some(MamPublish::Unreachable { reason: r }) = out
                .publishes
                .iter()
                .find(|p| matches!(p, MamPublish::Unreachable { .. }))
            {
                prop_assert_eq!(r, &reason);
            }
        }

        // MAM-12 [safety] (§28): `StatusFetched { connectable: false }`
        // publishes exactly one `NotConnectable` and zero `Unreachable`.
        // Total invariant — `Unreachable` belongs strictly to the
        // `Unreachable` event.
        #[test]
        fn status_fetched_not_connectable_publishes_notconnectable_not_unreachable(
            mut machine in any_mam_machine(),
            seedbox_port in proptest::option::of(any_vpn_port()),
            ratio in any_ratio(),
            upload_credit_bytes in any_buffer(),
        ) {
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::StatusFetched {
                    connectable: false,
                    seedbox_port,
                    ratio,
                    upload_credit_bytes,
                }),
            );
            let notconnectable_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::NotConnectable { .. }))
                .count();
            let unreachable_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::Unreachable { .. }))
                .count();
            prop_assert_eq!(notconnectable_count, 1);
            prop_assert_eq!(unreachable_count, 0);
        }

        // MAM-14 [safety] (§30): rising-edge AsnMismatch publish.  When an
        // `AsnMismatch` event arrives while `asn_state != Mismatched`, the
        // machine publishes exactly one `AsnMismatch { ip }` and transitions
        // to `Mismatched`.  Subsequent mismatches while already in
        // `Mismatched` publish zero.  Total invariant.
        #[test]
        fn asn_mismatch_publishes_on_rising_edge_only(
            mut machine in any_mam_machine(),
            ip_bytes in any::<[u8; 4]>(),
        ) {
            let pre = machine.asn_state();
            let ip = VpnIp(std::net::Ipv4Addr::from(ip_bytes));
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::AsnMismatch { ip }),
            );
            prop_assert_eq!(machine.asn_state(), AsnState::Mismatched);
            let mismatch_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::AsnMismatch { .. }))
                .count();
            if pre == AsnState::Mismatched {
                prop_assert_eq!(mismatch_count, 0,
                    "no re-publish when already in Mismatched");
            } else {
                prop_assert_eq!(mismatch_count, 1,
                    "rising edge must publish exactly one AsnMismatch");
            }
        }

        // MAM-15 [safety] (§30): rising-edge AsnAccepted publish.  When a
        // `SeedboxUpdated` event arrives while `asn_state != Accepted`, the
        // machine publishes exactly one `AsnAccepted` and transitions to
        // `Accepted`.  Subsequent updates while already in `Accepted`
        // publish zero.  Total invariant.
        #[test]
        fn asn_accepted_publishes_on_rising_edge_only(
            mut machine in any_mam_machine(),
        ) {
            let pre = machine.asn_state();
            let out = machine.handle(
                Instant::now(),
            chrono::Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, MamEvent::SeedboxUpdated {
                    registered_ip: None,
                    registered_asn: None,
                    registered_as: None,
                }),
            );
            prop_assert_eq!(machine.asn_state(), AsnState::Accepted);
            let accepted_count = out
                .publishes
                .iter()
                .filter(|p| matches!(p, MamPublish::AsnAccepted))
                .count();
            if pre == AsnState::Accepted {
                prop_assert_eq!(accepted_count, 0,
                    "no re-publish when already in Accepted");
            } else {
                prop_assert_eq!(accepted_count, 1,
                    "rising edge must publish exactly one AsnAccepted");
            }
        }

        // MAM-16 [safety] (§31): ObservedIpChanged dedups against the
        // last observed IP.  When ip == observed_ip the command emits
        // zero actions; otherwise it emits one UpdateSeedbox and (on
        // first observation) arms the StaleRegistrationRefresh chain.
        #[test]
        fn observed_ip_changed_dedups_against_held_observed_ip(
            mut machine in any_mam_machine(),
            ip_bytes in any::<[u8; 4]>(),
        ) {
            let ip = VpnIp(std::net::Ipv4Addr::from(ip_bytes));
            let pre_observed = machine.observed_ip();
            let pre_chain = machine.stale_chain_scheduled;
            let out = machine.handle_command(
                Instant::now(),
            chrono::Utc::now(),
                MamCommand::ObservedIpChanged { ip },
            );
            let update_count = out.actions.iter()
                .filter(|a| matches!(a, MamAction::UpdateSeedbox))
                .count();
            let schedule_count = out.actions.iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::StaleRegistrationRefresh, ..
                    }
                ))
                .count();
            if pre_observed == Some(ip) {
                prop_assert_eq!(update_count, 0,
                    "ObservedIpChanged must dedup when ip matches");
                prop_assert_eq!(schedule_count, 0);
            } else {
                prop_assert_eq!(update_count, 1,
                    "fresh observed IP must emit one UpdateSeedbox");
                if pre_chain {
                    prop_assert_eq!(schedule_count, 0,
                        "stale chain already armed → no re-schedule");
                } else {
                    prop_assert_eq!(schedule_count, 1,
                        "first observation arms the stale-registration timer");
                }
                prop_assert!(machine.stale_chain_scheduled);
                prop_assert_eq!(machine.observed_ip(), Some(ip));
            }
        }

        // MAM-18 [safety] (§32): Mousehole-style registered_ip dedup.
        // `ObservedIpChanged { ip }` skips `UpdateSeedbox` when MAM has
        // already recorded this IP (`registered_ip == Some(ip)`), even if
        // `observed_ip` differs.  Total.
        #[test]
        fn observed_ip_changed_dedups_against_registered_ip(
            mut machine in any_mam_machine(),
            ip_bytes in any::<[u8; 4]>(),
        ) {
            let ip = VpnIp(std::net::Ipv4Addr::from(ip_bytes));
            // Force registered_ip == ip so the dedup must fire.
            machine.registered_ip = Some(ip);
            let out = machine.handle_command(
                Instant::now(),
            chrono::Utc::now(),
                MamCommand::ObservedIpChanged { ip },
            );
            let update_count = out.actions.iter()
                .filter(|a| matches!(a, MamAction::UpdateSeedbox))
                .count();
            prop_assert_eq!(update_count, 0,
                "MAM-18: skip UpdateSeedbox when MAM already has this IP");
        }

        // MAM-19 [safety] (§32): SeedboxUpdated stores the registered
        // IP/ASN/AS reported by MAM, overwriting prior state.  Honest
        // `None` carry-through: missing fields don't clobber prior values.
        #[test]
        fn seedbox_updated_records_registered_meta(
            mut machine in any_mam_machine(),
            new_ip_bytes in proptest::option::of(any::<[u8; 4]>()),
            new_asn in proptest::option::of(any::<u32>()),
            new_as in proptest::option::of(any::<String>()),
        ) {
            let new_ip = new_ip_bytes
                .map(|b| VpnIp(std::net::Ipv4Addr::from(b)));
            // §32 dedup baseline mirrors `observed_ip` (what we sent),
            // independent of MAM's echoed `registered_ip`.
            let observed = machine.observed_ip;
            let pre_registered_asn = machine.registered_asn;
            let pre_registered_as = machine.registered_as.clone();
            machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown,
                MamEvent::SeedboxUpdated {
                    registered_ip: new_ip,
                    registered_asn: new_asn,
                    registered_as: new_as.clone(),
                },
            ));
            // registered_ip tracks what we registered (observed_ip),
            // NOT MAM's echo.  ASN/AS still come from the echo (None =
            // no fresh info → keep the old value).
            let expected_asn = if new_asn.is_some() { new_asn } else { pre_registered_asn };
            let expected_as = if new_as.is_some() { new_as } else { pre_registered_as };
            prop_assert_eq!(machine.registered_ip, observed);
            prop_assert_eq!(machine.registered_asn, expected_asn);
            prop_assert_eq!(machine.registered_as, expected_as);
        }

        // MAM-17 [safety] (§31): TimerFired(StaleRegistrationRefresh)
        // always emits exactly one UpdateSeedbox + re-schedules.  Total.
        #[test]
        fn stale_refresh_timer_always_reschedules(
            mut machine in any_mam_machine(),
        ) {
            let out = machine.handle(Instant::now(), chrono::Utc::now(), Timed::external(Instant::now(), ExternalCause::Unknown,
                MamEvent::TimerFired(MamTimer::StaleRegistrationRefresh),
            ));
            let update_count = out.actions.iter()
                .filter(|a| matches!(a, MamAction::UpdateSeedbox))
                .count();
            let reschedule_count = out.actions.iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::StaleRegistrationRefresh, ..
                    }
                ))
                .count();
            prop_assert_eq!(update_count, 1);
            prop_assert_eq!(reschedule_count, 1);
        }
    }
}
