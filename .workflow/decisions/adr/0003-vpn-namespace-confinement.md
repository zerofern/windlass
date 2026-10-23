# ADR-0003: VPN network namespace confinement

Status: Accepted

## Context

The [privacy boundary](../pdr/0001-vpn-privacy-boundary.md) requires torrent
traffic and seedbox-associated MAM requests to use the VPN and to stop when it
fails. [Direction](../../direction.md#deployment) left the enforcement mechanism
open. The owner uses a ProtonVPN subscription with WireGuard and port forwarding.

The proof of concept relied on application code choosing a proxy for MAM requests.
A client built without that proxy silently used the host network. The owner also
requires notifications to reach ntfy.sh while the VPN is down, and the dashboard
to remain reachable over Tailscale but not through the VPN.

## Decision

NixOS creates a dedicated network namespace whose only internet route is a
ProtonVPN WireGuard interface. qBittorrent and Windlass both run inside it. When
the tunnel is down or being replaced, their internet traffic fails rather than
using another route. IPv6 is blocked inside the namespace and DNS uses Proton's
resolver through the tunnel.

The namespace has one private link to the host. Only two kinds of traffic use it:

- The dashboard listens on the namespace side of that link. The host forwards
  its Tailscale address, and optionally a LAN address, to the dashboard.
- A notification relay on the host forwards connections only to ntfy.sh on port
  443. Windlass's TLS session ends at ntfy.sh, so the relay carries encrypted
  bytes it cannot read. Notifications therefore work while the VPN is down.

Inside the namespace, the firewall admits inbound tunnel traffic only on the
forwarded torrent port. The dashboard and qBittorrent's WebUI never listen on
the tunnel interface. qBittorrent's WebUI listens on namespace-local loopback.

NixOS owns the namespace, the WireGuard interface, the firewall, the forwarding,
and the relay. Windlass owns the decisions that need its knowledge of the system:
ProtonVPN port-forwarding leases, verifying that the tunnel works, and choosing
when to fail over to another configured server. A narrowly permitted NixOS unit
performs the switch Windlass requests.

## Rationale

Leak prevention becomes a property of the network rather than of HTTP client
configuration. Code that forgets a proxy cannot reach MAM or peers outside the
tunnel because no such route exists. The relay is the only exception, and its
fixed destination cannot carry MAM requests.

Placing Windlass inside the namespace keeps its MAM traffic and its tunnel checks
on the same path as qBittorrent's traffic, so both share one exit IP. Keeping
privileged network changes in NixOS limits Windlass to requesting a known unit.

## Scope

This selects the confinement mechanism and the ownership split. It does not
select the NixOS module or scripts, the relay program, firewall syntax, the
failover unit's interface, or the external service used for tunnel checks. The
[VPN confinement brief](../../features/B-0010/brief.md) defines the behaviour.

## Consequences

- A request that would need a non-tunnel route fails visibly instead of leaking.
- Server failover interrupts torrent and MAM traffic briefly and changes the exit
  IP and forwarded port. Seedbox registration must then be refreshed.
- Each failover changes the exit IP, and MAM rules require a VPN IP that changes
  less than once an hour. Automatic failover is limited accordingly.
- Integration tests need a namespace, a WireGuard peer, and a fake NAT-PMP gateway;
  they must not depend on ProtonVPN or MAM.
- The relay and the dashboard forwarding are the only host-facing paths to audit.

## Related records

- [VPN privacy boundary](../pdr/0001-vpn-privacy-boundary.md)
- [Single core](0001-single-core.md)
- [Direction](../../direction.md#vpn-privacy)
- [VPN confinement, port forwarding, and failover](../../features/B-0010/brief.md)
- [qBittorrent integration](../../features/B-0007/brief.md)
