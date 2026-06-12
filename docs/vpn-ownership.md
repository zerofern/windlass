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

If Windlass owns the network namespace and tunnel control plane, leak
prevention becomes a property of the namespace and firewall, not the
HTTP client configuration.  Wrong code cannot leak because there is
no non-tunnel path for packets to take, except the explicitly allowed
underlay traffic needed to establish the WireGuard session itself.

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

## Ownership boundary

This redesign does not mean business logic performs privileged
networking operations.  It means Windlass owns the VPN control plane
inside its process boundary while preserving the existing
functional-core / imperative-shell split.

The tunnel core is a pure, sans-IO state machine.  It receives typed
events such as handshake observed, endpoint unreachable, NAT-PMP
lease granted, NAT-PMP renewal failed, leak probe succeeded, leak
probe failed, and timer fired.  It returns typed actions such as
configure interface, install firewall policy, renew lease, rotate
endpoint, run leak probe, and publish tunnel state.

The tunnel shell is Linux-only and executes the privileged work:
netlink/WireGuard interface configuration, firewall rule changes,
NAT-PMP packet exchange, route inspection, DNS/leak probes, and
`wg show`-equivalent introspection.  It translates I/O results back
into tunnel-core events.

Docker remains the owner of container lifecycle and namespace
relationships.  The tunnel core owns tunnel state; the Docker core
owns dependent containers; the domain core owns cross-service policy
between them.  qBit, MAM, and disk cores continue to consume typed
facts through the existing runtime/fanout architecture.

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
by firewall rules, except for the minimal underlay allowlist required
to reach the configured WireGuard endpoint.  Code that constructs a
`reqwest::Client` without any proxy still cannot leak because the
kernel has no non-tunnel route to use for ordinary internet traffic.

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
Windlass negotiated with Proton.  Windlass restarts, upgrades, and
configuration restarts have an explicit qBit lifecycle policy so qBit
never continues running with an ambiguous or stale namespace.

**Privileged operations are narrow.**  `NET_ADMIN` is treated as a
deployment requirement for the tunnel shell, not a license for broad
privileged behavior throughout the program.  Privileged operations are
small, typed, observable shell actions.  If the container/runtime can
drop capabilities after boot-time setup without breaking renewals or
recovery, the design should do so; otherwise the residual risk is
documented and covered by integration tests.

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

If the `Endpoint` in the file is a hostname, the design must define
how it is resolved without creating a DNS leak.  Acceptable policies
are: require an IP literal endpoint, resolve the endpoint before
deployment, or implement an explicit pre-tunnel DNS allowlist path
that is covered by the same leak-prevention tests as the WireGuard
underlay.

**Operating system.**  Linux only.  Kernel 5.6 or later (kernel
WireGuard support).  The deployment environment is Linux 6.12.

**Deployment.**  Docker Compose.  qBittorrent runs in a separate
container that shares Windlass's network namespace via
`network_mode: container:windlass`.  Because that ties qBit's network
namespace lifecycle to Windlass's container lifecycle, the deployment
must specify whether Windlass restarts also restart qBit, pause qBit,
or otherwise prevent qBit from running against a stale namespace.

**Privilege.**  Windlass's container requires `NET_ADMIN`.  This
covers both kernel WireGuard interface management and firewall
configuration inside the namespace.  The privileged surface must be
limited to the tunnel shell; pure cores do not call privileged APIs,
perform I/O, or inspect process-global network state directly.

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
- Windlass refuses to start if the WireGuard endpoint cannot be
  resolved according to the configured endpoint-resolution policy.
- Before the tunnel is established, firewall policy allows only
  loopback and the explicit WireGuard underlay path required to reach
  the configured peer endpoint.
- After the tunnel is established, Windlass refuses to start if it
  can construct a working ordinary internet connection that does not
  traverse the tunnel.
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
- qBit either shares the live Windlass namespace or is stopped/paused;
  it never continues running after Windlass namespace replacement with
  stale egress assumptions.

**Failure and recovery.**

- A simulated handshake stall (no peer response for the timeout
  window) is detected and surfaces via the watchdog.  Recovery is
  driven by kernel `WireGuard`'s automatic re-key on its rekey timer
  combined with the operator-configurable endpoint rotation Windlass
  performs after `stall_count_before_rotate` consecutive stalls.
  No Windlass process restart is required.
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
- Capture stores typed summaries by default and redacts or suppresses
  secrets such as private keys, session cookies, and sensitive
  configuration material.
- An operator can dump the last N raw netlink and NAT-PMP packet
  pairs without restarting Windlass or attaching a debugger, subject
  to the same redaction rules.
- State transitions in the tunnel core carry causal links back to the
  events that produced them, consistent with how every other core
  records causality.

**Leak prevention.**

- The container's network namespace exposes `wg0` and `lo` as the
  only interfaces with internet routing after the tunnel is
  established.
- The only permitted non-tunnel underlay traffic is the WireGuard
  endpoint path required to establish and maintain the tunnel.
- Firewall rules drop egress on any other interface, including IPv6
  routes that the host stack might offer.
- DNS resolution inside the namespace uses the tunnel-routed
  resolver, not the host resolver, except for any explicitly designed
  and tested pre-tunnel endpoint-resolution path.

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
