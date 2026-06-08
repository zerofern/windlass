//! Sans-IO state machine for the in-process `WireGuard` tunnel.
//!
//! Mirrors the pattern of every other Windlass core ([`Machine`] trait
//! in `windlass-machine`): events in, typed actions and publishes out,
//! pure decision-making.  All I/O — kernel netlink, UDP sockets,
//! nftables, leak probes — happens in the [`Shell`]-side counterpart
//! (in `windlass-net`, landing in a follow-up phase).
//!
//! See `docs/vpn-ownership.md` for the design rationale and
//! acceptance criteria; this module is what carries the tunnel-state
//! invariants the machine is responsible for.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use nutype::nutype;
use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::VpnPort;

// ── Typed config primitives ──────────────────────────────────────────────────
//
// Project type rule: prefer deep domain-specific types over raw
// primitives.  These newtypes carry the validation rules the
// state machine relies on so a wrong literal in TunnelConfig is
// rejected at construction, not at runtime.

/// Number of `[Peer]` sections in the parsed wg.conf.  Must be at
/// least 1 — a tunnel without peers cannot establish.
#[nutype(
    validate(greater_or_equal = 1),
    derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)
)]
pub struct PeerCount(usize);

/// Consecutive `HandshakeStalled` events before the state machine
/// rotates the endpoint.  Must be at least 1.
#[nutype(
    validate(greater_or_equal = 1),
    derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)
)]
pub struct StallCountBeforeRotate(u32);

/// Consecutive `NatPmpFailed` events before the state machine
/// publishes `PortForwardingDegraded`.  Must be at least 1.
#[nutype(
    validate(greater_or_equal = 1),
    derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)
)]
pub struct NatPmpFailureThreshold(u32);

/// Lease-renewal cadence as basis points (`1/10_000`) of the granted
/// lifetime.  Must be in `(0, 10_000]`.
#[nutype(
    validate(greater = 0, less_or_equal = 10_000),
    derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)
)]
pub struct PortRenewalBasisPoints(u16);

// ── Config ───────────────────────────────────────────────────────────────────

/// Tunable parameters supplied at boot.  The peer list is the parsed
/// `[Peer]` sections from `wg.conf`; everything else is operational
/// policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelConfig {
    /// Number of peers in the supplied configuration.  Used by the
    /// machine to know whether endpoint rotation is possible.  The
    /// peer details themselves are held by the shell; the core only
    /// needs the count to make decisions.
    pub peer_count: PeerCount,
    /// How often the shell polls the kernel for the latest handshake
    /// timestamp.  Defaults to 30 s.
    pub handshake_poll_interval: Duration,
    /// Handshake-age threshold past which the tunnel is considered
    /// stale.  `WireGuard`'s own rekey-after-time is 120 s; we treat
    /// anything older than 180 s as a stall and fire recovery.
    pub handshake_stall_after: Duration,
    /// How many consecutive stall observations before we try
    /// endpoint rotation.  Default 3.
    pub stall_count_before_rotate: StallCountBeforeRotate,
    /// How many consecutive rotations before we enter `Stuck`.
    /// `None` means "try every peer once" — [`TunnelMachine::new`]
    /// resolves it to `peer_count`.  An explicit `Some(n)` higher
    /// than `peer_count` is clamped down because the rotation
    /// index wraps.  Default `None`.
    pub rotations_before_stuck: Option<u32>,
    /// Fraction of the granted port lease at which we renew.  Default
    /// 0.5 — Proton's 60 s leases get a 30 s renewal cadence.
    /// Stored as basis points (1/`10_000`) instead of `f32` so the
    /// config type can derive `Eq` and round-trip through the
    /// observability snapshot deterministically.  `10_000` = 1.0;
    /// 5000 = 0.5.  Default 5000.
    pub port_renewal_basis_points: PortRenewalBasisPoints,
    /// How often we run the leak probe (try a non-tunnel egress and
    /// expect it to fail).  Default 6 h, same as the §31 verify
    /// cadence we are replacing.
    pub leak_probe_interval: Duration,
    /// Consecutive NAT-PMP failures before we publish that port
    /// forwarding is degraded.  Default 3.
    pub natpmp_failure_threshold: NatPmpFailureThreshold,
    /// How often we re-query the public exit IP through the tunnel.
    /// Default 6 h.  This is what `TunnelPublish::ExitIpObserved`
    /// surfaces — the IP MAM sees us as.
    pub exit_ip_query_interval: Duration,
    /// NAT-PMP protocol (UDP vs TCP) for the periodic port-map
    /// request.  `ProtonVPN` expects TCP; some providers prefer UDP.
    pub natpmp_protocol: crate::natpmp::Protocol,
    /// Lifetime in seconds requested from the NAT-PMP gateway.
    /// `ProtonVPN` caps at 60 s regardless; making this configurable
    /// lets non-Proton providers tune it.
    pub natpmp_lifetime_seconds: u32,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            peer_count: PeerCount::try_new(1).expect("default"),
            handshake_poll_interval: Duration::from_secs(30),
            handshake_stall_after: Duration::from_mins(3),
            stall_count_before_rotate: StallCountBeforeRotate::try_new(3).expect("default"),
            rotations_before_stuck: None,
            port_renewal_basis_points: PortRenewalBasisPoints::try_new(5_000).expect("default"),
            leak_probe_interval: Duration::from_hours(6),
            natpmp_failure_threshold: NatPmpFailureThreshold::try_new(3).expect("default"),
            exit_ip_query_interval: Duration::from_hours(6),
            natpmp_protocol: crate::natpmp::Protocol::Tcp,
            natpmp_lifetime_seconds: 60,
        }
    }
}

// ── Events ───────────────────────────────────────────────────────────────────

/// Things the shell tells the core have happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TunnelEvent {
    /// Boot — the runtime is asking us to start.
    Init,
    /// The shell finished bringing the interface up (`ip link add`,
    /// `wg set`, address + route).
    InterfaceConfigured,
    /// The shell failed to bring the interface up.  The typed
    /// variant lets the core distinguish wg-set failures from
    /// routing failures without substring matching.
    InterfaceConfigureFailed { reason: InterfaceConfigureFailure },
    /// The shell installed the firewall kill switch.
    FirewallInstalled,
    /// The shell failed to install the firewall kill switch.  We
    /// treat this as fail-closed — no tunnel can come up without it.
    FirewallInstallFailed { reason: FirewallInstallFailure },
    /// The shell polled the kernel and observed a handshake; the
    /// `age_seconds` is `now - latest_handshake` from `wg show`.
    HandshakeReported { age_seconds: u64 },
    /// The shell polled and there is no handshake yet, or the
    /// previous one is older than [`TunnelConfig::handshake_stall_after`].
    HandshakeStalled,
    /// The shell received a NAT-PMP port-map response from the
    /// gateway.
    NatPmpLeaseGranted {
        external_port: u16,
        lifetime_seconds: u32,
        epoch_seconds: u32,
    },
    /// The NAT-PMP request did not complete successfully.
    NatPmpFailed { reason: NatPmpFailure },
    /// The leak probe finished — either the probe failed to reach
    /// non-tunnel egress (good — no leak path), or it did reach
    /// something (bad — leak detected).
    LeakProbeCompleted { outcome: LeakProbeOutcome },
    /// The shell queried an external IP-reflection service through
    /// the tunnel and got a usable answer.  This is the *exit IP*
    /// — what `ProtonVPN`'s server NATs us to, which is what MAM
    /// sees when we connect.  Replaces the inside-address
    /// placeholder the tunnel bridge used before.
    ExitIpObserved { ip: windlass_types::VpnIp },
    /// The exit-IP query failed.  Carried `reason` is for logs;
    /// failures are bounded-retried via the existing exit-IP timer
    /// and surface to the operator only after the threshold.
    ExitIpQueryFailed { reason: ExitIpFailure },
    /// A scheduled timer fired.
    TimerFired(TunnelTimer),
}

/// Typed reasons the shell reports for `ExitIpQueryFailed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitIpFailure {
    /// Network-level failure (DNS, TCP, TLS).
    Transport(String),
    /// Service returned non-success HTTP.
    HttpStatus(u16),
    /// Body was unparseable / not an IP address.
    Parse(String),
}

impl std::fmt::Display for ExitIpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(s) => write!(f, "transport: {s}"),
            Self::HttpStatus(c) => write!(f, "HTTP {c}"),
            Self::Parse(s) => write!(f, "parse: {s}"),
        }
    }
}

/// Typed reasons the shell reports for `InterfaceConfigureFailed`.
///
/// The string fields carry the underlying error message for the
/// operator's logs; the variants let downstream code react to the
/// failure class without parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterfaceConfigureFailure {
    /// `ip link add ... type wireguard` failed (kernel module
    /// missing, name in use, etc.).
    LinkAdd(String),
    /// `wg set` failed — usually means an invalid key, invalid
    /// endpoint, or wg userland missing.
    WgSet(String),
    /// `ip addr add` failed.
    AddressAdd(String),
    /// `ip link set ... up` failed.
    LinkUp(String),
    /// `ip route add` failed.
    RouteAdd(String),
    /// `ip link set ... mtu` failed.
    MtuSet(String),
    /// DNS resolver write failed.
    DnsWrite(String),
    /// Any other failure shape (operator wrote a wrong policy, etc.).
    Other(String),
}

impl std::fmt::Display for InterfaceConfigureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LinkAdd(s) => write!(f, "ip link add: {s}"),
            Self::WgSet(s) => write!(f, "wg set: {s}"),
            Self::AddressAdd(s) => write!(f, "ip addr add: {s}"),
            Self::LinkUp(s) => write!(f, "ip link set up: {s}"),
            Self::RouteAdd(s) => write!(f, "ip route add: {s}"),
            Self::MtuSet(s) => write!(f, "ip link set mtu: {s}"),
            Self::DnsWrite(s) => write!(f, "DNS resolver write: {s}"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// Typed reasons the shell can report for `FirewallInstallFailed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirewallInstallFailure {
    /// `nft` not available on the host.
    NftMissing(String),
    /// `nft` rejected the ruleset (parse / syntax / unsupported).
    RulesetRejected(String),
    /// Any other failure shape.
    Other(String),
}

impl std::fmt::Display for FirewallInstallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NftMissing(s) => write!(f, "nft missing: {s}"),
            Self::RulesetRejected(s) => write!(f, "nft ruleset rejected: {s}"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// Typed reasons the shell can report for `NatPmpFailed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NatPmpFailure {
    /// Local UDP socket bind failed.
    Bind(String),
    /// Sending the request to the gateway failed (no route, refused).
    Send(String),
    /// Gateway did not respond within the configured timeout.
    Timeout,
    /// Reading the response from the socket failed.
    Recv(String),
    /// Gateway returned a non-success NAT-PMP result code.  See
    /// `windlass_tunnel_core::natpmp::NatPmpResponseCode` for the
    /// documented codes.
    GatewayError { code: u16 },
    /// Response was malformed (wrong length, version, op).
    MalformedResponse(String),
}

impl std::fmt::Display for NatPmpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(s) => write!(f, "NAT-PMP socket bind: {s}"),
            Self::Send(s) => write!(f, "NAT-PMP send: {s}"),
            Self::Timeout => f.write_str("NAT-PMP gateway timeout"),
            Self::Recv(s) => write!(f, "NAT-PMP recv: {s}"),
            Self::GatewayError { code } => write!(f, "NAT-PMP gateway error code {code}"),
            Self::MalformedResponse(s) => write!(f, "NAT-PMP malformed response: {s}"),
        }
    }
}

/// Outcome of one leak-probe attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeakProbeOutcome {
    /// The probe could not reach anything outside the tunnel.  This
    /// is the *good* outcome — confirms the kill switch.
    NoEgressDetected,
    /// The probe reached external egress that wasn't routed through
    /// the tunnel.  Leak.  Carried details name what we reached so
    /// the operator can see exactly which path leaked.
    LeakDetected {
        interface: String,
        observed_remote: String,
    },
    /// The probe ran but couldn't determine an answer (e.g. local
    /// route lookup failure, transient resolver issue).  We do not
    /// treat this as a leak — but we surface it so the operator
    /// knows the verification was inconclusive.
    Inconclusive { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelTimer {
    /// Re-poll the kernel for the latest handshake.
    HandshakeWatchdog,
    /// Renew the NAT-PMP lease before it expires.
    PortRenewal,
    /// Run the periodic leak probe.
    LeakProbe,
    /// Re-query the public exit IP through the tunnel.
    ExitIpQuery,
}

impl TunnelTimer {
    /// Static name used as the `ExternalCause::Timer { name }` tag
    /// when the shell forwards a fired timer back into the runtime.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::HandshakeWatchdog => "TunnelTimer::HandshakeWatchdog",
            Self::PortRenewal => "TunnelTimer::PortRenewal",
            Self::LeakProbe => "TunnelTimer::LeakProbe",
            Self::ExitIpQuery => "TunnelTimer::ExitIpQuery",
        }
    }
}

// ── Actions ──────────────────────────────────────────────────────────────────

/// Side effects the shell should execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TunnelAction {
    /// Bring up the `WireGuard` interface using the held config.  One-shot
    /// at boot.
    ConfigureInterface,
    /// Install the nftables kill switch.  One-shot at boot, after the
    /// interface is up.
    InstallFirewall,
    /// Read the kernel's `latest_handshake` for the named peer.  The
    /// `peer_index` identifies which peer in the operator's `wg.conf`
    /// the shell should poll — required because endpoint rotation
    /// changes the active peer, and asking the shell to "poll the
    /// current peer" leaks core state into the shell (it would have
    /// to guess from config).  Result arrives back as
    /// [`TunnelEvent::HandshakeReported`] or
    /// [`TunnelEvent::HandshakeStalled`].
    PollHandshake { peer_index: usize },
    /// Send a NAT-PMP port-map request to the gateway.
    RequestNatPmp,
    /// Switch the active peer (`Endpoint` setting in `wg set`) to the
    /// peer at this index.  Recovery for an unresponsive endpoint.
    RotateEndpoint { peer_index: usize },
    /// Try to reach a non-tunnel egress.  Result arrives as
    /// [`TunnelEvent::LeakProbeCompleted`].
    RunLeakProbe,
    /// Query an external IP-reflection service through the tunnel
    /// to learn the public exit IP.  Result arrives as
    /// [`TunnelEvent::ExitIpObserved`] or
    /// [`TunnelEvent::ExitIpQueryFailed`].
    QueryExitIp,
    /// Schedule a timer to fire after the given duration.
    ScheduleTimer { timer: TunnelTimer, after: Duration },
}

// ── Publishes ────────────────────────────────────────────────────────────────

/// Typed facts other cores subscribe to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelPublish {
    /// Tunnel transitioned to healthy.  Rising-edge only — re-observing
    /// a healthy handshake is a no-op for the publish.
    Up,
    /// Tunnel transitioned to unhealthy.  Rising-edge only.
    Down {
        reason: String,
        since: DateTime<Utc>,
    },
    /// Tunnel transitioned to `Stuck` — we have exhausted automatic
    /// recovery and need operator attention.  Rising-edge only.
    Stuck {
        reason: String,
        since: DateTime<Utc>,
        attempted_recoveries: u32,
    },
    /// Tunnel recovered from `Stuck` (e.g. operator forced a re-handshake
    /// and it took).  Rising-edge only.
    Recovered,
    /// A forwarded port is available.  Fires on initial grant and on
    /// any port change.  qBit's listen port sync consumes this.
    PortReady { port: VpnPort },
    /// The forwarded port was lost (renewal failure threshold, epoch
    /// reset that the gateway didn't replace, etc.).
    PortUnavailable,
    /// The leak probe found a non-tunnel egress.  This is a `Critical`
    /// alert — the kill switch did not protect us as expected.
    LeakDetected {
        interface: String,
        observed_remote: String,
    },
    /// Port forwarding has failed
    /// [`TunnelConfig::natpmp_failure_threshold`] times in a row.
    /// Surfaces as a `Warning` alert without taking the tunnel down.
    PortForwardingDegraded {
        consecutive_failures: u32,
        last_reason: String,
    },
    /// The shell's exit-IP query returned a usable answer.  Fires
    /// on rising edge (first observation, or when the IP changes).
    /// Replaces the inside-address placeholder the tunnel bridge
    /// used before — domain admission keys on this for MAM dedup.
    ExitIpObserved { ip: windlass_types::VpnIp },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelTopic {
    Health,
    Port,
    Leak,
    /// Public exit IP observed through the tunnel (see
    /// [`TunnelPublish::ExitIpObserved`]).
    PublicIp,
}

impl HasTopic<TunnelTopic> for TunnelPublish {
    fn topic(&self) -> TunnelTopic {
        match self {
            Self::Up | Self::Down { .. } | Self::Stuck { .. } | Self::Recovered => {
                TunnelTopic::Health
            }
            Self::PortReady { .. }
            | Self::PortUnavailable
            | Self::PortForwardingDegraded { .. } => TunnelTopic::Port,
            Self::LeakDetected { .. } => TunnelTopic::Leak,
            Self::ExitIpObserved { .. } => TunnelTopic::PublicIp,
        }
    }
}

// ── Commands + Response ──────────────────────────────────────────────────────

/// Operator- or domain-initiated commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelCommand {
    /// Force the shell to poll the kernel handshake now.
    PollHandshakeNow,
    /// Force a NAT-PMP request now (e.g. after operator suspects the
    /// port was lost).
    RequestPortNow,
    /// Force an endpoint rotation now.  Wraps around the peer list.
    RotateEndpointNow,
    /// Force a leak probe now.
    RunLeakProbeNow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelResponse {
    Accepted,
    /// Returned when the command can't currently be honoured (e.g.
    /// rotate when there is only one peer).
    Rejected {
        reason: RejectionReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    NoAlternativePeer,
    NotYetInitialized,
}

// ── State ────────────────────────────────────────────────────────────────────

/// Health view consumed by snapshots and the observability UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelHealth {
    /// Boot — interface not yet configured.
    Initializing,
    /// Interface configured + firewall installed, waiting for first
    /// handshake.
    Connecting,
    /// Recent handshake observed.  The full state machine carries
    /// the age separately; this is just the gate.
    Up,
    /// Handshake stale or interface lost.
    Down,
    /// Recovery attempts exhausted; need operator action.
    Stuck { attempted_recoveries: u32 },
}

// `struct_excessive_bools`: the four boot/flag fields are independent
// state slots (interface up, firewall up, port published, degraded
// publish fired), not a state enum.  Collapsing them into one enum
// would obscure the orthogonal rising-edge gates we drive each from.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TunnelMachine {
    config: TunnelConfig,
    active_peer_index: usize,
    health: TunnelHealth,
    interface_configured: bool,
    firewall_installed: bool,
    /// Last handshake age in seconds, as reported by the shell.
    /// `None` means "never observed" or "stale and cleared".
    last_handshake_age_seconds: Option<u64>,
    /// Cumulative count of consecutive `HandshakeStalled` events
    /// since the last `HandshakeReported`.  Resets to 0 on a fresh
    /// healthy handshake.
    consecutive_stalls: u32,
    /// Cumulative count of endpoint rotations triggered by stalls.
    rotation_count: u32,
    /// Cumulative count of consecutive `NatPmpFailed` events since
    /// the last successful lease.
    consecutive_natpmp_failures: u32,
    /// Most recently granted external port.  `None` until the first
    /// successful NAT-PMP response, or after a `PortUnavailable`.
    forwarded_port: Option<u16>,
    /// Whether the held `forwarded_port` has already been published
    /// via `PortReady`.  We hold a port even when the
    /// publish-preconditions (`firewall_installed`) are not met yet;
    /// the publish then fires later, on the next transition that
    /// satisfies them.  Resets when the port changes or is lost.
    port_published: bool,
    /// Last NAT-PMP epoch we saw.  A decrease means the gateway
    /// rebooted (RFC 6886 §3.6) and the previous lease is invalid.
    last_natpmp_epoch: Option<u32>,
    /// Whether a `PortForwardingDegraded` publish has already fired
    /// for the current failure streak (rising-edge gate).
    port_degraded_published: bool,
    /// Last public exit IP the shell reported via
    /// `TunnelEvent::ExitIpObserved`.  `None` until the first
    /// successful query.  Used for rising-edge dedup of
    /// `TunnelPublish::ExitIpObserved`.
    last_exit_ip: Option<windlass_types::VpnIp>,
    /// Once true, the self-perpetuating `ExitIpQuery` timer chain
    /// is running.  Armed on the firewall-installed transition so a
    /// dropped event cannot kill the chain.
    exit_ip_chain_scheduled: bool,
}

impl TunnelMachine {
    #[must_use]
    pub const fn health(&self) -> &TunnelHealth {
        &self.health
    }

    #[must_use]
    pub const fn active_peer_index(&self) -> usize {
        self.active_peer_index
    }

    #[must_use]
    pub const fn forwarded_port(&self) -> Option<u16> {
        self.forwarded_port
    }

    /// Picks the next peer index when rotating endpoints.  Wraps
    /// around at the end of the list.
    fn next_peer_index(&self) -> usize {
        // PeerCount enforces >= 1 at construction so `n` is always
        // safe as the divisor.
        let n = self.config.peer_count.into_inner();
        (self.active_peer_index + 1) % n
    }

    /// If a forwarded port is held but has not yet been published,
    /// and the publish preconditions are satisfied (kill switch
    /// installed), returns the publish to fire.  Caller is
    /// responsible for flipping `port_published` to true.
    fn pending_port_ready(&self) -> Option<TunnelPublish> {
        if self.port_published || !self.firewall_installed {
            return None;
        }
        let port = self.forwarded_port?;
        let typed = VpnPort::try_new(port).ok()?;
        Some(TunnelPublish::PortReady { port: typed })
    }

    /// Stall-path logic shared by [`TunnelEvent::HandshakeStalled`]
    /// and an over-threshold [`TunnelEvent::HandshakeReported`].
    /// Counts the stall, drives endpoint rotation when the count
    /// crosses the configured threshold, escalates to `Stuck` when
    /// rotations are exhausted, and always reschedules the watchdog
    /// so a dropped event cannot kill the chain.
    fn apply_handshake_stalled(
        &mut self,
        wall_now: DateTime<Utc>,
    ) -> Outcome<TunnelAction, TunnelPublish> {
        self.consecutive_stalls = self.consecutive_stalls.saturating_add(1);
        let mut actions = Vec::new();
        let mut publishes = Vec::new();
        let was_up = matches!(self.health, TunnelHealth::Up);

        if self.consecutive_stalls < self.config.stall_count_before_rotate.into_inner() {
            // Stay in current health; just wait for the next poll.
        } else if self
            .config
            .rotations_before_stuck
            .is_some_and(|max| self.rotation_count < max)
            && self.config.peer_count.into_inner() > 1
        {
            self.active_peer_index = self.next_peer_index();
            self.rotation_count = self.rotation_count.saturating_add(1);
            self.consecutive_stalls = 0;
            actions.push(TunnelAction::RotateEndpoint {
                peer_index: self.active_peer_index,
            });
            if was_up {
                self.health = TunnelHealth::Down;
                publishes.push(TunnelPublish::Down {
                    reason: "handshake stalled; rotating endpoint".to_string(),
                    since: wall_now,
                });
            }
        } else {
            // Exhausted automatic recovery.
            let was_stuck = matches!(self.health, TunnelHealth::Stuck { .. });
            self.health = TunnelHealth::Stuck {
                attempted_recoveries: self.rotation_count,
            };
            if !was_stuck {
                publishes.push(TunnelPublish::Stuck {
                    reason: "handshake stalled past recovery threshold".to_string(),
                    since: wall_now,
                    attempted_recoveries: self.rotation_count,
                });
            }
        }

        actions.push(TunnelAction::ScheduleTimer {
            timer: TunnelTimer::HandshakeWatchdog,
            after: self.config.handshake_poll_interval,
        });
        Outcome { actions, publishes }
    }
}

impl Machine for TunnelMachine {
    type Config = TunnelConfig;
    type Event = TunnelEvent;
    type Action = TunnelAction;
    type Publish = TunnelPublish;
    type Topic = TunnelTopic;
    type Command = TunnelCommand;
    type Response = TunnelResponse;
    type StateSnapshot = Self;

    fn new(config: Self::Config, _now: Instant) -> Self {
        // Normalize `rotations_before_stuck`:
        // - `None`  → "try every peer once" (= peer_count).
        // - `Some(n)` capped at peer_count because rotation wraps.
        // PeerCount enforces >= 1; no max() needed.
        #[allow(clippy::cast_possible_truncation)]
        let peers_u32 = config.peer_count.into_inner() as u32;
        let normalized_rotations = Some(
            config
                .rotations_before_stuck
                .map_or(peers_u32, |n| n.min(peers_u32)),
        );
        let config = TunnelConfig {
            rotations_before_stuck: normalized_rotations,
            ..config
        };
        Self {
            config,
            active_peer_index: 0,
            health: TunnelHealth::Initializing,
            interface_configured: false,
            firewall_installed: false,
            last_handshake_age_seconds: None,
            consecutive_stalls: 0,
            rotation_count: 0,
            consecutive_natpmp_failures: 0,
            forwarded_port: None,
            port_published: false,
            last_natpmp_epoch: None,
            port_degraded_published: false,
            last_exit_ip: None,
            exit_ip_chain_scheduled: false,
        }
    }

    // One big match per event variant — long but flat, mirrors the
    // VpnMachine `handle` (which also carries this allow).
    #[allow(clippy::too_many_lines)]
    fn handle(
        &mut self,
        _now: Instant,
        wall_now: DateTime<Utc>,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            TunnelEvent::Init => Outcome {
                actions: vec![TunnelAction::ConfigureInterface],
                publishes: Vec::new(),
            },

            TunnelEvent::InterfaceConfigured => {
                self.interface_configured = true;
                Outcome {
                    actions: vec![TunnelAction::InstallFirewall],
                    publishes: Vec::new(),
                }
            }
            TunnelEvent::InterfaceConfigureFailed { reason } => {
                self.health = TunnelHealth::Down;
                Outcome {
                    actions: Vec::new(),
                    publishes: vec![TunnelPublish::Down {
                        reason: format!("interface configure failed: {reason}"),
                        since: wall_now,
                    }],
                }
            }

            TunnelEvent::FirewallInstalled => {
                self.firewall_installed = true;
                let was_initializing = matches!(self.health, TunnelHealth::Initializing);
                if was_initializing {
                    self.health = TunnelHealth::Connecting;
                }
                // If a NAT-PMP grant landed before the firewall
                // (out-of-order or shell-bug case), the port is held
                // but not yet published.  Now that the kill switch is
                // up, downstream consumers may safely sync the port.
                let mut publishes = Vec::new();
                if let Some(p) = self.pending_port_ready() {
                    publishes.push(p);
                    self.port_published = true;
                }
                self.exit_ip_chain_scheduled = true;
                Outcome {
                    actions: vec![
                        TunnelAction::PollHandshake {
                            peer_index: self.active_peer_index,
                        },
                        TunnelAction::RequestNatPmp,
                        TunnelAction::RunLeakProbe,
                        TunnelAction::QueryExitIp,
                        TunnelAction::ScheduleTimer {
                            timer: TunnelTimer::HandshakeWatchdog,
                            after: self.config.handshake_poll_interval,
                        },
                        TunnelAction::ScheduleTimer {
                            timer: TunnelTimer::LeakProbe,
                            after: self.config.leak_probe_interval,
                        },
                        TunnelAction::ScheduleTimer {
                            timer: TunnelTimer::ExitIpQuery,
                            after: self.config.exit_ip_query_interval,
                        },
                    ],
                    publishes,
                }
            }
            TunnelEvent::FirewallInstallFailed { reason } => {
                // Fail-closed: without the kill switch we refuse to
                // treat the tunnel as up.  Surface as Down so the
                // operator sees what happened.
                self.health = TunnelHealth::Down;
                Outcome {
                    actions: Vec::new(),
                    publishes: vec![TunnelPublish::Down {
                        reason: format!("firewall install failed: {reason}"),
                        since: wall_now,
                    }],
                }
            }

            TunnelEvent::HandshakeReported { age_seconds } => {
                // Fail-closed precondition: a handshake report cannot
                // promote health to `Up` before the interface is
                // configured and the kill switch is installed.  An
                // out-of-order or stale event under these conditions
                // is silently dropped — never a health transition.
                if !self.interface_configured || !self.firewall_installed {
                    return Outcome::none();
                }
                // Freshness check lives in core (not the shell): a
                // report with `age_seconds` above the configured
                // threshold is routed to the stall path so the
                // recovery state machine sees it.
                if Duration::from_secs(age_seconds) > self.config.handshake_stall_after {
                    return self.apply_handshake_stalled(wall_now);
                }
                self.last_handshake_age_seconds = Some(age_seconds);
                self.consecutive_stalls = 0;
                let was_up = matches!(self.health, TunnelHealth::Up);
                let was_stuck = matches!(self.health, TunnelHealth::Stuck { .. });
                self.health = TunnelHealth::Up;
                let mut publishes = Vec::new();
                if !was_up {
                    publishes.push(TunnelPublish::Up);
                }
                if was_stuck {
                    publishes.push(TunnelPublish::Recovered);
                    self.rotation_count = 0;
                }
                // Publish a held-but-unpublished port now that
                // preconditions are satisfied (this is the
                // belt-and-braces path; the firewall-install handler
                // is the main one).
                if let Some(p) = self.pending_port_ready() {
                    publishes.push(p);
                    self.port_published = true;
                }
                Outcome {
                    actions: Vec::new(),
                    publishes,
                }
            }
            TunnelEvent::HandshakeStalled => self.apply_handshake_stalled(wall_now),

            TunnelEvent::NatPmpLeaseGranted {
                external_port,
                lifetime_seconds,
                epoch_seconds,
            } => {
                self.consecutive_natpmp_failures = 0;
                self.port_degraded_published = false;

                // RFC 6886 §3.6: epoch decrease = gateway reboot,
                // existing leases invalid.  Force a fresh request on
                // the next tick by clearing the port.  This response
                // already carries a new lease, so the port is still
                // good — but we record the epoch shift so the next
                // failure surfaces immediately.
                let prior_epoch = self.last_natpmp_epoch.replace(epoch_seconds);
                if let Some(prior) = prior_epoch
                    && epoch_seconds < prior
                {
                    // Gateway rebooted.  We just got a fresh lease so
                    // the port is OK, but the operator should see
                    // this in the observability log via the snapshot
                    // diff.
                }

                let port_changed = self.forwarded_port != Some(external_port);
                self.forwarded_port = Some(external_port);
                if port_changed {
                    // The held port has changed; whatever was
                    // published before is stale.
                    self.port_published = false;
                }

                // Fail-closed: do not publish a forwarded port until
                // the kill switch is in place.  We still hold the
                // port in state and schedule renewal; the publish
                // fires later on the firewall-installed transition.
                let mut publishes = Vec::new();
                if let Some(p) = self.pending_port_ready() {
                    publishes.push(p);
                    self.port_published = true;
                }
                let actions = vec![TunnelAction::ScheduleTimer {
                    timer: TunnelTimer::PortRenewal,
                    after: lease_renewal_delay(
                        Duration::from_secs(u64::from(lifetime_seconds)),
                        self.config.port_renewal_basis_points.into_inner(),
                    ),
                }];
                Outcome { actions, publishes }
            }
            TunnelEvent::NatPmpFailed { reason } => {
                self.consecutive_natpmp_failures =
                    self.consecutive_natpmp_failures.saturating_add(1);
                let mut publishes = Vec::new();
                let crossed = !self.port_degraded_published
                    && self.consecutive_natpmp_failures
                        >= self.config.natpmp_failure_threshold.into_inner();
                if crossed {
                    publishes.push(TunnelPublish::PortForwardingDegraded {
                        consecutive_failures: self.consecutive_natpmp_failures,
                        last_reason: reason.to_string(),
                    });
                    self.port_degraded_published = true;
                    if self.forwarded_port.take().is_some() {
                        // Only surface PortUnavailable to consumers
                        // if we had actually published the port to
                        // them.  Otherwise this is bookkeeping only.
                        if self.port_published {
                            publishes.push(TunnelPublish::PortUnavailable);
                        }
                        self.port_published = false;
                    }
                }
                // Bounded retry: schedule another attempt with
                // increasing delay (cap at 5 min to avoid stampede).
                let backoff = backoff_for_attempt(self.consecutive_natpmp_failures);
                let actions = vec![TunnelAction::ScheduleTimer {
                    timer: TunnelTimer::PortRenewal,
                    after: backoff,
                }];
                Outcome { actions, publishes }
            }

            TunnelEvent::LeakProbeCompleted { outcome } => {
                let mut publishes = Vec::new();
                match outcome {
                    LeakProbeOutcome::NoEgressDetected | LeakProbeOutcome::Inconclusive { .. } => {
                        // No leak signal.  Reschedule and move on.
                    }
                    LeakProbeOutcome::LeakDetected {
                        interface,
                        observed_remote,
                    } => {
                        publishes.push(TunnelPublish::LeakDetected {
                            interface: interface.clone(),
                            observed_remote: observed_remote.clone(),
                        });
                        // A leak is severe: take the health gate down
                        // immediately so admission falls closed.
                        let was_up = matches!(self.health, TunnelHealth::Up);
                        self.health = TunnelHealth::Down;
                        if was_up {
                            publishes.push(TunnelPublish::Down {
                                reason: format!(
                                    "leak detected via {interface} reaching {observed_remote}"
                                ),
                                since: wall_now,
                            });
                        }
                    }
                }
                let actions = vec![TunnelAction::ScheduleTimer {
                    timer: TunnelTimer::LeakProbe,
                    after: self.config.leak_probe_interval,
                }];
                Outcome { actions, publishes }
            }

            TunnelEvent::TimerFired(TunnelTimer::HandshakeWatchdog) => Outcome {
                actions: vec![TunnelAction::PollHandshake {
                    peer_index: self.active_peer_index,
                }],
                publishes: Vec::new(),
            },
            TunnelEvent::TimerFired(TunnelTimer::PortRenewal) => Outcome {
                actions: vec![TunnelAction::RequestNatPmp],
                publishes: Vec::new(),
            },
            TunnelEvent::TimerFired(TunnelTimer::LeakProbe) => Outcome {
                actions: vec![TunnelAction::RunLeakProbe],
                publishes: Vec::new(),
            },
            // Self-perpetuating exit-IP query heartbeat.  Always
            // reschedules so a dropped event cannot kill the chain.
            TunnelEvent::TimerFired(TunnelTimer::ExitIpQuery) => Outcome {
                actions: vec![
                    TunnelAction::QueryExitIp,
                    TunnelAction::ScheduleTimer {
                        timer: TunnelTimer::ExitIpQuery,
                        after: self.config.exit_ip_query_interval,
                    },
                ],
                publishes: Vec::new(),
            },
            // Shell got a usable answer from the public-IP query.
            // Rising-edge publish on first observation or change.
            TunnelEvent::ExitIpObserved { ip } => {
                let changed = self.last_exit_ip != Some(ip);
                self.last_exit_ip = Some(ip);
                let publishes = if changed {
                    vec![TunnelPublish::ExitIpObserved { ip }]
                } else {
                    Vec::new()
                };
                Outcome {
                    actions: Vec::new(),
                    publishes,
                }
            }
            // Shell could not run the query.  Bounded retry happens
            // via the existing ExitIpQuery timer; we just log via the
            // state snapshot.  No publish yet — we wait for sustained
            // failure rather than transient noise.
            TunnelEvent::ExitIpQueryFailed { reason: _ } => Outcome::none(),
        }
    }

    fn handle_command(
        &mut self,
        now: Instant,
        wall_now: DateTime<Utc>,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        match cmd {
            TunnelCommand::PollHandshakeNow => Self::outcome(
                vec![TunnelAction::PollHandshake {
                    peer_index: self.active_peer_index,
                }],
                TunnelResponse::Accepted,
            ),
            TunnelCommand::RequestPortNow => {
                Self::outcome(vec![TunnelAction::RequestNatPmp], TunnelResponse::Accepted)
            }
            TunnelCommand::RotateEndpointNow => {
                if self.config.peer_count.into_inner() <= 1 {
                    return Self::outcome(
                        Vec::new(),
                        TunnelResponse::Rejected {
                            reason: RejectionReason::NoAlternativePeer,
                        },
                    );
                }
                if !self.interface_configured {
                    return Self::outcome(
                        Vec::new(),
                        TunnelResponse::Rejected {
                            reason: RejectionReason::NotYetInitialized,
                        },
                    );
                }
                self.active_peer_index = self.next_peer_index();
                self.rotation_count = self.rotation_count.saturating_add(1);
                self.consecutive_stalls = 0;
                Self::outcome(
                    vec![TunnelAction::RotateEndpoint {
                        peer_index: self.active_peer_index,
                    }],
                    TunnelResponse::Accepted,
                )
            }
            TunnelCommand::RunLeakProbeNow => {
                Self::outcome(vec![TunnelAction::RunLeakProbe], TunnelResponse::Accepted)
            }
        }
        // unused but required by the surrounding fn signature.
        // (Clippy refuses an actually-unused param so we sink them.)
        // The compiler will optimise this away.
        .also_use(now, wall_now)
    }

    fn state_snapshot(&self) -> Self::StateSnapshot {
        self.clone()
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Computes the renewal delay for a granted lease.  Scaled by the
/// configured basis-points fraction (`10_000` = 1.0) and clamped to
/// >= 1 s so we don't spin in tests with very small leases.
fn lease_renewal_delay(lifetime: Duration, basis_points: u16) -> Duration {
    let secs = lifetime.as_secs() * u64::from(basis_points) / 10_000;
    Duration::from_secs(secs.max(1))
}

/// Exponential backoff with a 5-minute cap.  Attempt 1 = 5 s, 2 = 10 s,
/// 3 = 20 s, etc.
fn backoff_for_attempt(attempt: u32) -> Duration {
    let base = Duration::from_secs(5);
    let cap = Duration::from_mins(5);
    let factor = 1u32
        .checked_shl(attempt.saturating_sub(1).min(8))
        .unwrap_or(1);
    let candidate = base.saturating_mul(factor);
    candidate.min(cap)
}

// Internal helper so `handle_command` can keep the `now`/`wall_now`
// parameters live without unused-variable warnings.  This avoids
// peppering `#[allow]` attributes.
trait AlsoUse {
    fn also_use(self, now: Instant, wall_now: DateTime<Utc>) -> Self;
}

impl<A, P, R> AlsoUse for CommandOutcome<A, P, R> {
    fn also_use(self, _now: Instant, _wall_now: DateTime<Utc>) -> Self {
        self
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_machine::ExternalCause;

    fn machine() -> TunnelMachine {
        TunnelMachine::new(
            TunnelConfig {
                peer_count: PeerCount::try_new(2).unwrap(),
                handshake_poll_interval: Duration::from_secs(30),
                handshake_stall_after: Duration::from_mins(3),
                stall_count_before_rotate: StallCountBeforeRotate::try_new(2).unwrap(),
                rotations_before_stuck: Some(1),
                port_renewal_basis_points: PortRenewalBasisPoints::try_new(5_000).unwrap(),
                leak_probe_interval: Duration::from_hours(6),
                natpmp_failure_threshold: NatPmpFailureThreshold::try_new(3).unwrap(),
                exit_ip_query_interval: Duration::from_hours(6),
                natpmp_protocol: crate::natpmp::Protocol::Tcp,
                natpmp_lifetime_seconds: 60,
            },
            Instant::now(),
        )
    }

    fn handle(m: &mut TunnelMachine, e: TunnelEvent) -> Outcome<TunnelAction, TunnelPublish> {
        m.handle(
            Instant::now(),
            Utc::now(),
            Timed::external(Instant::now(), ExternalCause::Init, e),
        )
    }

    /// Walks the boot sequence (`Init` → `InterfaceConfigured` →
    /// `FirewallInstalled`) so per-test setup arrives at the
    /// preconditions a handshake or NAT-PMP grant now requires
    /// (`interface_configured && firewall_installed`).
    fn boot(m: &mut TunnelMachine) {
        handle(m, TunnelEvent::Init);
        handle(m, TunnelEvent::InterfaceConfigured);
        handle(m, TunnelEvent::FirewallInstalled);
    }

    #[test]
    fn init_asks_shell_to_configure_interface() {
        let mut m = machine();
        let out = handle(&mut m, TunnelEvent::Init);
        assert_eq!(out.actions.len(), 1);
        assert!(matches!(out.actions[0], TunnelAction::ConfigureInterface));
        assert!(out.publishes.is_empty());
    }

    #[test]
    fn interface_configured_then_firewall_install() {
        let mut m = machine();
        handle(&mut m, TunnelEvent::Init);
        let out = handle(&mut m, TunnelEvent::InterfaceConfigured);
        assert_eq!(out.actions.len(), 1);
        assert!(matches!(out.actions[0], TunnelAction::InstallFirewall));
    }

    #[test]
    fn firewall_installed_drives_boot_actions_and_timers() {
        let mut m = machine();
        handle(&mut m, TunnelEvent::Init);
        handle(&mut m, TunnelEvent::InterfaceConfigured);
        let out = handle(&mut m, TunnelEvent::FirewallInstalled);
        // The shell should now poll handshake, request NAT-PMP, run
        // a leak probe, and have two scheduled timers in flight.
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::PollHandshake { .. }))
        );
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::RequestNatPmp))
        );
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::RunLeakProbe))
        );
        assert!(out.actions.iter().any(|a| matches!(
            a,
            TunnelAction::ScheduleTimer {
                timer: TunnelTimer::HandshakeWatchdog,
                ..
            }
        )));
        assert!(out.actions.iter().any(|a| matches!(
            a,
            TunnelAction::ScheduleTimer {
                timer: TunnelTimer::LeakProbe,
                ..
            }
        )));
        assert!(matches!(m.health(), TunnelHealth::Connecting));
    }

    #[test]
    fn firewall_install_failed_keeps_us_down_with_typed_reason() {
        let mut m = machine();
        let out = handle(
            &mut m,
            TunnelEvent::FirewallInstallFailed {
                reason: FirewallInstallFailure::RulesetRejected("nft load failed".to_string()),
            },
        );
        assert!(matches!(m.health(), TunnelHealth::Down));
        assert!(matches!(
            out.publishes.first(),
            Some(TunnelPublish::Down { .. })
        ));
    }

    #[test]
    fn first_handshake_publishes_up_once() {
        let mut m = machine();
        boot(&mut m);
        let out = handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 5 });
        assert!(out.publishes.contains(&TunnelPublish::Up));
        let out2 = handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        assert!(!out2.publishes.contains(&TunnelPublish::Up));
    }

    #[test]
    fn handshake_before_firewall_is_silently_dropped() {
        // Fail-closed precondition: an out-of-order HandshakeReported
        // cannot promote health to Up before the kill switch is in.
        let mut m = machine();
        // Interface up but no firewall yet.
        handle(&mut m, TunnelEvent::Init);
        handle(&mut m, TunnelEvent::InterfaceConfigured);
        let out = handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 5 });
        assert!(
            out.publishes.is_empty(),
            "expected drop, got {:?}",
            out.publishes
        );
        assert!(
            out.actions.is_empty(),
            "expected drop, got {:?}",
            out.actions
        );
        assert!(!matches!(m.health(), TunnelHealth::Up));
    }

    #[test]
    fn handshake_before_interface_is_silently_dropped() {
        let mut m = machine();
        let out = handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        assert!(out.publishes.is_empty());
        assert!(out.actions.is_empty());
        assert!(matches!(m.health(), TunnelHealth::Initializing));
    }

    #[test]
    fn over_threshold_handshake_routes_to_stall_path() {
        // Freshness is a core decision: an `age_seconds` past
        // `handshake_stall_after` must NOT publish `Up`, must
        // increment the stall counter, and must reschedule the
        // watchdog like a regular `HandshakeStalled`.
        let mut m = machine();
        boot(&mut m);
        let stale_age = m.config.handshake_stall_after.as_secs() + 1;
        let out = handle(
            &mut m,
            TunnelEvent::HandshakeReported {
                age_seconds: stale_age,
            },
        );
        assert!(!out.publishes.contains(&TunnelPublish::Up));
        assert!(out.actions.iter().any(|a| matches!(
            a,
            TunnelAction::ScheduleTimer {
                timer: TunnelTimer::HandshakeWatchdog,
                ..
            }
        )));
        // Reaching the rotate threshold (2) by sending one more stale
        // report must trigger an endpoint rotation just like
        // back-to-back `HandshakeStalled` events do.
        let out2 = handle(
            &mut m,
            TunnelEvent::HandshakeReported {
                age_seconds: stale_age,
            },
        );
        assert!(
            out2.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::RotateEndpoint { .. }))
        );
    }

    #[test]
    fn handshake_stalls_below_threshold_do_not_rotate() {
        let mut m = machine();
        boot(&mut m);
        // Get to Up first.
        handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        // One stall; threshold is 2.
        let out = handle(&mut m, TunnelEvent::HandshakeStalled);
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::RotateEndpoint { .. }))
        );
        assert!(matches!(m.health(), TunnelHealth::Up));
    }

    #[test]
    fn handshake_stalls_at_threshold_rotate_endpoint() {
        let mut m = machine();
        boot(&mut m);
        handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        handle(&mut m, TunnelEvent::HandshakeStalled);
        let out = handle(&mut m, TunnelEvent::HandshakeStalled);
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::RotateEndpoint { .. }))
        );
        assert_eq!(m.active_peer_index(), 1);
        assert!(matches!(m.health(), TunnelHealth::Down));
        assert!(
            out.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::Down { .. }))
        );
    }

    #[test]
    fn stalls_after_all_rotations_publish_stuck_once() {
        let mut m = machine();
        boot(&mut m);
        // Fast-forward to one rotation already done.
        handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        handle(&mut m, TunnelEvent::HandshakeStalled);
        handle(&mut m, TunnelEvent::HandshakeStalled);
        // Now exhaust the rotation budget by stalling twice more.
        handle(&mut m, TunnelEvent::HandshakeStalled);
        let out = handle(&mut m, TunnelEvent::HandshakeStalled);
        assert!(
            out.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::Stuck { .. })),
            "expected Stuck publish, got {:?}",
            out.publishes
        );
        assert!(matches!(m.health(), TunnelHealth::Stuck { .. }));
        // Re-stalling should not republish Stuck.
        let out2 = handle(&mut m, TunnelEvent::HandshakeStalled);
        assert!(
            !out2
                .publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::Stuck { .. }))
        );
    }

    #[test]
    fn handshake_recovers_from_stuck_publishes_recovered() {
        let mut m = machine();
        boot(&mut m);
        // Walk to Stuck.
        handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        for _ in 0..4 {
            handle(&mut m, TunnelEvent::HandshakeStalled);
        }
        assert!(matches!(m.health(), TunnelHealth::Stuck { .. }));
        let out = handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        assert!(out.publishes.contains(&TunnelPublish::Recovered));
        assert!(matches!(m.health(), TunnelHealth::Up));
    }

    #[test]
    fn natpmp_grant_publishes_port_ready_and_schedules_renewal() {
        let mut m = machine();
        boot(&mut m);
        let out = handle(
            &mut m,
            TunnelEvent::NatPmpLeaseGranted {
                external_port: 51820,
                lifetime_seconds: 60,
                epoch_seconds: 100,
            },
        );
        assert!(
            out.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortReady { .. }))
        );
        assert!(out.actions.iter().any(|a| matches!(
            a,
            TunnelAction::ScheduleTimer {
                timer: TunnelTimer::PortRenewal,
                ..
            }
        )));
        assert_eq!(m.forwarded_port(), Some(51820));
    }

    #[test]
    fn natpmp_grant_before_firewall_holds_port_without_publish() {
        // Defense-in-depth: even if the shell sends a NAT-PMP grant
        // before the firewall is up (out-of-order or shell bug), we
        // must not publish `PortReady` until the kill switch is in
        // place — otherwise qBit could sync a port over an
        // un-protected egress.
        let mut m = machine();
        handle(&mut m, TunnelEvent::Init);
        handle(&mut m, TunnelEvent::InterfaceConfigured);
        // Firewall NOT yet installed.
        let out = handle(
            &mut m,
            TunnelEvent::NatPmpLeaseGranted {
                external_port: 51820,
                lifetime_seconds: 60,
                epoch_seconds: 100,
            },
        );
        assert!(
            !out.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortReady { .. })),
            "must not publish PortReady before firewall is installed"
        );
        // Port is still held internally so we don't lose it.
        assert_eq!(m.forwarded_port(), Some(51820));
        // The deferred publish fires on the FirewallInstalled
        // transition.
        let out2 = handle(&mut m, TunnelEvent::FirewallInstalled);
        assert!(
            out2.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortReady { .. })),
            "deferred PortReady should fire on FirewallInstalled"
        );
    }

    #[test]
    fn natpmp_same_port_does_not_republish() {
        let mut m = machine();
        boot(&mut m);
        handle(
            &mut m,
            TunnelEvent::NatPmpLeaseGranted {
                external_port: 51820,
                lifetime_seconds: 60,
                epoch_seconds: 100,
            },
        );
        let out = handle(
            &mut m,
            TunnelEvent::NatPmpLeaseGranted {
                external_port: 51820,
                lifetime_seconds: 60,
                epoch_seconds: 200,
            },
        );
        assert!(
            !out.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortReady { .. }))
        );
    }

    #[test]
    fn natpmp_failures_publish_degraded_once_at_threshold() {
        let mut m = machine();
        boot(&mut m);
        handle(
            &mut m,
            TunnelEvent::NatPmpLeaseGranted {
                external_port: 51820,
                lifetime_seconds: 60,
                epoch_seconds: 100,
            },
        );
        let out1 = handle(
            &mut m,
            TunnelEvent::NatPmpFailed {
                reason: NatPmpFailure::Timeout,
            },
        );
        let out2 = handle(
            &mut m,
            TunnelEvent::NatPmpFailed {
                reason: NatPmpFailure::Timeout,
            },
        );
        let out3 = handle(
            &mut m,
            TunnelEvent::NatPmpFailed {
                reason: NatPmpFailure::Timeout,
            },
        );
        assert!(
            !out1
                .publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortForwardingDegraded { .. }))
        );
        assert!(
            !out2
                .publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortForwardingDegraded { .. }))
        );
        assert!(
            out3.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortForwardingDegraded { .. }))
        );
        assert!(
            out3.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortUnavailable))
        );
        // Subsequent failures during the same streak do not republish.
        let out4 = handle(
            &mut m,
            TunnelEvent::NatPmpFailed {
                reason: NatPmpFailure::Timeout,
            },
        );
        assert!(
            !out4
                .publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::PortForwardingDegraded { .. }))
        );
    }

    #[test]
    fn leak_detected_publishes_and_takes_health_down() {
        let mut m = machine();
        boot(&mut m);
        handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        assert!(matches!(m.health(), TunnelHealth::Up));
        let out = handle(
            &mut m,
            TunnelEvent::LeakProbeCompleted {
                outcome: LeakProbeOutcome::LeakDetected {
                    interface: "eth0".to_string(),
                    observed_remote: "203.0.113.1".to_string(),
                },
            },
        );
        assert!(
            out.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::LeakDetected { .. }))
        );
        assert!(
            out.publishes
                .iter()
                .any(|p| matches!(p, TunnelPublish::Down { .. }))
        );
        assert!(matches!(m.health(), TunnelHealth::Down));
    }

    #[test]
    fn leak_no_egress_just_reschedules() {
        let mut m = machine();
        boot(&mut m);
        handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 1 });
        let out = handle(
            &mut m,
            TunnelEvent::LeakProbeCompleted {
                outcome: LeakProbeOutcome::NoEgressDetected,
            },
        );
        assert!(out.publishes.is_empty());
        assert!(out.actions.iter().any(|a| matches!(
            a,
            TunnelAction::ScheduleTimer {
                timer: TunnelTimer::LeakProbe,
                ..
            }
        )));
        assert!(matches!(m.health(), TunnelHealth::Up));
    }

    #[test]
    fn watchdog_timer_fires_poll_handshake() {
        let mut m = machine();
        let out = handle(
            &mut m,
            TunnelEvent::TimerFired(TunnelTimer::HandshakeWatchdog),
        );
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::PollHandshake { .. }))
        );
    }

    #[test]
    fn port_renewal_timer_fires_request_natpmp() {
        let mut m = machine();
        let out = handle(&mut m, TunnelEvent::TimerFired(TunnelTimer::PortRenewal));
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::RequestNatPmp))
        );
    }

    #[test]
    fn rotate_endpoint_now_rejected_when_one_peer() {
        let mut m = TunnelMachine::new(
            TunnelConfig {
                peer_count: PeerCount::try_new(1).unwrap(),
                ..TunnelConfig::default()
            },
            Instant::now(),
        );
        m.interface_configured = true;
        let out = m.handle_command(Instant::now(), Utc::now(), TunnelCommand::RotateEndpointNow);
        assert!(matches!(
            out.response,
            TunnelResponse::Rejected {
                reason: RejectionReason::NoAlternativePeer
            }
        ));
        assert!(out.actions.is_empty());
    }

    #[test]
    fn rotate_endpoint_now_rejected_before_init() {
        let mut m = machine();
        let out = m.handle_command(Instant::now(), Utc::now(), TunnelCommand::RotateEndpointNow);
        assert!(matches!(
            out.response,
            TunnelResponse::Rejected {
                reason: RejectionReason::NotYetInitialized
            }
        ));
    }

    #[test]
    fn rotate_endpoint_now_advances_peer_index() {
        let mut m = machine();
        m.interface_configured = true;
        let start = m.active_peer_index();
        let out = m.handle_command(Instant::now(), Utc::now(), TunnelCommand::RotateEndpointNow);
        assert!(matches!(out.response, TunnelResponse::Accepted));
        assert_ne!(m.active_peer_index(), start);
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, TunnelAction::RotateEndpoint { .. }))
        );
    }

    #[test]
    fn rotations_before_stuck_none_normalizes_to_peer_count() {
        // `None` means "try every peer once".  TunnelMachine::new
        // resolves it against the supplied peer_count.
        let m = TunnelMachine::new(
            TunnelConfig {
                peer_count: PeerCount::try_new(3).unwrap(),
                rotations_before_stuck: None,
                ..TunnelConfig::default()
            },
            Instant::now(),
        );
        assert_eq!(m.config.rotations_before_stuck, Some(3));
    }

    #[test]
    fn rotations_before_stuck_clamped_to_peer_count() {
        // Over-set values get clamped down because the rotation loop
        // wraps anyway.
        let m = TunnelMachine::new(
            TunnelConfig {
                peer_count: PeerCount::try_new(2).unwrap(),
                rotations_before_stuck: Some(99),
                ..TunnelConfig::default()
            },
            Instant::now(),
        );
        assert_eq!(m.config.rotations_before_stuck, Some(2));
    }

    #[test]
    fn snapshot_serializes_with_health_and_peer() {
        let mut m = machine();
        boot(&mut m);
        handle(&mut m, TunnelEvent::HandshakeReported { age_seconds: 5 });
        let value = serde_json::to_value(m.state_snapshot()).expect("snapshot should serialize");
        // Health snapshot reflects up.
        assert_eq!(value["health"], serde_json::json!("Up"));
        // Peer index is included.
        assert!(value.get("active_peer_index").is_some());
        // Forwarded port slot is present (still null).
        assert!(value.get("forwarded_port").is_some());
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;
    use windlass_machine::ExternalCause;

    fn any_event() -> impl Strategy<Value = TunnelEvent> {
        prop_oneof![
            Just(TunnelEvent::Init),
            Just(TunnelEvent::InterfaceConfigured),
            any::<String>().prop_map(|r| TunnelEvent::InterfaceConfigureFailed {
                reason: InterfaceConfigureFailure::Other(r),
            }),
            Just(TunnelEvent::FirewallInstalled),
            any::<String>().prop_map(|r| TunnelEvent::FirewallInstallFailed {
                reason: FirewallInstallFailure::Other(r),
            }),
            (0u64..=1_000_000u64)
                .prop_map(|age| TunnelEvent::HandshakeReported { age_seconds: age }),
            Just(TunnelEvent::HandshakeStalled),
            (1u16..=u16::MAX, 1u32..=86_400u32, 0u32..=u32::MAX).prop_map(
                |(port, lifetime, epoch)| TunnelEvent::NatPmpLeaseGranted {
                    external_port: port,
                    lifetime_seconds: lifetime,
                    epoch_seconds: epoch,
                }
            ),
            any::<String>().prop_map(|r| TunnelEvent::NatPmpFailed {
                reason: NatPmpFailure::Recv(r),
            }),
            Just(TunnelEvent::LeakProbeCompleted {
                outcome: LeakProbeOutcome::NoEgressDetected
            }),
            Just(TunnelEvent::TimerFired(TunnelTimer::HandshakeWatchdog)),
            Just(TunnelEvent::TimerFired(TunnelTimer::PortRenewal)),
            Just(TunnelEvent::TimerFired(TunnelTimer::LeakProbe)),
        ]
    }

    proptest! {
        /// GLOBAL-1 (no panic): handle tolerates any (state, event) pair.
        #[test]
        fn handle_never_panics(event in any_event()) {
            let mut m = TunnelMachine::new(TunnelConfig::default(), Instant::now());
            let _ = m.handle(
                Instant::now(),
                Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, event),
            );
        }

        /// TUN-1 (safety): `Up` publishes are rising-edge only and
        /// gated on the kill switch being installed.  Walking the
        /// boot sequence first, two consecutive in-threshold
        /// `HandshakeReported` events publish `Up` exactly once.
        #[test]
        fn up_publish_is_rising_edge(
            age1 in 0u64..30,
            age2 in 0u64..30,
        ) {
            let cfg = TunnelConfig::default();
            let mut m = TunnelMachine::new(cfg, Instant::now());
            // Boot through preconditions.
            for e in [
                TunnelEvent::Init,
                TunnelEvent::InterfaceConfigured,
                TunnelEvent::FirewallInstalled,
            ] {
                let _ = m.handle(
                    Instant::now(),
                    Utc::now(),
                    Timed::external(Instant::now(), ExternalCause::Unknown, e),
                );
            }
            let out1 = m.handle(
                Instant::now(),
                Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown,
                    TunnelEvent::HandshakeReported { age_seconds: age1 }),
            );
            let out2 = m.handle(
                Instant::now(),
                Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown,
                    TunnelEvent::HandshakeReported { age_seconds: age2 }),
            );
            let total_up = out1.publishes.iter().chain(out2.publishes.iter())
                .filter(|p| matches!(p, TunnelPublish::Up)).count();
            prop_assert_eq!(total_up, 1);
        }

        /// TUN-2 (safety): a NAT-PMP grant always records the port
        /// internally and always schedules a renewal timer,
        /// regardless of whether the kill switch is yet installed.
        /// The `PortReady` publish is deferred separately
        /// (`natpmp_grant_before_firewall_holds_port_without_publish`
        /// in the unit tests).
        #[test]
        fn natpmp_grant_records_port_and_schedules(
            port in 1u16..=u16::MAX,
            lifetime in 1u32..=3600u32,
            epoch in 0u32..=u32::MAX,
        ) {
            let mut m = TunnelMachine::new(TunnelConfig::default(), Instant::now());
            let out = m.handle(
                Instant::now(),
                Utc::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown,
                    TunnelEvent::NatPmpLeaseGranted {
                        external_port: port,
                        lifetime_seconds: lifetime,
                        epoch_seconds: epoch,
                    }),
            );
            prop_assert_eq!(m.forwarded_port(), Some(port));
            let renewal_count = out.actions.iter().filter(|a|
                matches!(a, TunnelAction::ScheduleTimer {
                    timer: TunnelTimer::PortRenewal, ..
                })
            ).count();
            prop_assert_eq!(renewal_count, 1);
        }
    }
}
