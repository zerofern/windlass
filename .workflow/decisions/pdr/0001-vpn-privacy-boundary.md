# PDR-0001: VPN privacy boundary

Status: Accepted

## Context

Windlass needs VPN privacy while providing a cohesive MAM experience. The owner
is considering ordinary systemd services on NixOS rather than containers.
The required privacy behaviour must remain clear across deployment choices.

## Decision

Torrent traffic and MAM requests associated with the seedbox must use the VPN.
If VPN protection is unavailable, these operations stop until protection returns.
Local management access can remain available so the user can diagnose problems.

## Rationale

Continuing protected operations through an unprotected connection would violate
the owner's privacy requirement. Availability must yield to privacy for these
operations. Keeping local management available lets the user understand and
resolve a failure without relaxing the boundary.

## Scope

This decision governs product behaviour regardless of deployment model or which
component manages the VPN. It does not select a VPN provider, isolation mechanism,
or recovery implementation. It does not require unrelated host traffic to use
the VPN.

## Related records

- [Direction](../../direction.md#vpn-privacy)
