#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::{VpnIp, VpnPort};

// `VpnConfig` is no longer `Copy` (§35 adds a `Vec<String>` of dependent
// names) but stays cloneable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnConfig {
    pub health_poll_interval: Duration,
    pub unhealthy_poll_interval: Duration,
    pub port_read_retry_interval: Duration,
    /// §31: cadence of the self-perpetuating ifconfig.co verification
    /// timer. Default: 6 hours.
    pub public_ip_verify_interval: Duration,
    /// §31: number of consecutive verification failures before publishing
    /// `PublicIpVerificationDegraded`. Default: 3.
    pub public_ip_verify_failure_threshold: u32,
    /// §35: container names of every dependent that shares Gluetun's
    /// network namespace (e.g. `qbittorrent`, `mlm`).  Used by the stale-
    /// namespace check.  An empty list disables the §35 orchestration
    /// invariants (useful for tests).
    pub dependent_names: Vec<String>,
    /// §35: maximum number of `RestartContainer` actions emitted within
    /// `restart_window_duration` before the circuit breaker trips and
    /// further restart actions are blocked.  Default: 3.
    pub max_restarts_per_window: u32,
    /// §35: sliding-window duration for the restart circuit breaker.
    /// Default: 10 minutes.
    pub restart_window_duration: Duration,
}

/// §31: VPN verification payload from ifconfig.co/json.
///
/// Only `ip` is currently consumed by the rest of the machine; the other
/// fields (`asn`, `country`, `hostname`) ride along for logging and the
/// future ASN-aware dedup story.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedIpInfo {
    pub ip: VpnIp,
    pub asn: Option<String>,
    pub country: Option<String>,
    pub hostname: Option<String>,
}

/// §35: per-dependent container state tracked by the VPN core for the
/// stale-namespace and premature-start invariants.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependentState {
    /// When the container was last observed to have started (from
    /// Docker's `State.StartedAt` field).  `None` before the first
    /// successful inspection.
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `true` once the dependent has been observed to have started at or
    /// after the current `gluetun.healthy_since`.  Reset to `false` on
    /// every Gluetun-healthy transition until the next inspection
    /// confirms a fresh start.
    pub network_trusted: bool,
}

/// §33: which external check produced a `PublicIpMismatch`.
///
/// `IfConfigCo` is the §31 source: ifconfig.co/json reports the public IP
/// the open internet sees us as.  `MamJsonIp` is the §33 source: MAM's
/// own `/json/jsonIp.php` reports the IP MAM sees us coming from.  The two
/// usually agree, but when they diverge the alert names the source so the
/// operator can tell a public-internet edge case from a MAM-compliance
/// problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationSource {
    IfConfigCo,
    MamJsonIp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnCommand {
    StartMonitoring,
    RefreshState,
    ReadForwardedPort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnTimer {
    HealthPoll,
    PortReadRetry,
    /// §31: self-perpetuating ifconfig.co verification cadence (default 6h).
    PublicIpVerify,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnEvent {
    Init,
    ContainerHealthy,
    ContainerUnhealthy,
    PortFileChanged {
        port: VpnPort,
    },
    /// §31: the Gluetun-managed IP file has a new value.  Gluetun writes
    /// this instantly on every VPN-state change and deletes the file when
    /// disconnected (see `PublicIpFileUnavailable`).
    PublicIpFromFile {
        ip: VpnIp,
    },
    /// §31: Gluetun deleted the IP file — the VPN is disconnected.
    /// The shell sends this when the file disappears so the core can
    /// clear `observed_ip` without waiting for `ContainerUnhealthy`.
    PublicIpFileUnavailable,
    /// §31: a `VerifyPublicIp` action completed and ifconfig.co returned
    /// usable data.
    PublicIpVerified {
        info: VerifiedIpInfo,
    },
    /// §31: a `VerifyPublicIp` action could not produce a result.  Treated
    /// as transient up to `public_ip_verify_failure_threshold`, then
    /// surfaces `PublicIpVerificationDegraded`.
    PublicIpVerifyFailed {
        reason: String,
    },
    /// §33: a `VerifyMamIp` action returned a usable response from MAM's
    /// `/json/jsonIp.php` endpoint.  Same shape as `PublicIpVerified` —
    /// the core compares the IP against `observed_ip` and publishes
    /// `PublicIpMismatch { source: MamJsonIp }` on disagreement.
    MamIpVerified {
        info: VerifiedIpInfo,
    },
    /// §33: a `VerifyMamIp` action could not produce a result.  Same
    /// failure-counter semantics as `PublicIpVerifyFailed` but tracked
    /// independently so a per-source `MamIpVerificationDegraded` publish
    /// can fire even when ifconfig.co is healthy.
    MamIpVerifyFailed {
        reason: String,
    },
    /// §35: Docker `inspect` reported a dependent's `StartedAt`
    /// timestamp.  `None` means the container is not running.
    DependentInspected {
        name: String,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    StateRead {
        connected: bool,
        port: Option<VpnPort>,
    },
    StateReadFailed {
        reason: String,
    },
    TimerFired(VpnTimer),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnAction {
    InspectContainer,
    ReadPortFiles,
    StartMonitoring,
    /// §31: ask the VPN shell to perform an HTTP verification of the
    /// current public IP through the Gluetun proxy (default endpoint:
    /// ifconfig.co/json).
    VerifyPublicIp,
    /// §33: ask the VPN shell to call MAM's `/json/jsonIp.php` through
    /// the Gluetun proxy.  Fired in parallel with `VerifyPublicIp` so the
    /// 6h timer covers both verification sources.
    VerifyMamIp,
    /// §35: ask the shell to inspect a dependent container's
    /// `StartedAt`.  The shell maps this to a Docker inspect call and
    /// answers with `DependentInspected`.
    InspectDependent {
        name: String,
    },
    /// §35: restart a dependent container.  Only emitted when the
    /// circuit breaker (`restart_window`) permits.  Used for the
    /// stale-namespace recovery path.
    RestartContainer {
        name: String,
    },
    /// §35: write a crash dump for the current incident.  Suppressed
    /// after the first emission per incident (`crash_dump_emitted_for_
    /// current_incident`).
    WriteCrashDump {
        incident_id: u64,
    },
    ScheduleTimer {
        timer: VpnTimer,
        after: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnPublish {
    Connected,
    Disconnected,
    PortReady {
        port: VpnPort,
    },
    PortUnavailable,
    /// §31: published on the rising edge of an observed-IP change, sourced
    /// from the Gluetun-managed file.  The domain forwards this to the
    /// MAM core's `ObservedIpChanged` command.
    PublicIpObserved {
        ip: VpnIp,
    },
    /// §31: Gluetun deleted the IP file or the VPN is disconnected.
    /// Clears `admission.vpn_ip_compliant` in the domain.
    PublicIpUnavailable,
    /// §31 + §33: a verification source reports a different IP than
    /// Gluetun's file — a potential leak.  `source` names which check
    /// disagreed so the operator can tell ifconfig.co edge cases from
    /// MAM-compliance problems.  Flips the §29 `vpn_ip_compliant` gate
    /// to `Some(false)` and fires a `Critical` alert.
    PublicIpMismatch {
        file_ip: VpnIp,
        verified_ip: VpnIp,
        source: VerificationSource,
    },
    /// §31: ifconfig.co verification has failed at least
    /// `public_ip_verify_failure_threshold` consecutive times.
    /// Surfaces as a `Warning` alert without blocking admission.
    PublicIpVerificationDegraded {
        consecutive_failures: u32,
        last_reason: String,
    },
    /// §33: MAM `/json/jsonIp.php` verification has failed at least
    /// `public_ip_verify_failure_threshold` consecutive times.
    /// Surfaces as a `Warning` alert without blocking admission —
    /// independent of `PublicIpVerificationDegraded` so we can tell
    /// "ifconfig.co flaky" from "MAM unreachable from us".
    MamIpVerificationDegraded {
        consecutive_failures: u32,
        last_reason: String,
    },
    /// §35: a dependent container's `started_at` predates the current
    /// Gluetun `healthy_since` — its network namespace is stale and
    /// traffic from it may be leaking outside the VPN.  Domain routes
    /// this to a `Critical` alert and the §29 admission gate.
    DependentNetworkUntrusted {
        name: String,
        dependent_started_at: chrono::DateTime<chrono::Utc>,
        gluetun_healthy_since: chrono::DateTime<chrono::Utc>,
    },
    /// §35: the restart circuit breaker tripped — `max_restarts_per_
    /// window` restarts have been emitted within
    /// `restart_window_duration`.  Further `RestartContainer` actions
    /// are blocked until the window slides.  Domain routes to a
    /// `Critical` alert.
    RestartStorm {
        window_count: u32,
        max: u32,
    },
    /// §35: a dependent's `started_at` is now at-or-after
    /// `gluetun.healthy_since` — the namespace is trusted again.
    /// Domain uses this to clear the per-dependent admission block.
    DependentNetworkTrusted {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnTopic {
    Connectivity,
    Port,
    /// §31: public-IP observation + verification topic.
    PublicIp,
    /// §35: Gluetun stack-orchestration topic.
    Orchestration,
}

impl HasTopic<VpnTopic> for VpnPublish {
    fn topic(&self) -> VpnTopic {
        match self {
            Self::Connected | Self::Disconnected => VpnTopic::Connectivity,
            Self::PortReady { .. } | Self::PortUnavailable => VpnTopic::Port,
            Self::PublicIpObserved { .. }
            | Self::PublicIpUnavailable
            | Self::PublicIpMismatch { .. }
            | Self::PublicIpVerificationDegraded { .. }
            | Self::MamIpVerificationDegraded { .. } => VpnTopic::PublicIp,
            Self::DependentNetworkUntrusted { .. }
            | Self::DependentNetworkTrusted { .. }
            | Self::RestartStorm { .. } => VpnTopic::Orchestration,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnResponse {
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpnMachine {
    config: VpnConfig,
    connected: bool,
    port: Option<VpnPort>,
    /// §31: last IP from the Gluetun file.  `None` when disconnected or
    /// before the first file read.
    observed_ip: Option<VpnIp>,
    /// §31: last IP reported by ifconfig.co verification.  Used to detect
    /// file-vs-verification mismatches.
    last_verified_ip: Option<VpnIp>,
    /// §31: consecutive ifconfig.co failure count.  Reset on the next
    /// successful verification.
    verification_failures: u32,
    /// §31: once true, the self-perpetuating `PublicIpVerify` timer chain
    /// is running.  Armed on the first verification attempt.
    verify_chain_scheduled: bool,
    /// §33: last IP reported by MAM's `/json/jsonIp.php` verification.
    last_mam_verified_ip: Option<VpnIp>,
    /// §33: consecutive MAM-side failure count.  Tracked independently
    /// from `verification_failures` so per-source degraded signals are
    /// possible (ifconfig.co healthy ⇄ MAM unreachable, or vice versa).
    mam_verification_failures: u32,
    /// §35: timestamp at which Gluetun transitioned to healthy.  Reset
    /// whenever Gluetun goes unhealthy.  Compared against
    /// `DependentState::started_at` for the stale-namespace check.
    healthy_since: Option<chrono::DateTime<chrono::Utc>>,
    /// §35: per-dependent container state.  Populated from
    /// `DependentInspected` events.
    dependents: HashMap<String, DependentState>,
    /// §35: sliding-window of recent `RestartContainer` action times for
    /// the circuit breaker.  Times older than `restart_window_duration`
    /// are pruned on every check.
    restart_window: VecDeque<chrono::DateTime<chrono::Utc>>,
    /// §35: identifier for the current incident (a contiguous run of
    /// problems triggered by the same Gluetun-unhealthy event).  Bumps on
    /// every Gluetun-unhealthy → healthy transition so the dedup flag
    /// gets a fresh allowance per incident.
    incident_id: u64,
    /// §35: `true` once the current incident has produced a
    /// `WriteCrashDump` action, suppressing further dumps until the next
    /// incident.  Reset when `incident_id` bumps.
    crash_dump_emitted_for_current_incident: bool,
}

impl VpnMachine {
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    #[must_use]
    pub const fn port(&self) -> Option<VpnPort> {
        self.port
    }

    #[must_use]
    pub const fn observed_ip(&self) -> Option<VpnIp> {
        self.observed_ip
    }

    #[must_use]
    pub const fn last_verified_ip(&self) -> Option<VpnIp> {
        self.last_verified_ip
    }

    /// Returns the most recent IP MAM's `/json/jsonIp.php` reported (§33).
    #[must_use]
    pub const fn last_mam_verified_ip(&self) -> Option<VpnIp> {
        self.last_mam_verified_ip
    }

    /// Returns when Gluetun was last observed to transition to healthy
    /// (§35).  `None` until the first healthy transition or after an
    /// unhealthy reset.
    #[must_use]
    pub const fn healthy_since(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.healthy_since
    }

    /// Returns the current incident id (§35).  Bumped on every
    /// Gluetun-unhealthy → healthy transition.
    #[must_use]
    pub const fn incident_id(&self) -> u64 {
        self.incident_id
    }

    /// Returns per-dependent state (§35), keyed by container name.
    #[must_use]
    pub const fn dependents(&self) -> &HashMap<String, DependentState> {
        &self.dependents
    }
}

impl Machine for VpnMachine {
    type Config = VpnConfig;
    type Event = VpnEvent;
    type Action = VpnAction;
    type Publish = VpnPublish;
    type Topic = VpnTopic;
    type Command = VpnCommand;
    type Response = VpnResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            connected: false,
            port: None,
            observed_ip: None,
            last_verified_ip: None,
            verification_failures: 0,
            verify_chain_scheduled: false,
            last_mam_verified_ip: None,
            mam_verification_failures: 0,
            healthy_since: None,
            dependents: HashMap::new(),
            restart_window: VecDeque::new(),
            incident_id: 0,
            crash_dump_emitted_for_current_incident: false,
        }
    }

    // §31 added several new arms (file-IP, verification, mismatch, timer);
    // the function is long because the event set grew, not because any arm
    // is complex.
    #[allow(clippy::too_many_lines)]
    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            VpnEvent::Init => Outcome {
                actions: vec![VpnAction::StartMonitoring, VpnAction::InspectContainer],
                publish: Vec::new(),
            },
            VpnEvent::ContainerHealthy => {
                let was_unhealthy = !self.connected;
                self.connected = true;
                // §35: rising-edge unhealthy → healthy.  Record
                // `healthy_since`, bump the incident id (so the next
                // crash-dump dedup gets a fresh allowance), reset the
                // dump flag, and reset per-dependent trust until the
                // next inspection.
                if was_unhealthy {
                    self.healthy_since = Some(chrono::Utc::now());
                    self.incident_id = self.incident_id.saturating_add(1);
                    self.crash_dump_emitted_for_current_incident = false;
                    for state in self.dependents.values_mut() {
                        state.network_trusted = false;
                    }
                }
                let mut actions = vec![
                    VpnAction::ReadPortFiles,
                    VpnAction::ScheduleTimer {
                        timer: VpnTimer::HealthPoll,
                        after: self.config.health_poll_interval,
                    },
                ];
                // §35: ask the shell to inspect every dependent so the
                // stale-namespace check can run.
                for name in &self.config.dependent_names {
                    actions.push(VpnAction::InspectDependent { name: name.clone() });
                }
                Outcome {
                    actions,
                    publish: vec![VpnPublish::Connected],
                }
            }
            VpnEvent::ContainerUnhealthy => {
                self.connected = false;
                self.port = None;
                let mut publish = vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable];
                // §31: clear observed_ip on disconnect; surface the
                // unavailable signal so the domain can clear the §29 gate.
                if self.observed_ip.is_some() {
                    self.observed_ip = None;
                    publish.push(VpnPublish::PublicIpUnavailable);
                }
                self.last_verified_ip = None;
                self.verification_failures = 0;
                self.last_mam_verified_ip = None;
                self.mam_verification_failures = 0;
                // §35: clear healthy_since — every dependent's network
                // is now considered untrusted until the next healthy
                // transition and inspection.  Don't drop the dependents
                // map (we still want their last-known started_at) but
                // mark them untrusted.
                self.healthy_since = None;
                for state in self.dependents.values_mut() {
                    state.network_trusted = false;
                }
                Outcome {
                    actions: vec![VpnAction::ScheduleTimer {
                        timer: VpnTimer::HealthPoll,
                        after: self.config.unhealthy_poll_interval,
                    }],
                    publish,
                }
            }
            VpnEvent::PortFileChanged { port } => {
                self.port = Some(port);
                Outcome {
                    actions: Vec::new(),
                    publish: vec![VpnPublish::PortReady { port }],
                }
            }
            VpnEvent::StateRead { connected, port } => {
                self.connected = connected;
                if connected {
                    self.port = port;
                    let port_publish = port.map_or(VpnPublish::PortUnavailable, |p| {
                        VpnPublish::PortReady { port: p }
                    });
                    Outcome {
                        actions: Vec::new(),
                        publish: vec![VpnPublish::Connected, port_publish],
                    }
                } else {
                    // A disconnected VPN never holds a forwarded port, regardless
                    // of what the shell reports. Mirror ContainerUnhealthy (VPN-1).
                    self.port = None;
                    let mut publish = vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable];
                    if self.observed_ip.is_some() {
                        self.observed_ip = None;
                        publish.push(VpnPublish::PublicIpUnavailable);
                    }
                    self.last_verified_ip = None;
                    self.verification_failures = 0;
                    Outcome {
                        actions: Vec::new(),
                        publish,
                    }
                }
            }
            VpnEvent::StateReadFailed { .. } => Outcome {
                actions: vec![VpnAction::ScheduleTimer {
                    timer: VpnTimer::PortReadRetry,
                    after: self.config.port_read_retry_interval,
                }],
                publish: Vec::new(),
            },
            VpnEvent::TimerFired(VpnTimer::HealthPoll) => Outcome {
                actions: vec![VpnAction::InspectContainer],
                publish: Vec::new(),
            },
            VpnEvent::TimerFired(VpnTimer::PortReadRetry) => Outcome {
                actions: vec![VpnAction::ReadPortFiles],
                publish: Vec::new(),
            },
            // §31 / VPN-8: rising-edge IP-from-file signal.  When the file
            // value changes, publish `PublicIpObserved` and trigger an
            // immediate ifconfig.co verification.  Re-observing the same IP
            // is a no-op.  Also arms the self-perpetuating verification
            // timer the first time we ever see an IP.
            VpnEvent::PublicIpFromFile { ip } => {
                if self.observed_ip == Some(ip) {
                    return Outcome::none();
                }
                self.observed_ip = Some(ip);
                // §31 + §33: trigger immediate verification on both sources
                // after a fresh file IP.  The 6h timer continues to handle
                // the no-change case for both checks in parallel.
                let mut actions = vec![VpnAction::VerifyPublicIp, VpnAction::VerifyMamIp];
                if !self.verify_chain_scheduled {
                    self.verify_chain_scheduled = true;
                    actions.push(VpnAction::ScheduleTimer {
                        timer: VpnTimer::PublicIpVerify,
                        after: self.config.public_ip_verify_interval,
                    });
                }
                Outcome {
                    actions,
                    publish: vec![VpnPublish::PublicIpObserved { ip }],
                }
            }
            // §31: Gluetun deleted the IP file.  Clear observed_ip and the
            // per-source verification state; surface the unavailable
            // signal — same shape as a disconnect.
            VpnEvent::PublicIpFileUnavailable => {
                if self.observed_ip.is_none() {
                    return Outcome::none();
                }
                self.observed_ip = None;
                self.last_verified_ip = None;
                self.verification_failures = 0;
                self.last_mam_verified_ip = None;
                self.mam_verification_failures = 0;
                Outcome {
                    actions: Vec::new(),
                    publish: vec![VpnPublish::PublicIpUnavailable],
                }
            }
            // §31 / VPN-9: ifconfig.co verification succeeded.  Reset the
            // failure counter and compare with the file IP — mismatch is a
            // leak warning.
            VpnEvent::PublicIpVerified { info } => {
                self.verification_failures = 0;
                self.last_verified_ip = Some(info.ip);
                let mut publish = Vec::new();
                if let Some(file_ip) = self.observed_ip
                    && file_ip != info.ip
                {
                    publish.push(VpnPublish::PublicIpMismatch {
                        file_ip,
                        verified_ip: info.ip,
                        source: VerificationSource::IfConfigCo,
                    });
                }
                Outcome {
                    actions: Vec::new(),
                    publish,
                }
            }
            // §33 / VPN-11: MAM `/json/jsonIp.php` verification succeeded.
            // Same shape as `PublicIpVerified` but tracks the MAM-side
            // counter and publishes with `VerificationSource::MamJsonIp`
            // on disagreement.
            VpnEvent::MamIpVerified { info } => {
                self.mam_verification_failures = 0;
                self.last_mam_verified_ip = Some(info.ip);
                let mut publish = Vec::new();
                if let Some(file_ip) = self.observed_ip
                    && file_ip != info.ip
                {
                    publish.push(VpnPublish::PublicIpMismatch {
                        file_ip,
                        verified_ip: info.ip,
                        source: VerificationSource::MamJsonIp,
                    });
                }
                Outcome {
                    actions: Vec::new(),
                    publish,
                }
            }
            // §31 / VPN-10: rising-edge degraded publish on persistent
            // ifconfig.co failure.  Threshold is configurable; default 3.
            VpnEvent::PublicIpVerifyFailed { reason } => {
                let threshold = self.config.public_ip_verify_failure_threshold;
                let before = self.verification_failures;
                self.verification_failures = before.saturating_add(1);
                let mut publish = Vec::new();
                if threshold > 0 && before < threshold && self.verification_failures >= threshold {
                    publish.push(VpnPublish::PublicIpVerificationDegraded {
                        consecutive_failures: self.verification_failures,
                        last_reason: reason,
                    });
                }
                Outcome {
                    actions: Vec::new(),
                    publish,
                }
            }
            // §33 / VPN-12: same rising-edge pattern for the MAM source.
            // Counter is independent of `verification_failures` so a
            // per-source degraded signal can fire even when the other
            // source is healthy.
            VpnEvent::MamIpVerifyFailed { reason } => {
                let threshold = self.config.public_ip_verify_failure_threshold;
                let before = self.mam_verification_failures;
                self.mam_verification_failures = before.saturating_add(1);
                let mut publish = Vec::new();
                if threshold > 0
                    && before < threshold
                    && self.mam_verification_failures >= threshold
                {
                    publish.push(VpnPublish::MamIpVerificationDegraded {
                        consecutive_failures: self.mam_verification_failures,
                        last_reason: reason,
                    });
                }
                Outcome {
                    actions: Vec::new(),
                    publish,
                }
            }
            // §31 + §33: self-perpetuating verification heartbeat.  Fires
            // both checks per tick + re-schedules unconditionally so a
            // dropped response cannot kill the chain.
            VpnEvent::TimerFired(VpnTimer::PublicIpVerify) => Outcome {
                actions: vec![
                    VpnAction::VerifyPublicIp,
                    VpnAction::VerifyMamIp,
                    VpnAction::ScheduleTimer {
                        timer: VpnTimer::PublicIpVerify,
                        after: self.config.public_ip_verify_interval,
                    },
                ],
                publish: Vec::new(),
            },
            // §35 / VPN-13: stale-namespace detection.  A dependent
            // whose StartedAt predates Gluetun's healthy_since may be on
            // a stale network namespace; emit `RestartContainer` and
            // publish `DependentNetworkUntrusted`.  Circuit breaker
            // (VPN-15) blocks the restart action if the window cap is
            // reached.  Crash dump dedup (VPN-16) emits at most one
            // `WriteCrashDump` per incident.
            VpnEvent::DependentInspected { name, started_at } => {
                let entry = self.dependents.entry(name.clone()).or_default();
                entry.started_at = started_at;
                let Some(healthy_since) = self.healthy_since else {
                    // Gluetun isn't healthy → we have no anchor.  Update
                    // the recorded started_at but make no decision.
                    return Outcome::none();
                };
                let Some(dep_started_at) = started_at else {
                    // Dependent not running.  Trust flag stays false.
                    entry.network_trusted = false;
                    return Outcome::none();
                };
                if dep_started_at >= healthy_since {
                    // Fresh namespace — trust it.  Rising-edge publish.
                    let was_untrusted = !entry.network_trusted;
                    entry.network_trusted = true;
                    return Outcome {
                        actions: Vec::new(),
                        publish: if was_untrusted {
                            vec![VpnPublish::DependentNetworkTrusted { name }]
                        } else {
                            Vec::new()
                        },
                    };
                }
                // Stale namespace.  Mark untrusted and request restart
                // (if the circuit breaker allows).
                entry.network_trusted = false;
                let mut actions = Vec::new();
                let now = chrono::Utc::now();
                let window = chrono::Duration::from_std(self.config.restart_window_duration)
                    .unwrap_or_else(|_| chrono::Duration::seconds(0));
                while let Some(front) = self.restart_window.front() {
                    if now.signed_duration_since(*front) > window {
                        self.restart_window.pop_front();
                    } else {
                        break;
                    }
                }
                let max = self.config.max_restarts_per_window;
                let window_count = u32::try_from(self.restart_window.len()).unwrap_or(u32::MAX);
                let mut publish = vec![VpnPublish::DependentNetworkUntrusted {
                    name: name.clone(),
                    dependent_started_at: dep_started_at,
                    gluetun_healthy_since: healthy_since,
                }];
                if max == 0 || window_count < max {
                    actions.push(VpnAction::RestartContainer { name });
                    self.restart_window.push_back(now);
                } else {
                    // VPN-15: circuit breaker tripped.  Suppress the
                    // restart, publish RestartStorm, and emit a crash
                    // dump iff we haven't already this incident.
                    publish.push(VpnPublish::RestartStorm { window_count, max });
                    if !self.crash_dump_emitted_for_current_incident {
                        self.crash_dump_emitted_for_current_incident = true;
                        actions.push(VpnAction::WriteCrashDump {
                            incident_id: self.incident_id,
                        });
                    }
                }
                Outcome { actions, publish }
            }
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            VpnCommand::StartMonitoring => {
                vec![VpnAction::StartMonitoring, VpnAction::InspectContainer]
            }
            VpnCommand::RefreshState => vec![VpnAction::InspectContainer],
            VpnCommand::ReadForwardedPort => vec![VpnAction::ReadPortFiles],
        };
        Self::outcome(actions, VpnResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
    use windlass_types::VpnPort;

    use crate::{VpnAction, VpnCommand, VpnConfig, VpnEvent, VpnMachine, VpnPublish, VpnTimer};

    fn machine() -> VpnMachine {
        VpnMachine::new(
            VpnConfig {
                health_poll_interval: Duration::from_secs(2),
                unhealthy_poll_interval: Duration::from_millis(250),
                port_read_retry_interval: Duration::from_millis(500),
                public_ip_verify_interval: Duration::from_secs(6 * 60 * 60),
                public_ip_verify_failure_threshold: 3,
                dependent_names: Vec::new(),
                max_restarts_per_window: 3,
                restart_window_duration: Duration::from_mins(10),
            },
            Instant::now(),
        )
    }

    fn handle(machine: &mut VpnMachine, event: VpnEvent) -> Outcome<VpnAction, VpnPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn init_starts_monitoring_and_health_poll() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::Init);

        assert_eq!(
            out.actions,
            vec![VpnAction::StartMonitoring, VpnAction::InspectContainer]
        );
        assert!(out.publish.is_empty());
    }

    #[test]
    fn start_monitoring_command_matches_init_actions() {
        let mut machine = machine();

        let out = machine.handle_command(Instant::now(), VpnCommand::StartMonitoring);

        assert_eq!(
            out.actions,
            vec![VpnAction::StartMonitoring, VpnAction::InspectContainer]
        );
    }

    #[test]
    fn healthy_container_publishes_connected_and_reads_port_files() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::ContainerHealthy);

        assert!(machine.is_connected());
        assert_eq!(
            out.actions,
            vec![
                VpnAction::ReadPortFiles,
                VpnAction::ScheduleTimer {
                    timer: VpnTimer::HealthPoll,
                    after: Duration::from_secs(2),
                },
            ]
        );
        assert_eq!(out.publish, vec![VpnPublish::Connected]);
    }

    #[test]
    fn unhealthy_container_publishes_disconnected_and_schedules_fast_poll() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::ContainerUnhealthy);

        assert!(!machine.is_connected());
        assert_eq!(
            out.actions,
            vec![VpnAction::ScheduleTimer {
                timer: VpnTimer::HealthPoll,
                after: Duration::from_millis(250),
            }]
        );
        assert_eq!(
            out.publish,
            vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable]
        );
    }

    #[test]
    fn port_file_changed_publishes_port_ready() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(&mut machine, VpnEvent::PortFileChanged { port });

        assert_eq!(machine.port(), Some(port));
        assert_eq!(out.publish, vec![VpnPublish::PortReady { port }]);
    }

    #[test]
    fn state_read_failed_schedules_port_read_retry() {
        let mut machine = machine();

        let out = handle(
            &mut machine,
            VpnEvent::StateReadFailed {
                reason: "files not ready".to_string(),
            },
        );

        assert_eq!(
            out.actions,
            vec![VpnAction::ScheduleTimer {
                timer: VpnTimer::PortReadRetry,
                after: Duration::from_millis(500),
            }]
        );
        assert!(out.publish.is_empty());
    }

    #[test]
    fn port_read_retry_timer_fires_read_port_files() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::TimerFired(VpnTimer::PortReadRetry));

        assert_eq!(out.actions, vec![VpnAction::ReadPortFiles]);
        assert!(out.publish.is_empty());
    }

    #[test]
    fn health_poll_timer_inspects_container() {
        let mut machine = machine();

        let out = handle(&mut machine, VpnEvent::TimerFired(VpnTimer::HealthPoll));

        assert_eq!(out.actions, vec![VpnAction::InspectContainer]);
        assert!(out.publish.is_empty());
    }

    // StateRead four-shape tests (story 18 / VPN-4).

    #[test]
    fn state_read_connected_with_port_publishes_connected_and_port_ready() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: true,
                port: Some(port),
            },
        );

        assert!(machine.is_connected());
        assert_eq!(machine.port(), Some(port));
        assert_eq!(
            out.publish,
            vec![VpnPublish::Connected, VpnPublish::PortReady { port }]
        );
        assert!(out.actions.is_empty());
    }

    #[test]
    fn state_read_connected_without_port_publishes_connected_and_port_unavailable() {
        let mut machine = machine();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: true,
                port: None,
            },
        );

        assert!(machine.is_connected());
        assert_eq!(machine.port(), None);
        assert_eq!(
            out.publish,
            vec![VpnPublish::Connected, VpnPublish::PortUnavailable]
        );
        assert!(out.actions.is_empty());
    }

    #[test]
    fn state_read_disconnected_without_port_publishes_disconnected_and_port_unavailable() {
        let mut machine = machine();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: false,
                port: None,
            },
        );

        assert!(!machine.is_connected());
        assert_eq!(machine.port(), None);
        assert_eq!(
            out.publish,
            vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable]
        );
        assert!(out.actions.is_empty());
    }

    #[test]
    fn state_read_disconnected_with_port_clears_port_and_publishes_unavailable() {
        // Dishonest shell event: connected=false but port=Some(_). The machine
        // must defend: never advertise a port for a disconnected VPN (VPN-1/VPN-4).
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            VpnEvent::StateRead {
                connected: false,
                port: Some(port),
            },
        );

        assert!(!machine.is_connected());
        assert_eq!(
            machine.port(),
            None,
            "port must be cleared when disconnected"
        );
        assert_eq!(
            out.publish,
            vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable]
        );
        assert!(out.actions.is_empty());
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};
    use windlass_types::{VpnIp, VpnPort};

    use crate::{VerifiedIpInfo, VpnAction, VpnConfig, VpnEvent, VpnMachine, VpnPublish, VpnTimer};

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    // Fully-arbitrary machine state (every `connected × port` combination,
    // including ones a real event history would not reach). VPN-2 is a *total*
    // invariant, so it must hold even on unreachable states.
    fn any_vpn_machine() -> impl Strategy<Value = VpnMachine> {
        (
            any::<bool>(),
            proptest::option::of(any_vpn_port()),
            proptest::option::of(any_vpn_ip()),
            proptest::option::of(any_vpn_ip()),
            0u32..=10u32,
            any::<bool>(),
            proptest::option::of(any_vpn_ip()),
            0u32..=10u32,
        )
            .prop_map(
                |(
                    connected,
                    port,
                    observed_ip,
                    last_verified_ip,
                    verification_failures,
                    verify_chain_scheduled,
                    last_mam_verified_ip,
                    mam_verification_failures,
                )| {
                    let mut machine = VpnMachine::new(
                        VpnConfig {
                            health_poll_interval: Duration::from_secs(2),
                            unhealthy_poll_interval: Duration::from_millis(250),
                            port_read_retry_interval: Duration::from_millis(500),
                            public_ip_verify_interval: Duration::from_secs(6 * 60 * 60),
                            public_ip_verify_failure_threshold: 3,
                            dependent_names: Vec::new(),
                            max_restarts_per_window: 3,
                            restart_window_duration: Duration::from_mins(10),
                        },
                        Instant::now(),
                    );
                    machine.connected = connected;
                    machine.port = port;
                    machine.observed_ip = observed_ip;
                    machine.last_verified_ip = last_verified_ip;
                    machine.verification_failures = verification_failures;
                    machine.verify_chain_scheduled = verify_chain_scheduled;
                    machine.last_mam_verified_ip = last_mam_verified_ip;
                    machine.mam_verification_failures = mam_verification_failures;
                    machine
                },
            )
    }

    fn any_vpn_ip() -> impl Strategy<Value = VpnIp> {
        any::<[u8; 4]>().prop_map(|b| VpnIp(std::net::Ipv4Addr::from(b)))
    }

    fn any_verified_ip_info() -> impl Strategy<Value = VerifiedIpInfo> {
        (
            any_vpn_ip(),
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<String>()),
        )
            .prop_map(|(ip, asn, country, hostname)| VerifiedIpInfo {
                ip,
                asn,
                country,
                hostname,
            })
    }

    fn any_vpn_event() -> impl Strategy<Value = VpnEvent> {
        prop_oneof![
            Just(VpnEvent::Init),
            Just(VpnEvent::ContainerHealthy),
            Just(VpnEvent::ContainerUnhealthy),
            any_vpn_port().prop_map(|port| VpnEvent::PortFileChanged { port }),
            any_vpn_ip().prop_map(|ip| VpnEvent::PublicIpFromFile { ip }),
            Just(VpnEvent::PublicIpFileUnavailable),
            any_verified_ip_info().prop_map(|info| VpnEvent::PublicIpVerified { info }),
            any::<String>().prop_map(|reason| VpnEvent::PublicIpVerifyFailed { reason }),
            any_verified_ip_info().prop_map(|info| VpnEvent::MamIpVerified { info }),
            any::<String>().prop_map(|reason| VpnEvent::MamIpVerifyFailed { reason }),
            // §35: DependentInspected with arbitrary names + started_at.
            // The Utc::now() snapshot is fine since the predicates only
            // compare against healthy_since (also a chrono timestamp).
            (any::<String>(), proptest::option::of(0i64..=3_600i64)).prop_map(|(name, offset)| {
                VpnEvent::DependentInspected {
                    name,
                    started_at: offset.map(|s| chrono::Utc::now() + chrono::Duration::seconds(s)),
                }
            }),
            (any::<bool>(), proptest::option::of(any_vpn_port()))
                .prop_map(|(connected, port)| VpnEvent::StateRead { connected, port }),
            any::<String>().prop_map(|reason| VpnEvent::StateReadFailed { reason }),
            Just(VpnEvent::TimerFired(VpnTimer::HealthPoll)),
            Just(VpnEvent::TimerFired(VpnTimer::PortReadRetry)),
            Just(VpnEvent::TimerFired(VpnTimer::PublicIpVerify)),
        ]
    }

    proptest! {
        // GLOBAL-1 (no panic): handle tolerates any (state, event).
        #[test]
        fn handle_never_panics(mut machine in any_vpn_machine(), event in any_vpn_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // VPN-2 (Guarantee C): every published `PortReady` carries the port the
        // machine currently holds, and is only published when a port is held.
        #[test]
        fn published_port_ready_matches_held_port(
            mut machine in any_vpn_machine(),
            event in any_vpn_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for publish in &out.publish {
                if let VpnPublish::PortReady { port } = publish {
                    prop_assert_eq!(machine.port(), Some(*port));
                }
            }
        }

        // VPN-8 [safety] (§31): rising-edge `PublicIpObserved`.
        // `PublicIpFromFile { ip }` publishes exactly one
        // `PublicIpObserved` iff `pre.observed_ip != Some(ip)`, and zero
        // otherwise.  After the call, `observed_ip == Some(ip)`.  Total
        // invariant.
        #[test]
        fn public_ip_observed_publishes_on_rising_edge_only(
            mut machine in any_vpn_machine(),
            ip in any_vpn_ip(),
        ) {
            let pre = machine.observed_ip();
            let out = machine.handle(Instant::now(), Timed::now(
                VpnEvent::PublicIpFromFile { ip },
            ));
            prop_assert_eq!(machine.observed_ip(), Some(ip));
            let observed_count = out.publish.iter()
                .filter(|p| matches!(p, VpnPublish::PublicIpObserved { .. }))
                .count();
            if pre == Some(ip) {
                prop_assert_eq!(observed_count, 0);
            } else {
                prop_assert_eq!(observed_count, 1);
            }
        }

        // VPN-9 [safety] (§31): `PublicIpVerified` publishes
        // `PublicIpMismatch` iff the verified IP differs from the held
        // `observed_ip`, and `observed_ip` is set.  Total.
        #[test]
        fn public_ip_mismatch_publishes_iff_disagree(
            mut machine in any_vpn_machine(),
            info in any_verified_ip_info(),
        ) {
            let pre_observed = machine.observed_ip();
            let out = machine.handle(Instant::now(), Timed::now(
                VpnEvent::PublicIpVerified { info: info.clone() },
            ));
            let mismatch_count = out.publish.iter()
                .filter(|p| matches!(p, VpnPublish::PublicIpMismatch { .. }))
                .count();
            let should_mismatch = pre_observed.is_some()
                && pre_observed != Some(info.ip);
            if should_mismatch {
                prop_assert_eq!(mismatch_count, 1);
            } else {
                prop_assert_eq!(mismatch_count, 0);
            }
            // The post-state always records the verified IP.
            prop_assert_eq!(machine.last_verified_ip(), Some(info.ip));
        }

        // VPN-10 [safety] (§31): rising-edge
        // `PublicIpVerificationDegraded` publish on threshold-crossing
        // failure.  Total.
        #[test]
        fn public_ip_verification_degraded_on_rising_edge(
            mut machine in any_vpn_machine(),
            reason in any::<String>(),
        ) {
            let threshold = machine.config.public_ip_verify_failure_threshold;
            let pre = machine.verification_failures;
            let out = machine.handle(Instant::now(), Timed::now(
                VpnEvent::PublicIpVerifyFailed { reason },
            ));
            let degraded_count = out.publish.iter()
                .filter(|p| matches!(p, VpnPublish::PublicIpVerificationDegraded { .. }))
                .count();
            let crossed = threshold > 0
                && pre < threshold
                && pre.saturating_add(1) >= threshold;
            if crossed {
                prop_assert_eq!(degraded_count, 1);
            } else {
                prop_assert_eq!(degraded_count, 0);
            }
        }

        // §31: TimerFired(PublicIpVerify) always emits exactly one
        // VerifyPublicIp + re-schedules.  Total.
        #[test]
        fn public_ip_verify_timer_always_reschedules(
            mut machine in any_vpn_machine(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(
                VpnEvent::TimerFired(VpnTimer::PublicIpVerify),
            ));
            let verify_count = out.actions.iter()
                .filter(|a| matches!(a, VpnAction::VerifyPublicIp))
                .count();
            let reschedule_count = out.actions.iter()
                .filter(|a| matches!(
                    a,
                    VpnAction::ScheduleTimer { timer: VpnTimer::PublicIpVerify, .. }
                ))
                .count();
            prop_assert_eq!(verify_count, 1);
            prop_assert_eq!(reschedule_count, 1);
        }
    }
}
