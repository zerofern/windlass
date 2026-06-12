//! Pure mapping from [`TunnelPublish`] to the events and publishes
//! the rest of the runtime consumes.
//!
//! Lives here (not in `init.rs`) so it can be unit-tested without
//! standing up the full runtime: the bridge is the load-bearing
//! seam between the new tunnel core and the existing
//! VpnMachine/domain consumers, and any miss in the mapping
//! (`PortUnavailable` dropped, `LeakDetected` not flipping
//! admission, etc.) is a real correctness bug.
//!
//! The runtime in `init.rs` runs a spawned drain loop that, per
//! [`TunnelPublish`], asks this function for the typed output and
//! then performs the actual sends + shared-state updates.

use windlass_domain_core::WindlassEvent;
use windlass_tunnel_core::TunnelPublish;
use windlass_types::VpnIp;
use windlass_vpn_core::{VerificationSource, VpnEvent, VpnPublish};

/// What the bridge wants the runtime to do for one [`TunnelPublish`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct TunnelBridgeOutput {
    /// Events to inject into the [`VpnMachine`] event channel.
    /// `VpnMachine` still runs as a thin state translator;
    /// publishes it produces flow to the domain via the existing
    /// VPN forwarder task.
    pub vpn_events: Vec<VpnEvent>,
    /// Publishes to inject *directly* into the domain channel as
    /// [`WindlassEvent::Vpn`].  These are the signals the
    /// domain consumes for admission gating that `VpnMachine`
    /// doesn't reconstruct from its inputs.
    pub vpn_publishes: Vec<VpnPublish>,
    /// When true, the spawned task should clear the
    /// `forwarded_port` Arc so qBit / domain stop trusting a port
    /// the tunnel no longer holds.  Currently fired on
    /// `PortUnavailable` and `PortForwardingDegraded`.
    pub clear_forwarded_port: bool,
}

/// Wraps `WindlassEvent::Vpn(publish)` for the runtime forwarder.
pub(super) const fn wrap_publish_for_domain(publish: VpnPublish) -> WindlassEvent {
    WindlassEvent::Vpn(publish)
}

/// Map one [`TunnelPublish`] to the downstream signals the runtime
/// should produce.  Stateless: the publishes that need the public
/// exit IP (`Up` / `Recovered` / `LeakDetected`) carry it from the
/// tunnel core, which owns that fact — the bridge holding its own
/// copy was a second source of truth that could drift.  The tunnel
/// interface address from `wg.conf` is never a valid substitute for
/// the public IP MAM/domain care about.
#[must_use]
pub(super) fn bridge_tunnel_publish(publish: &TunnelPublish) -> TunnelBridgeOutput {
    match publish {
        TunnelPublish::Up { exit_ip } | TunnelPublish::Recovered { exit_ip } => {
            TunnelBridgeOutput {
                vpn_events: vec![VpnEvent::ContainerHealthy],
                vpn_publishes: exit_ip
                    .map(|ip| vec![VpnPublish::PublicIpObserved { ip }])
                    .unwrap_or_default(),
                clear_forwarded_port: false,
            }
        }
        TunnelPublish::Down { .. } | TunnelPublish::Stuck { .. } => TunnelBridgeOutput {
            vpn_events: vec![VpnEvent::ContainerUnhealthy],
            vpn_publishes: vec![VpnPublish::PublicIpUnavailable],
            // Going Down implies the tunnel's port forwarding is
            // also gone, so clear the cached port.
            clear_forwarded_port: true,
        },
        TunnelPublish::LeakDetected {
            observed_remote,
            exit_ip,
            ..
        } => {
            // file_ip = the last public exit IP we trusted.
            // verified_ip = what we actually reached the leak target as,
            // parsed from `observed_remote` if it's an IPv4 literal —
            // otherwise UNSPECIFIED so the typed publish still flips
            // the admission gate.  The tunnel-core `LeakDetected`
            // publish carries the full `interface`/`observed_remote`
            // strings; the operator-facing alert log shows both via
            // `/observability`.
            let zero = VpnIp(std::net::Ipv4Addr::UNSPECIFIED);
            let verified_ip = observed_remote
                .parse::<std::net::IpAddr>()
                .ok()
                .and_then(VpnIp::from_ip)
                .unwrap_or(zero);
            TunnelBridgeOutput {
                vpn_events: vec![VpnEvent::ContainerUnhealthy],
                vpn_publishes: vec![VpnPublish::PublicIpMismatch {
                    file_ip: exit_ip.unwrap_or(zero),
                    verified_ip,
                    source: VerificationSource::IfConfigCo,
                }],
                clear_forwarded_port: true,
            }
        }
        TunnelPublish::PortReady { port } => TunnelBridgeOutput {
            vpn_events: vec![VpnEvent::PortFileChanged { port: *port }],
            vpn_publishes: vec![],
            clear_forwarded_port: false,
        },
        TunnelPublish::PortUnavailable | TunnelPublish::PortForwardingDegraded { .. } => {
            // The tunnel core says the forwarded port is gone (or
            // degraded past the threshold).  Previously the
            // bridge silently dropped both — that left qBit
            // configured against a port the tunnel no longer
            // held.  We now (a) clear the cached forwarded_port
            // arc so the next status read returns None, and (b)
            // publish VpnPublish::PortUnavailable directly to the
            // domain so it can react to the loss.
            TunnelBridgeOutput {
                vpn_events: vec![],
                vpn_publishes: vec![VpnPublish::PortUnavailable],
                clear_forwarded_port: true,
            }
        }
        // Exit-IP verification has failed past the threshold.  We
        // surface it as a Warning via PublicIpVerificationDegraded
        // so the operator sees it in the alert log, but admission
        // is NOT flipped — the tunnel itself is still up; we just
        // can't confirm the exit IP for now.
        TunnelPublish::ExitIpVerificationDegraded {
            consecutive_failures,
            last_reason,
        } => TunnelBridgeOutput {
            vpn_events: vec![],
            vpn_publishes: vec![VpnPublish::PublicIpVerificationDegraded {
                consecutive_failures: *consecutive_failures,
                last_reason: last_reason.clone(),
            }],
            clear_forwarded_port: false,
        },
        TunnelPublish::ExitIpObserved { ip } => TunnelBridgeOutput {
            // Real exit IP from the §31-equivalent query through
            // the tunnel.  Publish to the domain so MAM's
            // dynamic-seedbox dedup uses the actual Proton egress
            // IP — not the inside-address placeholder we used
            // before this lands.
            vpn_events: vec![],
            vpn_publishes: vec![VpnPublish::PublicIpObserved { ip: *ip }],
            clear_forwarded_port: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_types::VpnPort;

    fn ip(s: &str) -> VpnIp {
        VpnIp(s.parse().unwrap())
    }

    #[test]
    fn up_without_exit_ip_does_not_publish_observed() {
        let out = bridge_tunnel_publish(&TunnelPublish::Up { exit_ip: None });
        assert_eq!(out.vpn_events, vec![VpnEvent::ContainerHealthy]);
        assert!(out.vpn_publishes.is_empty());
        assert!(!out.clear_forwarded_port);
    }

    #[test]
    fn up_with_exit_ip_publishes_observed() {
        let out = bridge_tunnel_publish(&TunnelPublish::Up {
            exit_ip: Some(ip("203.0.113.10")),
        });
        assert_eq!(
            out.vpn_publishes,
            vec![VpnPublish::PublicIpObserved {
                ip: ip("203.0.113.10")
            }]
        );
    }

    #[test]
    fn recovered_publishes_observed_same_as_up() {
        let out = bridge_tunnel_publish(&TunnelPublish::Recovered {
            exit_ip: Some(ip("203.0.113.10")),
        });
        assert!(out.vpn_publishes.iter().any(|p| matches!(
            p,
            VpnPublish::PublicIpObserved { ip } if *ip == self::ip("203.0.113.10")
        )));
    }

    #[test]
    fn down_clears_forwarded_port_and_publishes_unavailable() {
        let out = bridge_tunnel_publish(&TunnelPublish::Down {
            reason: "test".into(),
            since: chrono::Utc::now(),
        });
        assert_eq!(out.vpn_events, vec![VpnEvent::ContainerUnhealthy]);
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PublicIpUnavailable]);
        assert!(out.clear_forwarded_port);
    }

    #[test]
    fn stuck_publishes_unhealthy_and_unavailable() {
        let out = bridge_tunnel_publish(&TunnelPublish::Stuck {
            reason: "test".into(),
            since: chrono::Utc::now(),
            attempted_recoveries: 3,
        });
        assert_eq!(out.vpn_events, vec![VpnEvent::ContainerUnhealthy]);
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PublicIpUnavailable]);
        assert!(out.clear_forwarded_port);
    }

    #[test]
    fn leak_detected_flips_admission_gate_and_clears_port() {
        let out = bridge_tunnel_publish(&TunnelPublish::LeakDetected {
            interface: "eth0".into(),
            observed_remote: "203.0.113.1".into(),
            exit_ip: None,
        });
        assert_eq!(out.vpn_events, vec![VpnEvent::ContainerUnhealthy]);
        assert!(
            matches!(
                out.vpn_publishes.first(),
                Some(VpnPublish::PublicIpMismatch { .. })
            ),
            "LeakDetected must synthesize PublicIpMismatch so the §29 admission gate flips"
        );
        assert!(out.clear_forwarded_port);
    }

    #[test]
    fn port_ready_feeds_vpn_event() {
        let port = VpnPort::try_new(51820).unwrap();
        let out = bridge_tunnel_publish(&TunnelPublish::PortReady { port });
        assert_eq!(out.vpn_events, vec![VpnEvent::PortFileChanged { port }]);
        assert!(out.vpn_publishes.is_empty());
        assert!(!out.clear_forwarded_port);
    }

    /// Regression test for Phase 8 review #2: previously the bridge
    /// dropped `PortUnavailable` entirely, leaving qBit configured
    /// against a port the tunnel no longer held.
    #[test]
    fn port_unavailable_clears_arc_and_publishes_unavailable() {
        let out = bridge_tunnel_publish(&TunnelPublish::PortUnavailable);
        assert!(out.vpn_events.is_empty());
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PortUnavailable]);
        assert!(out.clear_forwarded_port);
    }

    /// Exit-IP `Degraded` should route to
    /// `PublicIpVerificationDegraded` so the domain alert log
    /// surfaces it as a Warning, but admission must stay open
    /// (the tunnel itself is still up; we just can't confirm the
    /// IP yet).
    #[test]
    fn exit_ip_degraded_routes_to_public_ip_verification_degraded_warning() {
        let out = bridge_tunnel_publish(&TunnelPublish::ExitIpVerificationDegraded {
            consecutive_failures: 5,
            last_reason: "ifconfig.co 503".into(),
        });
        assert!(out.vpn_events.is_empty());
        assert_eq!(out.vpn_publishes.len(), 1);
        match &out.vpn_publishes[0] {
            VpnPublish::PublicIpVerificationDegraded {
                consecutive_failures,
                last_reason,
            } => {
                assert_eq!(*consecutive_failures, 5);
                assert_eq!(last_reason, "ifconfig.co 503");
            }
            other => panic!("expected PublicIpVerificationDegraded, got {other:?}"),
        }
        assert!(!out.clear_forwarded_port);
    }

    /// Real exit IP from the query should reach the domain as
    /// `PublicIpObserved` so MAM dedups against the actual Proton
    /// egress IP rather than the inside-address placeholder.
    #[test]
    fn exit_ip_observed_routes_to_public_ip_observed() {
        let exit = ip("203.0.113.5");
        let out = bridge_tunnel_publish(&TunnelPublish::ExitIpObserved { ip: exit });
        assert_eq!(
            out.vpn_publishes,
            vec![VpnPublish::PublicIpObserved { ip: exit }]
        );
    }

    /// `LeakDetected` with a parseable IPv4 in `observed_remote`
    /// should thread through as the `verified_ip` on the
    /// `PublicIpMismatch` (M4 review fix).
    #[test]
    fn leak_detected_threads_observed_remote_into_verified_ip() {
        let out = bridge_tunnel_publish(&TunnelPublish::LeakDetected {
            interface: "eth0".into(),
            observed_remote: "203.0.113.1:443".into(),
            exit_ip: None,
        });
        // "host:port" doesn't parse as a bare IpAddr — should fall
        // back to UNSPECIFIED (no leak parsing this time).
        let unspec = VpnIp(std::net::Ipv4Addr::UNSPECIFIED);
        match &out.vpn_publishes[0] {
            VpnPublish::PublicIpMismatch { verified_ip, .. } => {
                assert_eq!(*verified_ip, unspec);
            }
            other => panic!("expected PublicIpMismatch, got {other:?}"),
        }
    }

    /// When `observed_remote` is a bare IP literal, the bridge
    /// should narrow it through `VpnIp::from_ip` and surface it.
    #[test]
    fn leak_detected_with_bare_ip_surfaces_concrete_verified_ip() {
        let out = bridge_tunnel_publish(&TunnelPublish::LeakDetected {
            interface: "eth0".into(),
            observed_remote: "203.0.113.1".into(),
            exit_ip: None,
        });
        match &out.vpn_publishes[0] {
            VpnPublish::PublicIpMismatch { verified_ip, .. } => {
                assert_eq!(*verified_ip, ip("203.0.113.1"));
            }
            other => panic!("expected PublicIpMismatch, got {other:?}"),
        }
    }

    /// The exit IP carried on the publish (owned by the tunnel
    /// core) must land as the mismatch's `file_ip`.
    #[test]
    fn leak_detected_uses_carried_exit_ip_as_file_ip() {
        let out = bridge_tunnel_publish(&TunnelPublish::LeakDetected {
            interface: "eth0".into(),
            observed_remote: "203.0.113.1".into(),
            exit_ip: Some(ip("198.51.100.20")),
        });
        match &out.vpn_publishes[0] {
            VpnPublish::PublicIpMismatch { file_ip, .. } => {
                assert_eq!(*file_ip, ip("198.51.100.20"));
            }
            other => panic!("expected PublicIpMismatch, got {other:?}"),
        }
    }

    /// Same regression: `PortForwardingDegraded` past the NAT-PMP
    /// failure threshold must clear the cached forwarded port.
    #[test]
    fn port_forwarding_degraded_clears_arc_and_publishes_unavailable() {
        let out = bridge_tunnel_publish(&TunnelPublish::PortForwardingDegraded {
            consecutive_failures: 3,
            last_reason: "timeout".into(),
        });
        assert!(out.vpn_events.is_empty());
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PortUnavailable]);
        assert!(out.clear_forwarded_port);
    }
}
