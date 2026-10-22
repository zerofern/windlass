# ADR-0001: One application core initially

Status: Accepted

## Context

Windlass needs coordinated behaviour across MAM requests, persistent pause state,
configuration, downloads, library organisation, disk monitoring, and notifications.
The owner wants strong test coverage and prefers one core initially. The proof
of concept's multiple cores and generic runtime do not constrain the new design.

## Decision

Use one pure, synchronous application core. It owns application state and handles
events sequentially, returning actions for the imperative shell to execute.
Time and external observations enter as explicit inputs. Completion and failure
of actions return as events.

Internal modules and domain types may organise this core. They do not become
independent cores with separate event loops or inter-core messaging.

Use one input stream for user commands, external observations, operation results,
and timer expiry. The shell supplies time when dispatching each event. Only the
core decides policy; shell tasks execute effects concurrently without borrowing
the core. Returning multiple actions does not imply completion ordering.
Dependent effects wait for explicit completion events.

Outstanding operations have identities. The core rejects late or superseded
results that no longer apply. Cancellation cannot recall a request already sent.

Timers are named one-shot actions with replacement and cancellation. Each timer
instance has an identity, so obsolete expiry events are harmless. The core owns
deadlines and rechecks eligibility on expiry; the shell only waits and reports.
Periodic work explicitly schedules its next occurrence. No fixed tick is required.

The shell supplies monotonic time for active deadlines and wall time for durable
cooldowns. Startup loads configuration and durable state before external work
can begin. A missing or unreadable store must not silently enable requests.
Request blocking can take effect while a save is pending, but durable success
is acknowledged only after the save for that change succeeds.

## Rationale

One core gives pause commands, configuration changes, timers, and external results
one ordering. Tests can exercise their interactions directly through state and
returned actions. This avoids needing a messaging runtime to test coordinated
behaviour and keeps request policy in one place.

MAM request permission, endpoint timing, and retry decisions belong in this core.
The shell translates permitted actions into API calls. Core tests must cover
manual commands, retries, resume, and restart without bypassing API restrictions.
The core also owns the shared MAM token bucket, calculating refill from supplied
time. Token availability does not override endpoint restrictions or other gates.

## Scope

This is the initial application structure. It does not select persistence,
transport libraries, or a migration strategy. The shell owns asynchronous I/O;
NixOS owns the VPN. The core neither performs I/O nor reads the clock.

## Consequences

Core tests use concrete inputs and assert state and actions without mocks or
dependency injection. Shell and operating-system guarantees still require
integration tests. Reconsider multiple cores only when a demonstrated need
justifies their coordination cost.

Use a local fake MAM HTTP server for shell integration tests, with synthetic
credentials, controllable responses, and a request journal. Cover successful
calls, authentication failures, rate-limit responses, malformed responses, and
delayed requests for cancellation tests. Tests must not require live MAM access.

Derive endpoint contracts and fixtures from the supplied docs/MAM sources,
independently of client implementation. Passing against the fake demonstrates
agreement with those fixtures, not live MAM conformance. Unsupported endpoints
must fail visibly rather than return generic success.

The archived POC/windlass-testkit fake is a reuse candidate, not authority for the
new contract. Audit its routes, response shapes, and placeholder handlers before
reuse. The fake supports shell tests; core tests still use events and actions
directly. Tests of rate-limit timing should use controlled time rather than wait
through real API cooldowns.

## Related records

- [Direction](../../direction.md)
- [VPN privacy boundary](../pdr/0001-vpn-privacy-boundary.md)
- [Application foundation](../../features/B-0008/brief.md)
- [SQLite persistence](0002-sqlite-persistence.md)
