#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

//! Thin VPN-state translator between the tunnel bridge and the domain.
//!
//! Windlass owns its `WireGuard` tunnel in-process
//! (`docs/vpn-ownership.md`); the tunnel core publishes typed facts and
//! the runtime bridge (`windlass/src/shell/tunnel_bridge.rs`) maps them
//! into the [`VpnEvent`]s this machine consumes.  The machine keeps the
//! connected/port view the domain's existing [`VpnPublish`] consumers
//! were built against and emits the rising-edge `Crashed` / `Recovered`
//! signals that drive crash recovery.
//!
//! The legacy Gluetun mode (container health polling, IP/port file
//! watching, proxy-routed IP verification) is gone; this machine has no
//! actions, no timers, and no commands — it is event-driven from the
//! bridge only.  Several [`VpnPublish`] variants are produced by the
//! bridge directly (never by this machine) and live here because the
//! domain consumes them under the `WindlassEvent::Vpn` envelope.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::{VpnIp, VpnPort};

/// §33: which external check produced a `PublicIpMismatch`.
///
/// `IfConfigCo` is the public-internet source — in tunnel mode the
/// bridge uses it when the leak probe catches a non-tunnel egress.
/// `MamJsonIp` is MAM's own `/json/jsonIp.php` view.  The two usually
/// agree, but when they diverge the alert names the source so the
/// operator can tell a public-internet edge case from a MAM-compliance
/// problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationSource {
    IfConfigCo,
    MamJsonIp,
}

/// No commands: the tunnel core owns the operator command surface
/// (`TunnelCommand`); this translator is driven by bridge events only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnCommand {}

/// Events the tunnel bridge injects (see
/// `windlass/src/shell/tunnel_bridge.rs` for the mapping from
/// `TunnelPublish`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnEvent {
    /// Tunnel transitioned to healthy (`TunnelPublish::Up` /
    /// `Recovered`).
    ContainerHealthy,
    /// Tunnel transitioned to unhealthy (`TunnelPublish::Down` /
    /// `Stuck`, or a detected leak).
    ContainerUnhealthy,
    /// A forwarded port is available (`TunnelPublish::PortReady`).
    PortFileChanged { port: VpnPort },
}

/// No actions: the machine is a pure translator; all VPN I/O lives in
/// the tunnel core + `windlass-net`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnAction {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnPublish {
    Connected,
    Disconnected,
    PortReady {
        port: VpnPort,
    },
    PortUnavailable,
    /// Public exit IP observed through the tunnel.  Produced by the
    /// bridge (from `TunnelPublish::ExitIpObserved` / `Up`); the domain
    /// forwards it to the MAM core's `ObservedIpChanged` command.
    PublicIpObserved {
        ip: VpnIp,
    },
    /// The tunnel is down or the exit IP can no longer be confirmed.
    /// Produced by the bridge; clears `admission.vpn_ip_compliant` in
    /// the domain.
    PublicIpUnavailable,
    /// A verification source reports a different IP than the one we
    /// trusted — a potential leak.  Produced by the bridge from
    /// `TunnelPublish::LeakDetected`.  Flips the §29
    /// `vpn_ip_compliant` gate to `Some(false)` and fires a `Critical`
    /// alert.
    PublicIpMismatch {
        file_ip: VpnIp,
        verified_ip: VpnIp,
        source: VerificationSource,
    },
    /// Exit-IP verification has failed repeatedly (produced by the
    /// bridge from `TunnelPublish::ExitIpVerificationDegraded`).
    /// Surfaces as a `Warning` alert without blocking admission.
    PublicIpVerificationDegraded {
        consecutive_failures: u32,
        last_reason: String,
    },
    /// §38 / VPN-17: rising-edge healthy → unhealthy transition.
    /// Sibling to `Disconnected`, which is idempotent.  Domain reacts
    /// to this exactly-once signal to drive crash recovery (log dump,
    /// stop dependents, Critical alert).
    Crashed,
    /// §38 / VPN-18: rising-edge unhealthy → healthy transition.
    /// Sibling to `Connected`, which is idempotent.  Domain reacts to
    /// this once per real recovery to start dependents back up.
    Recovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnTopic {
    Connectivity,
    Port,
    /// Public-IP observation + verification topic.
    PublicIp,
}

impl HasTopic<VpnTopic> for VpnPublish {
    fn topic(&self) -> VpnTopic {
        match self {
            Self::Connected | Self::Disconnected | Self::Crashed | Self::Recovered => {
                VpnTopic::Connectivity
            }
            Self::PortReady { .. } | Self::PortUnavailable => VpnTopic::Port,
            Self::PublicIpObserved { .. }
            | Self::PublicIpUnavailable
            | Self::PublicIpMismatch { .. }
            | Self::PublicIpVerificationDegraded { .. } => VpnTopic::PublicIp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnResponse {
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VpnMachine {
    connected: bool,
    port: Option<VpnPort>,
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
}

impl Machine for VpnMachine {
    type Config = ();
    type Event = VpnEvent;
    type Action = VpnAction;
    type Publish = VpnPublish;
    type Topic = VpnTopic;
    type Command = VpnCommand;
    type Response = VpnResponse;
    type StateSnapshot = Self;

    fn new((): Self::Config, _now: Instant) -> Self {
        Self {
            connected: false,
            port: None,
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        _wall_now: chrono::DateTime<chrono::Utc>,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            VpnEvent::ContainerHealthy => {
                let was_unhealthy = !self.connected;
                self.connected = true;
                let mut publishes = vec![VpnPublish::Connected];
                if was_unhealthy {
                    // §38: rising-edge recovery signal so domain can drive
                    // the post-crash StartDependents fan-out exactly once.
                    publishes.push(VpnPublish::Recovered);
                }
                Outcome {
                    actions: Vec::new(),
                    publishes,
                }
            }
            VpnEvent::ContainerUnhealthy => {
                let was_connected = self.connected;
                self.connected = false;
                self.port = None;
                let mut publishes = vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable];
                if was_connected {
                    // §38: rising-edge crash signal so domain can drive
                    // log dump + StopDependents + Critical alert exactly
                    // once per crash.
                    publishes.push(VpnPublish::Crashed);
                }
                Outcome {
                    actions: Vec::new(),
                    publishes,
                }
            }
            VpnEvent::PortFileChanged { port } => {
                self.port = Some(port);
                Outcome {
                    actions: Vec::new(),
                    publishes: vec![VpnPublish::PortReady { port }],
                }
            }
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        _wall_now: chrono::DateTime<chrono::Utc>,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        // `VpnCommand` is uninhabited; this match proves it at the type
        // level and the function can never actually run.
        match cmd {}
    }

    fn state_snapshot(&self) -> Self::StateSnapshot {
        self.clone()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use windlass_machine::{ExternalCause, Machine, Outcome, Timed};
    use windlass_types::VpnPort;

    use crate::{VpnEvent, VpnMachine, VpnPublish};

    fn machine() -> VpnMachine {
        VpnMachine::new((), Instant::now())
    }

    fn handle(machine: &mut VpnMachine, event: VpnEvent) -> Outcome<crate::VpnAction, VpnPublish> {
        machine.handle(
            Instant::now(),
            chrono::Utc::now(),
            Timed::external(Instant::now(), ExternalCause::Unknown, event),
        )
    }

    #[test]
    fn healthy_publishes_connected_and_recovered_on_rising_edge() {
        let mut m = machine();
        // Boot: unhealthy → healthy is a rising edge.
        let out = handle(&mut m, VpnEvent::ContainerHealthy);
        assert_eq!(
            out.publishes,
            vec![VpnPublish::Connected, VpnPublish::Recovered]
        );
        assert!(m.is_connected());
        // Re-observing healthy is idempotent: Connected only.
        let out = handle(&mut m, VpnEvent::ContainerHealthy);
        assert_eq!(out.publishes, vec![VpnPublish::Connected]);
    }

    #[test]
    fn unhealthy_publishes_crashed_exactly_once_per_transition() {
        let mut m = machine();
        handle(&mut m, VpnEvent::ContainerHealthy);
        let out = handle(&mut m, VpnEvent::ContainerUnhealthy);
        assert_eq!(
            out.publishes,
            vec![
                VpnPublish::Disconnected,
                VpnPublish::PortUnavailable,
                VpnPublish::Crashed,
            ]
        );
        assert!(!m.is_connected());
        // Repeated unhealthy: no second Crashed.
        let out = handle(&mut m, VpnEvent::ContainerUnhealthy);
        assert_eq!(
            out.publishes,
            vec![VpnPublish::Disconnected, VpnPublish::PortUnavailable]
        );
    }

    #[test]
    fn unhealthy_drops_the_held_port() {
        let mut m = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        handle(&mut m, VpnEvent::ContainerHealthy);
        let out = handle(&mut m, VpnEvent::PortFileChanged { port });
        assert_eq!(out.publishes, vec![VpnPublish::PortReady { port }]);
        assert_eq!(m.port(), Some(port));
        handle(&mut m, VpnEvent::ContainerUnhealthy);
        assert_eq!(m.port(), None, "a disconnected VPN never holds a port");
    }

    #[test]
    fn state_snapshot_reflects_post_event_state() {
        // §37b: snapshot is an owned, serializable view of the machine.
        let mut m = machine();
        handle(&mut m, VpnEvent::ContainerHealthy);
        let port = VpnPort::try_new(51_820).unwrap();
        handle(&mut m, VpnEvent::PortFileChanged { port });
        let snapshot = m.state_snapshot();
        let value = serde_json::to_value(snapshot).expect("snapshot serializes");
        assert_eq!(value["connected"], true);
        assert_eq!(value["port"], 51_820);
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::Instant;

    use proptest::prelude::*;
    use windlass_machine::{ExternalCause, Machine, Timed};
    use windlass_types::VpnPort;

    use crate::{VpnEvent, VpnMachine, VpnPublish};

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).expect("range is valid"))
    }

    fn any_vpn_event() -> impl Strategy<Value = VpnEvent> {
        prop_oneof![
            Just(VpnEvent::ContainerHealthy),
            Just(VpnEvent::ContainerUnhealthy),
            any_vpn_port().prop_map(|port| VpnEvent::PortFileChanged { port }),
        ]
    }

    proptest! {
        /// VPN-17/VPN-18 (safety): `Crashed` and `Recovered` are
        /// rising-edge-only across any event sequence — never two of
        /// the same without the opposite transition in between.
        #[test]
        fn crashed_and_recovered_are_rising_edge_only(
            events in proptest::collection::vec(any_vpn_event(), 0..64),
        ) {
            let mut m = VpnMachine::new((), Instant::now());
            let mut last_edge: Option<bool> = None; // true = crashed, false = recovered
            for event in events {
                let out = m.handle(
                    Instant::now(),
                    chrono::Utc::now(),
                    Timed::external(Instant::now(), ExternalCause::Unknown, event),
                );
                for p in out.publishes {
                    match p {
                        VpnPublish::Crashed => {
                            prop_assert_ne!(last_edge, Some(true), "double Crashed");
                            last_edge = Some(true);
                        }
                        VpnPublish::Recovered => {
                            prop_assert_ne!(last_edge, Some(false), "double Recovered");
                            last_edge = Some(false);
                        }
                        _ => {}
                    }
                }
            }
        }

        /// Liveness baseline: the handler never panics for any event
        /// sequence.
        #[test]
        fn handle_never_panics(
            events in proptest::collection::vec(any_vpn_event(), 0..64),
        ) {
            let mut m = VpnMachine::new((), Instant::now());
            for event in events {
                let _ = m.handle(
                    Instant::now(),
                    chrono::Utc::now(),
                    Timed::external(Instant::now(), ExternalCause::Unknown, event),
                );
            }
        }
    }
}
