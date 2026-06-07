//! Leak probe.
//!
//! Enumerates the kernel interfaces inside Windlass's network
//! namespace and reports whether any interface other than `lo` and
//! the configured tunnel interface carries an IPv4 or IPv6 address.
//!
//! The acceptance criterion in `docs/vpn-ownership.md` is:
//!
//! > The container's network namespace exposes `wg0` and `lo` as the
//! > only interfaces with internet routing after the tunnel is
//! > established.
//!
//! Anything beyond that — a leftover `eth0` from a misconfigured
//! `network_mode`, a stray bridge interface — would imply a path
//! the kill switch is not protecting.  Probe returns
//! [`windlass_tunnel_core::LeakProbeOutcome::LeakDetected`] in that
//! case; the tunnel core takes the health gate down on the
//! resulting event.
//!
//! ## Implementation
//!
//! Today the probe parses `ip -j addr show` JSON output via the
//! shared [`crate::command::Runner`].  Migration to in-process
//! `rtnetlink` follows the same shape: produce an
//! [`InterfaceSnapshot`], hand it to [`leak_outcome_from_snapshot`].

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

/// Applies the leak-decision rule to a snapshot: any interface with a
/// global address that isn't `lo` or the configured tunnel name
/// is a leak path.
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
