# VPN Ownership

Windlass owns its WireGuard tunnel in-process instead of routing through
a separate Gluetun container.  This document captures why, what we are
trying to achieve, the external constraints we have to work within, and
the acceptance criteria the new design must meet.

This is not a plan and not a description of the code changes.  Those
live elsewhere.  This document is the north star against which the
plan and the implementation are evaluated.

## Why

Three independent problems push toward in-process VPN ownership.

**1.  Gluetun loses connections it cannot recover from.**  In
production, Gluetun's container remains healthy from Docker's
perspective while the underlying WireGuard handshake has died.  The
§38 crash-recovery path in Windlass cannot help because Gluetun looks
fine from the outside.  The §31/§33 IP cross-check was added
specifically to detect this class of failure, but the recovery action
it triggers — restarting Gluetun — depends on Gluetun coming back
cleanly, which is precisely what is failing.

The recovery primitives Windlass already has (sans-IO state machines,
observability with pause/step, causal event records, the
restart-storm circuit breaker) are exactly what a stuck VPN tunnel
needs.  They cannot be applied to Gluetun because Gluetun is opaque.

**2.  The current leak surface is a code property, not a system
property.**  `MamClient::new` accepts `proxy_url: Option<&str>` and
silently uses the host network on `None`.  `VpnShell` constructs its
own `reqwest::Client` with its own proxy handling.  Any future client
that calls `reqwest::Client::builder()` without going through a
proxy is a fresh leak path.  The runtime cross-check (§31/§33)
detects leaks but does not prevent them.

If Windlass owns the only network egress its container has, leak
prevention becomes a property of the namespace and firewall, not the
HTTP client configuration.  Wrong code cannot leak because there is
no non-tunnel path for packets to take.

**3.  Cross-container coordination is the wrong abstraction.**  Today
Windlass watches `/tmp/gluetun/ip` and `/tmp/gluetun/forwarded_port`,
polls Docker for Gluetun's health, and depends on Gluetun's HTTP
proxy for outbound MAM traffic.  Each of these is a translation layer
between two processes with different lifecycles.  The translations
add latency, race windows, and failure modes that have no analogue in
the actual VPN protocol.

A single process that owns the tunnel sees handshake state, port
forwarding state, and connectivity state directly.  There are no
files to watch, no proxies to configure, and no other process to
coordinate with.

## Objectives

The redesign succeeds if the following are true.

**Tunnel resilience without process restart.**  A handshake timeout
triggers a re-key in the same process.  An unresponsive endpoint
triggers an endpoint rotation in the same process.  A failed port
forwarding renewal is retried in the same process.  Windlass restarts
become rare events tied to configuration changes or upgrades, not
recovery actions.

**Leak prevention as a system property.**  After boot, the only
internet egress Windlass's network namespace has is the WireGuard
interface.  A non-tunnel route is either absent or actively blocked
by firewall rules.  Code that constructs a `reqwest::Client` without
any proxy still cannot leak because the kernel has no non-tunnel
route to use.

**Single source of truth for VPN state.**  The tunnel's handshake
time, current endpoint, forwarded port, and observed public IP are
all readable from a single in-process structure.  Other cores read
state through typed publishes from the tunnel core, not from files
or by querying another container.

**Observability is first-class for the tunnel.**  Every state
transition, every netlink call, every NAT-PMP exchange goes through
the same observability pipeline as the existing cores.  Pause, step,
breakpoints, and HTTP-style exchange capture all work for tunnel
operations.  When the tunnel fails in a way the state machine did
not anticipate, the operator has enough raw data on hand to
diagnose without attaching a debugger.

**Notification-ready failure surface.**  Tunnel failures publish
typed events with enough context (reason, duration, last-known-good
state) that the future notifications subsystem can produce
specific, actionable messages without further code changes in the
tunnel core.

**qBit and Windlass share egress.**  Both processes appear to MAM
and to remote peers under the same public IP.  MAM's connectability
check continues to succeed.  qBit's forwarded port is the one
Windlass negotiated with Proton.

## External requirements

These are constraints we must accept; they are not choices we are
making.

**VPN provider.**  ProtonVPN.  This implies WireGuard transport and
NAT-PMP for port forwarding.  The implementation must speak Proton's
NAT-PMP dialect (port requests sent through the tunnel to the
gateway, 60-second TTL, renewal required).

**Configuration format.**  A ProtonVPN-generated `wg.conf` file
mounted into the container.  Windlass parses it at boot.  Hot-reload
is not required; configuration changes mean a Windlass restart.

**Operating system.**  Linux only.  Kernel 5.6 or later (kernel
WireGuard support).  The deployment environment is Linux 6.12.

**Deployment.**  Docker Compose.  qBittorrent runs in a separate
container that shares Windlass's network namespace via
`network_mode: container:windlass`.

**Privilege.**  Windlass's container requires `NET_ADMIN`.  This
covers both kernel WireGuard interface management and firewall
configuration inside the namespace.

**MAM connectability invariant.**  MAM marks a seedbox not
connectable if qBit's listen port is open on a different public IP
than the seedbox's registered IP.  qBit and Windlass must egress
under the same public IP at all times.

**MAM dynamic-seedbox rate limit.**  One call per hour, rolling
window, server-enforced.  Windlass already enforces this client-side.
The redesign must preserve the guard.

**MAM session and other external APIs.**  qBit WebUI, MAM session
cookie handling, disk monitoring, alert publishing, and every other
core's external interface continue to behave as they do today.  The
redesign changes what is below the HTTP client; what is above it
should be unaffected.

## Acceptance criteria

The redesign is complete when all of these can be demonstrated.

**Boot.**

- Windlass refuses to start if the WireGuard configuration file is
  absent, unreadable, or malformed.  The failure message names the
  problem.
- Windlass refuses to start if it can construct a working TCP
  connection that does not traverse the tunnel.
- The first MAM call after boot succeeds and reports the Proton egress
  IP, not the host IP.

**Steady state.**

- The tunnel handshake is renewed automatically.  `wg show`-equivalent
  introspection of the tunnel shows a recent handshake at all times
  during normal operation.
- The forwarded port granted by Proton's NAT-PMP service is held
  continuously.  A renewal failure triggers a retry; sustained failure
  publishes a typed event.
- qBittorrent's listen port equals the Proton-granted port.
- MAM reports `connectable: yes` for the seedbox.
- Windlass's IP (as observed locally via the tunnel) equals the IP
  MAM's `/json/jsonIp.php` reports for our requests.

**Failure and recovery.**

- A simulated handshake stall (no peer response for the timeout
  window) is detected, triggers a re-key, and recovers without a
  Windlass restart.
- A simulated endpoint outage (peer unreachable for the rotation
  threshold) triggers an endpoint rotation, if alternative peers are
  present in the configuration, without a Windlass restart.
- A simulated NAT-PMP failure (gateway not responding) triggers
  bounded retries; persistent failure publishes a typed event.
- A failure mode the state machine does not anticipate transitions
  the tunnel to a visible `Stuck` state rather than silently
  degrading.

**Observability.**

- The tunnel core appears in `/observability` alongside the existing
  cores.  Pause, step, and breakpoints behave the same way.
- Netlink operations and NAT-PMP exchanges are captured in the same
  ring as HTTP exchanges, typed appropriately.
- An operator can dump the last N raw netlink and NAT-PMP packet
  pairs without restarting Windlass or attaching a debugger.
- State transitions in the tunnel core carry causal links back to the
  events that produced them, consistent with how every other core
  records causality.

**Leak prevention.**

- The container's network namespace exposes `wg0` and `lo` as the
  only interfaces with internet routing.
- Firewall rules drop egress on any other interface, including IPv6
  routes that the host stack might offer.
- DNS resolution inside the namespace uses the tunnel-routed
  resolver, not the host resolver.

**Notification hook.**

- Tunnel failure publishes (`Down`, `Stuck`, `LeakDetected`, etc.)
  carry sufficient typed context (reason, duration, last known-good
  state) for a future notification mapper to produce a specific
  message without modifying tunnel-core code.

## Out of scope

Listing these explicitly so they do not creep into the work.

- **Other VPN providers.**  The design must not preclude eventual
  support for PIA, Mullvad, or Wireguard-native providers, but
  implementing them is not part of this redesign.
- **Hot-reload of the WireGuard configuration.**  Configuration
  changes require a Windlass restart.
- **IPv6 tunneling.**  IPv6 inside the tunnel is permitted if Proton's
  configuration includes it; routing arbitrary IPv6 traffic over the
  tunnel is not in scope.  IPv6 leak prevention via firewall is in
  scope.
- **TLS pinning or certificate management.**  Standard TLS verification
  through whatever CA bundle the base image carries is sufficient.
- **A web UI for editing WireGuard configuration.**  Configuration is
  a file the operator manages outside Windlass.
- **Kernel-bypass userspace WireGuard.**  Kernel WireGuard is the
  target.  Userspace fallback can be considered if a deployment
  environment lacks kernel support; the current target does not.
