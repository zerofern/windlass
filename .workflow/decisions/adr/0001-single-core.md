# ADR-0001: One application core initially

Status: Accepted

## Context

Windlass must coordinate MAM requests, persistent controls, downloads, and library
work across multiple input sources. The owner chose one core with strong test
coverage. The proof of concept's generic runtime does not constrain this design.

## Decision

Use one pure, synchronous core that owns its in-memory state. Expose that state
read-only, without a separate status representation. Handle events sequentially,
returning actions to the shell. Time and observations are inputs; action outcomes
return as events.

Internal modules may organise the core without independent event loops.

Separate the runner, core, and shell. The runner owns the core, consumes one event
stream, and dispatches returned actions through the shell. The shell executes
effects concurrently and returns result events. Dependent effects wait for
explicit completion events; action order does not establish completion order.

Each producer pairs its event with occurrence time before enqueueing it. Preserve
that timestamp through delivery. Process events in queue order, without sorting
by timestamp. Processing time, if needed, is a separate input. The console and
future HTTP API share the event ingress and observe read-only core state. An API
validates public requests before translating them into permitted domain events.
Queue acceptance does not acknowledge durable success.

The ingress applies backpressure. Shutdown stops new submissions and drains
accepted events and all resulting effects, including follow-up storage actions.
Remaining producer handles cannot keep a requested shutdown alive.

Start without operation identifiers. Result events name their operation. The
core permits one outstanding MAM preference storage operation, issuing another
after completion while other events remain responsive. Late results cannot undo
newer controls. This limit does not serialise unrelated I/O. Cancellation cannot
recall a request already sent.

Timers are named one-shot actions with replacement and cancellation. Each timer
instance has an identity, so obsolete expiry events are harmless. The core owns
deadlines and rechecks eligibility on expiry; the shell only waits and reports.
Periodic work explicitly schedules its next occurrence. No fixed tick is required.

Clock readings contain monotonic time for active deadlines and wall time for
durable cooldowns. A startup event prompts the core to request configuration and
stored facts through actions. The shell reads only when requested. The core's
state stays in memory; writes persist specific facts rather than snapshots.
Missing or unreadable storage cannot silently enable requests. Blocking can take
effect during a save; durable success requires confirmed persistence.

## Rationale

One runner gives requests and observations one ordering across input sources.
Occurrence timestamps survive queue delay. Separating event delivery from action
execution keeps console and HTTP details outside the core's operating model.
Core tests exercise coordination through state and actions without a runtime.

MAM admission, endpoint timing, retries, and the shared token bucket belong in the
core. Calculate refill from supplied time. Token availability does not override
other admission gates. Tests cover manual requests, retries, resume, and restart
without bypassing API restrictions.

## Scope

This is the initial application structure. It does not select persistence,
transport libraries, or a migration strategy. The shell owns asynchronous I/O;
NixOS owns the VPN. The core neither performs I/O nor reads the clock.

## Consequences

Core tests assert concrete state and actions without mocks or dependency
injection. Shell guarantees require integration tests. Reconsider multiple cores
when a demonstrated need justifies their coordination cost.

Use a local fake MAM HTTP server with synthetic credentials, controllable
responses, and a request journal. Cover success, authentication failures, rate
limits, malformed responses, and delayed requests for cancellation. Tests must
not require live MAM access.

Derive contracts and fixtures from docs/MAM independently of client code. Passing
against the fake establishes agreement with those fixtures, not live conformance.
Unsupported endpoints must fail visibly.

Audit the archived POC/windlass-testkit fake's routes, response shapes, and
placeholder handlers before reuse. It supports shell tests; core tests use events
and actions directly. Test timing with controlled time instead of real cooldowns.

## Related records

- [Direction](../../direction.md)
- [VPN privacy boundary](../pdr/0001-vpn-privacy-boundary.md)
- [Application foundation](../../features/B-0008/brief.md)
- [SQLite persistence](0002-sqlite-persistence.md)
