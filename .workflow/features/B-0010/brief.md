# VPN confinement, port forwarding, and failover

Status: Draft
Feature ID: B-0010

## Goal

Keep all BitTorrent and seedbox-associated MAM traffic inside a ProtonVPN
WireGuard tunnel that fails closed. Keep the forwarded port current, prove the
tunnel works, and move to another configured server when it does not, while
notifications and the dashboard stay available.

## Stories and acceptance

### S1: Fail closed

Story: When the VPN is down, the user can trust that no torrent or MAM traffic
used another route.

Acceptance:

- qBittorrent and Windlass run in the namespace defined by
  [ADR-0003](../../decisions/adr/0003-vpn-namespace-confinement.md).
- With the tunnel down, connections from the namespace to peers, trackers, MAM,
  and the tunnel-check service fail. No IPv4 or IPv6 traffic leaves another way.
- qBittorrent is also bound to the WireGuard interface as a second safeguard.
- The only exits besides the tunnel are the dashboard link and the ntfy.sh relay.
  The relay refuses any other destination.
- Notifications reach ntfy.sh while the tunnel is down.
- The dashboard is reachable over Tailscale whether or not the tunnel works, and
  not through the tunnel.

### S2: Keep the forwarded port current

Story: Peers can reach qBittorrent through ProtonVPN's forwarded port.

Acceptance:

- Windlass requests UDP and TCP mappings from the ProtonVPN gateway with NAT-PMP,
  using 60-second leases renewed about every 45 seconds. The core owns the
  renewal timer; the shell exchanges NAT-PMP packets.
- When the granted port changes, Windlass sets qBittorrent's listen port through
  its API and confirms the change. qBittorrent's own UPnP and NAT-PMP are off.
- The dashboard shows the forwarded port, when it was last renewed, and whether
  qBittorrent uses it.
- A failed renewal is retried and marks the tunnel as suspect (S3).

### S3: Prove the tunnel works

Story: Windlass knows whether traffic actually flows through the tunnel, not just
whether the interface exists.

Acceptance:

- Windlass makes an HTTPS request to a neutral exit-IP service outside MAM, about
  every 15 minutes and immediately when problems are suspected: a failed lease
  renewal, a request timeout, startup, or after a server switch.
- These checks do not use MAM endpoints or consume MAM request tokens.
- VPN protection is available only after a recent successful check. A failed check
  makes protection unavailable, with the effects defined in
  [Direction](../../direction.md#vpn-privacy).
- The observed exit IP is shared with MAM initialization, which refreshes seedbox
  registration when it changes.
- Checks, results, and the exit IP appear in history.

### S4: Fail over to another server

Story: When one ProtonVPN server stops working, Windlass moves to another one
without the user intervening.

Acceptance:

- The deployment provides several ProtonVPN WireGuard configurations, one per P2P
  server, each generated with NAT-PMP enabled and kept as a NixOS secret.
- The core decides when to switch and which server is next, after repeated failed
  checks. The shell asks a narrowly permitted NixOS unit to switch; Windlass has
  no other privileged network access.
- Automatic switches respect MAM's requirement that the VPN IP change less than
  once an hour. When that limit blocks a switch, the dashboard says so and the
  user is notified.
- After a switch, Windlass rechecks the tunnel, renews the port mapping, updates
  qBittorrent, and lets MAM initialization refresh seedbox registration.
- Switches, their reasons, and outcomes appear in history and in notifications.

## Feature-wide constraints and acceptance

- Follow the [VPN privacy boundary](../../decisions/pdr/0001-vpn-privacy-boundary.md)
  and [namespace confinement](../../decisions/adr/0003-vpn-namespace-confinement.md).
- MAM access uses an ASN-locked API session, so registration survives exit IP
  changes within ProtonVPN's ASN.
- Core tests drive lease timing, check scheduling, protection state, and failover
  decisions with supplied time and events.
- Integration tests use a local namespace, a local WireGuard peer, a fake NAT-PMP
  gateway, and a fake exit-IP service. They never contact ProtonVPN or MAM.

## Scope

The NixOS namespace and relay design, ProtonVPN NAT-PMP port forwarding, tunnel
checks, VPN protection status, server failover, and their dashboard panels.

## Non-goals

Implementing WireGuard, VPN providers other than ProtonVPN, qBittorrent torrent
management (B-0007), and MAM registration requests (B-0004).

## Related records

- [ADR-0003: VPN namespace confinement](../../decisions/adr/0003-vpn-namespace-confinement.md)
- [MAM initialization](../B-0004/brief.md)
- [qBittorrent integration](../B-0007/brief.md)
- [Web dashboard](../B-0003/brief.md)
- [ProtonVPN references](../../references.md#protonvpn)

## Open questions and assumptions

- Do all ProtonVPN P2P servers share one ASN? The ASN-locked session depends on it.
- Which NixOS mechanism builds the namespace, and can it swap WireGuard
  configurations without restarting qBittorrent?
- Which exit-IP service, and how many failed checks trigger a switch?
- MAM initialization also observes the exit IP through MAM's `jsonIp.php` every
  five minutes. Should it rely on this feature's checks instead, keeping
  `jsonIp.php` for the ASN as MAM sees it?
- How is the failover unit permitted: a polkit rule, a socket, or another route?
- Should the dashboard also be forwarded to the LAN, or only to Tailscale?
- The user must tell MAM staff about VPN use, as MAM rule 1.2 requires.
