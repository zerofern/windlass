//! Leak probe — two layers, both required by
//! `docs/vpn-ownership.md`.
//!
//! ## Layer 1 — Interface enumeration
//!
//! Parses `ip -j addr show` JSON output via the shared
//! [`crate::command::Runner`] and reports whether any interface
//! other than `lo` and the configured tunnel interface carries a
//! non-link-local IPv4 or IPv6 address.  See
//! [`leak_outcome_from_snapshot`].  Strays are *candidates*, not a
//! verdict: the shipped tunnel topology deliberately attaches a
//! control-network interface (Postgres, dashboard ingress), and the
//! leak invariant per `docs/vpn-ownership.md` is that egress on it
//! is dropped by the kill switch — not that it doesn't exist.
//!
//! ## Layer 2 — Active connect-bind
//!
//! Tries an outbound TCP connect bound to a non-tunnel source IP.
//! Under a correctly configured namespace + nftables ruleset the
//! connect must fail (no route or firewall drop); a successful
//! connect proves we have an egress path the kill switch isn't
//! enforcing.  This is the authoritative leak verdict for strays
//! found by layer 1.  Implemented with `SO_BINDTODEVICE` on Linux
//! via [`active_connect_probe`]; platforms without that socket
//! option (only non-Linux test builds for this project) fall back
//! to the layer-1 outcome.

use std::collections::BTreeSet;

use serde::Deserialize;
use thiserror::Error;
use windlass_tunnel_core::LeakProbeOutcome;

/// One interface row in the `ip -j addr` output.  We only deserialize
/// the fields we read.
#[derive(Debug, Clone, Deserialize)]
struct IpAddrRow {
    ifname: String,
    #[serde(default)]
    addr_info: Vec<IpAddrInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct IpAddrInfo {
    family: String,
    #[serde(default)]
    scope: Option<String>,
}

/// Owned snapshot of the namespace's interfaces, derived from
/// `ip -j addr show` (or, later, from netlink).  Kept as a
/// distinct type so the leak-decision logic can be tested without
/// running `ip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceSnapshot {
    /// Interface names that carry at least one non-link-local IPv4
    /// or IPv6 address.  A set so order-insensitive.
    pub interfaces_with_global_addr: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeakProbeReport {
    pub snapshot: InterfaceSnapshot,
    pub outcome: LeakProbeOutcome,
}

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("failed to parse `ip -j addr show` output: {0}")]
    Parse(#[source] serde_json::Error),
}

/// Parses `ip -j addr show` JSON into a snapshot.
///
/// # Errors
///
/// Returns [`ProbeError::Parse`] if the JSON isn't shaped the way
/// `iproute2` produces it.
pub fn parse_ip_addr_show(json: &str) -> Result<InterfaceSnapshot, ProbeError> {
    let rows: Vec<IpAddrRow> = serde_json::from_str(json).map_err(ProbeError::Parse)?;
    let interfaces_with_global_addr = rows
        .into_iter()
        .filter_map(|row| {
            let has_global = row
                .addr_info
                .iter()
                .any(|a| (a.family == "inet" || a.family == "inet6") && !is_link_scope(a));
            if has_global { Some(row.ifname) } else { None }
        })
        .collect();
    Ok(InterfaceSnapshot {
        interfaces_with_global_addr,
    })
}

fn is_link_scope(info: &IpAddrInfo) -> bool {
    // `iproute2` reports link-scoped (fe80::/10 and 169.254/16)
    // addresses with scope = "link".  Those don't route anywhere and
    // are not a leak signal.
    matches!(&info.scope, Some(s) if s == "link")
}

/// Applies the layer-1 rule to a snapshot.
///
/// Any interface with a global address that isn't `lo` or the
/// configured tunnel name is a leak *candidate*.  On Linux the
/// active connect probe delivers the verdict for candidates;
/// expected control-network interfaces that the kill switch fences
/// resolve to `NoEgressDetected` there.
#[must_use]
pub fn leak_outcome_from_snapshot(
    snapshot: &InterfaceSnapshot,
    tunnel_interface: &str,
) -> LeakProbeOutcome {
    let stray: Vec<&str> = snapshot
        .interfaces_with_global_addr
        .iter()
        .map(String::as_str)
        .filter(|name| *name != "lo" && *name != tunnel_interface)
        .collect();
    if stray.is_empty() {
        return LeakProbeOutcome::NoEgressDetected;
    }
    // We list every stray interface so the operator sees them all.
    let interface = stray.join(",");
    LeakProbeOutcome::LeakDetected {
        interface,
        // We don't know which specific remote a leak would have
        // reached without an active connect attempt — that comes in a
        // follow-up.  For now, name what we found.
        observed_remote: "non-tunnel interface present in namespace".to_string(),
    }
}

/// Strays we'll try an active TCP connect against.  Public IPs that
/// stay routed reliably and don't host services we care about.
const ACTIVE_PROBE_TARGETS: &[(&str, u16)] = &[
    // Cloudflare DNS over HTTPS.  Reliable, public, harmless to ping.
    ("1.1.1.1", 443),
    ("1.0.0.1", 443),
];

/// Active leak probe — `SO_BINDTODEVICE` connects to each stray.
///
/// For each non-tunnel interface, try a connect to one of
/// [`ACTIVE_PROBE_TARGETS`].  A successful connect proves there's a
/// routable egress path that bypasses the kill switch.  Times out
/// fast (300 ms per attempt).  Failure modes (no targets reachable,
/// no non-tunnel interfaces, etc.) fold into `NoEgressDetected` —
/// the layer-1 enumeration probe is the primary signal; the active
/// probe is a verification.
#[cfg(target_os = "linux")]
pub fn active_connect_probe(
    snapshot: &InterfaceSnapshot,
    tunnel_interface: &str,
) -> LeakProbeOutcome {
    use std::time::Duration;

    let stray: Vec<&str> = snapshot
        .interfaces_with_global_addr
        .iter()
        .map(String::as_str)
        .filter(|name| *name != "lo" && *name != tunnel_interface)
        .collect();
    if stray.is_empty() {
        // Layer 1 already covers "no stray interface to probe".
        return LeakProbeOutcome::NoEgressDetected;
    }

    for iface in &stray {
        for (host, port) in ACTIVE_PROBE_TARGETS {
            if try_bound_connect(iface, host, *port, Duration::from_millis(300)).is_ok() {
                return LeakProbeOutcome::LeakDetected {
                    interface: (*iface).to_string(),
                    observed_remote: format!("{host}:{port}"),
                };
            }
        }
    }
    LeakProbeOutcome::NoEgressDetected
}

#[cfg(target_os = "linux")]
fn try_bound_connect(
    iface: &str,
    host: &str,
    port: u16,
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let addr: std::net::IpAddr = host.parse().map_err(std::io::Error::other)?;
    let domain = match addr {
        std::net::IpAddr::V4(_) => socket2::Domain::IPV4,
        std::net::IpAddr::V6(_) => socket2::Domain::IPV6,
    };
    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    // SO_BINDTODEVICE binds outgoing traffic to the named interface,
    // bypassing the routing table.  This forces the connect to use a
    // non-tunnel path; under a correct ruleset the firewall drops it.
    let iface_len = libc::socklen_t::try_from(iface.len())
        .map_err(|_| std::io::Error::other("interface name too long"))?;
    let bind_res = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            iface.as_ptr().cast(),
            iface_len,
        )
    };
    if bind_res != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let sock_addr = std::net::SocketAddr::new(addr, port).into();
    let tcp_std: std::net::TcpStream = match socket.connect_timeout(&sock_addr, timeout) {
        Ok(()) => socket.into(),
        Err(e) => return Err(e),
    };
    tcp_std.set_nonblocking(false)?;
    // We don't actually want to talk to the target — just confirm the
    // socket opened.  Drop it to close the connection.
    drop(tcp_std);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn active_connect_probe(
    snapshot: &InterfaceSnapshot,
    tunnel_interface: &str,
) -> LeakProbeOutcome {
    // Non-Linux targets (currently only tests in CI) don't have
    // SO_BINDTODEVICE.  Fall back to the layer-1 outcome.
    leak_outcome_from_snapshot(snapshot, tunnel_interface)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `ip -j addr show` output: a tunnel interface and `lo`
    /// only — the kill-switch-correct state.
    const HEALTHY_JSON: &str = r#"[
        {"ifname":"lo","addr_info":[
            {"family":"inet","scope":"host"},
            {"family":"inet6","scope":"host"}
        ]},
        {"ifname":"wg0","addr_info":[
            {"family":"inet","scope":"global"}
        ]}
    ]"#;

    /// Same but with a stray `eth0` carrying a global address — a
    /// misconfigured namespace where the kill switch can't protect
    /// us.
    const LEAKY_JSON: &str = r#"[
        {"ifname":"lo","addr_info":[
            {"family":"inet","scope":"host"}
        ]},
        {"ifname":"wg0","addr_info":[
            {"family":"inet","scope":"global"}
        ]},
        {"ifname":"eth0","addr_info":[
            {"family":"inet","scope":"global"}
        ]}
    ]"#;

    /// IPv6 link-local only — common on real hosts.  Should NOT
    /// register as a leak.
    const LINK_LOCAL_ONLY_JSON: &str = r#"[
        {"ifname":"lo","addr_info":[]},
        {"ifname":"wg0","addr_info":[
            {"family":"inet","scope":"global"}
        ]},
        {"ifname":"docker0","addr_info":[
            {"family":"inet6","scope":"link"}
        ]}
    ]"#;

    #[test]
    fn healthy_namespace_no_leak() {
        let snap = parse_ip_addr_show(HEALTHY_JSON).unwrap();
        let outcome = leak_outcome_from_snapshot(&snap, "wg0");
        assert!(matches!(outcome, LeakProbeOutcome::NoEgressDetected));
    }

    #[test]
    fn stray_interface_is_leak() {
        let snap = parse_ip_addr_show(LEAKY_JSON).unwrap();
        let outcome = leak_outcome_from_snapshot(&snap, "wg0");
        match outcome {
            LeakProbeOutcome::LeakDetected { interface, .. } => {
                assert!(interface.contains("eth0"));
            }
            other => panic!("expected LeakDetected, got {other:?}"),
        }
    }

    #[test]
    fn link_local_only_is_not_a_leak() {
        // docker0 here only carries an IPv6 link-local — there's no
        // route to the internet via it, so it must not flip the
        // probe.
        let snap = parse_ip_addr_show(LINK_LOCAL_ONLY_JSON).unwrap();
        let outcome = leak_outcome_from_snapshot(&snap, "wg0");
        assert!(matches!(outcome, LeakProbeOutcome::NoEgressDetected));
    }

    #[test]
    fn host_scope_lo_is_not_a_leak() {
        // `lo`'s host-scoped addresses aren't a leak even though they
        // pass the "global"-shaped filter (we exclude `lo` by name).
        let snap = parse_ip_addr_show(HEALTHY_JSON).unwrap();
        let outcome = leak_outcome_from_snapshot(&snap, "wg0");
        assert!(matches!(outcome, LeakProbeOutcome::NoEgressDetected));
    }

    #[test]
    fn invalid_json_is_typed_error() {
        let err = parse_ip_addr_show("not json").unwrap_err();
        assert!(matches!(err, ProbeError::Parse(_)));
    }

    #[test]
    fn custom_tunnel_name_is_respected() {
        // Caller may name the tunnel something other than wg0.
        let json = r#"[
            {"ifname":"vpn0","addr_info":[{"family":"inet","scope":"global"}]}
        ]"#;
        let snap = parse_ip_addr_show(json).unwrap();
        let outcome = leak_outcome_from_snapshot(&snap, "vpn0");
        assert!(matches!(outcome, LeakProbeOutcome::NoEgressDetected));
        let outcome2 = leak_outcome_from_snapshot(&snap, "wg0");
        assert!(matches!(outcome2, LeakProbeOutcome::LeakDetected { .. }));
    }
}
