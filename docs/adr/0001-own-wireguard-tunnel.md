---
status: accepted
---

# Own the WireGuard tunnel in-process instead of using Gluetun

Gluetun's container can stay healthy while its WireGuard handshake is dead,
leaving Windlass's sans-IO recovery primitives blind to the actual fault
and forcing every HTTP client to opt into a proxy or risk leaking. We own
the WireGuard tunnel inside the Windlass process so leak prevention becomes
a namespace+firewall property (no non-tunnel route exists), recovery
primitives act on tunnel state directly, and the cross-process file-watch
+ proxy + Docker-health coordination collapses to one process Windlass
deploys and operates itself. Tradeoff accepted: Windlass becomes Linux-only
and ships its own privileged netlink/firewall shell.
