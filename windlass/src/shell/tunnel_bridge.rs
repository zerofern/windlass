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
/// should produce.  `tunnel_inside_ip` is the tunnel's interface
/// address from `wg.conf` — used as a dedup placeholder until a
/// real exit-IP query lands.  `exit_ip` is the latest exit IP the
/// tunnel core knows (from `TunnelPublish::ExitIpObserved` after
/// the shell's HTTP query); if present, the bridge uses it in
/// preference to the inside address.
#[must_use]
pub(super) fn bridge_tunnel_publish(
    publish: &TunnelPublish,
    tunnel_inside_ip: Option<VpnIp>,
    exit_ip: Option<VpnIp>,
) -> TunnelBridgeOutput {
    let observed_ip = exit_ip.or(tunnel_inside_ip);
    match publish {
        TunnelPublish::Up | TunnelPublish::Recovered => TunnelBridgeOutput {
            vpn_events: vec![VpnEvent::ContainerHealthy],
            vpn_publishes: observed_ip
                .map(|ip| vec![VpnPublish::PublicIpObserved { ip }])
                .unwrap_or_default(),
            clear_forwarded_port: false,
        },
        TunnelPublish::Down { .. } | TunnelPublish::Stuck { .. } => TunnelBridgeOutput {
            vpn_events: vec![VpnEvent::ContainerUnhealthy],
            vpn_publishes: vec![VpnPublish::PublicIpUnavailable],
            // Going Down implies the tunnel's port forwarding is
            // also gone, so clear the cached port.
            clear_forwarded_port: true,
        },
        TunnelPublish::LeakDetected { .. } => {
            let zero = VpnIp(std::net::Ipv4Addr::UNSPECIFIED);
            TunnelBridgeOutput {
                vpn_events: vec![VpnEvent::ContainerUnhealthy],
                vpn_publishes: vec![VpnPublish::PublicIpMismatch {
                    file_ip: observed_ip.unwrap_or(zero),
                    verified_ip: zero,
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
    fn up_with_known_inside_ip_publishes_observed() {
        let out = bridge_tunnel_publish(&TunnelPublish::Up, Some(ip("10.2.0.2")), None);
        assert_eq!(out.vpn_events, vec![VpnEvent::ContainerHealthy]);
        assert_eq!(
            out.vpn_publishes,
            vec![VpnPublish::PublicIpObserved { ip: ip("10.2.0.2") }]
        );
        assert!(!out.clear_forwarded_port);
    }

    #[test]
    fn up_with_exit_ip_prefers_exit_over_inside() {
        let out = bridge_tunnel_publish(
            &TunnelPublish::Up,
            Some(ip("10.2.0.2")),
            Some(ip("203.0.113.10")),
        );
        assert_eq!(
            out.vpn_publishes,
            vec![VpnPublish::PublicIpObserved {
                ip: ip("203.0.113.10")
            }]
        );
    }

    #[test]
    fn recovered_publishes_observed_same_as_up() {
        let out = bridge_tunnel_publish(&TunnelPublish::Recovered, Some(ip("10.2.0.2")), None);
        assert!(out.vpn_publishes.iter().any(|p| matches!(
            p,
            VpnPublish::PublicIpObserved { ip } if *ip == self::ip("10.2.0.2")
        )));
    }

    #[test]
    fn down_clears_forwarded_port_and_publishes_unavailable() {
        let out = bridge_tunnel_publish(
            &TunnelPublish::Down {
                reason: "test".into(),
                since: chrono::Utc::now(),
            },
            Some(ip("10.2.0.2")),
            None,
        );
        assert_eq!(out.vpn_events, vec![VpnEvent::ContainerUnhealthy]);
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PublicIpUnavailable]);
        assert!(out.clear_forwarded_port);
    }

    #[test]
    fn stuck_publishes_unhealthy_and_unavailable() {
        let out = bridge_tunnel_publish(
            &TunnelPublish::Stuck {
                reason: "test".into(),
                since: chrono::Utc::now(),
                attempted_recoveries: 3,
            },
            Some(ip("10.2.0.2")),
            None,
        );
        assert_eq!(out.vpn_events, vec![VpnEvent::ContainerUnhealthy]);
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PublicIpUnavailable]);
        assert!(out.clear_forwarded_port);
    }

    #[test]
    fn leak_detected_flips_admission_gate_and_clears_port() {
        let out = bridge_tunnel_publish(
            &TunnelPublish::LeakDetected {
                interface: "eth0".into(),
                observed_remote: "203.0.113.1".into(),
            },
            Some(ip("10.2.0.2")),
            None,
        );
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
        let out = bridge_tunnel_publish(&TunnelPublish::PortReady { port }, None, None);
        assert_eq!(out.vpn_events, vec![VpnEvent::PortFileChanged { port }]);
        assert!(out.vpn_publishes.is_empty());
        assert!(!out.clear_forwarded_port);
    }

    /// Regression test for Phase 8 review #2: previously the bridge
    /// dropped `PortUnavailable` entirely, leaving qBit configured
    /// against a port the tunnel no longer held.
    #[test]
    fn port_unavailable_clears_arc_and_publishes_unavailable() {
        let out = bridge_tunnel_publish(&TunnelPublish::PortUnavailable, None, None);
        assert!(out.vpn_events.is_empty());
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PortUnavailable]);
        assert!(out.clear_forwarded_port);
    }

    /// Same regression: `PortForwardingDegraded` past the NAT-PMP
    /// failure threshold must clear the cached forwarded port.
    #[test]
    fn port_forwarding_degraded_clears_arc_and_publishes_unavailable() {
        let out = bridge_tunnel_publish(
            &TunnelPublish::PortForwardingDegraded {
                consecutive_failures: 3,
                last_reason: "timeout".into(),
            },
            None,
            None,
        );
        assert!(out.vpn_events.is_empty());
        assert_eq!(out.vpn_publishes, vec![VpnPublish::PortUnavailable]);
        assert!(out.clear_forwarded_port);
    }
}
