# Operator Readiness Work

This document tracks the work needed before Windlass is comfortable to use as
the daily operator for the Gluetun + qBittorrent + MAM stack.

The scope here is intentionally narrow: operator reliability, operator UI
trustworthiness, manual download safety, and compliance visibility. Book
recommendations, LLM profiles, and librarian automation are later phases unless
they directly block operator use.

## Goal

Windlass should be usable soon as an operator:

- Keep qBittorrent's listen port synced with Gluetun.
- Keep MAM seedbox/connectability state updated.
- Surface VPN, qBit, MAM, disk, torrent, and HnR status clearly.
- Let the user manually add a MAM torrent safely.
- Make the web UI reflect current state immediately after opening.
- Preserve enough event/action history to debug operator behavior.

## Story: Fix Initial UI State Snapshot On SSE Connect

Status: To Do

### Problem

When the web UI opens after Windlass has already booted, the dashboard can sit
waiting for a state update even though the operator has a valid current state.
The SSE stream sends the initial debug-mode flag, then only live observations.
If no new state-changing event happens, the dashboard has no `StateSnapshot` to
render.

This makes the operator look disconnected or empty during normal use.

### User Story

As the operator user, when I open or refresh the Windlass UI, I want the current
operator state to appear immediately, so I can trust the dashboard without
waiting for a new event.

### Acceptance Criteria

- A new `/api/v1/stream` SSE subscriber receives the latest known
  `StateSnapshot` during connection setup.
- The initial state is sent before or alongside live observations, so opening
  the dashboard after boot does not depend on a future state change.
- The existing initial `DebugModeChanged` observation still reaches the client.
- The dashboard no longer shows the empty/waiting state after connecting to an
  already-running operator.
- Add or update backend/web tests that prove a fresh stream subscriber receives
  initial state.
- `just check` passes.
- Frontend build passes.

### Implementation Notes

- Likely area: `windlass-web/src/routes/stream.rs`.
- The stream handler already has access to `AppState`.
- Use the existing latest-state source rather than inventing a second UI state
  cache.
- Preserve the broadcast stream for live observations.

## Backlog

These are candidate stories to spec next:

- Introduce the generic service runtime for `Machine + Shell + TopicFanout`.
- Move the VPN core onto the service runtime.
- Move the qBittorrent core onto the service runtime.
- Move the MAM core onto the service runtime.
- Move the domain core onto the service runtime.
- Move the DB core onto the service runtime.
- Replace the direct publish bridge with typed subscriptions.
- Use `Timed<Event>` end to end.
- Make the dashboard and chaos page use one shared state display model.
- Keep route state and background data fresh when tabs/pages are not active.
- Improve debug page event/action queue visibility.
- Allow clicking an event or action in debug view to set a breakpoint for that
  variant.
- Clarify the manual download happy path and blocked states in the UI.
- Make HnR/compliance risk visible enough that unsafe manual deletion is hard
  to miss.

## Story: Introduce Generic Service Runtime

Status: To Do

### Problem

Windlass has the shared `Machine`, `Shell`, `Timed`, and `TopicFanout`
abstractions, but the runtime still wires service cores together through a
direct bridge. Each service loop currently has to be assembled manually.

### User Story

As the maintainer, I want a generic runtime for sans-I/O services, so each
external-system core can run the same way and new service cores do not need
custom orchestration.

### Acceptance Criteria

- Add a generic service runner that owns a machine, shell, event channel,
  command channel, and topic fanout.
- The runner calls `Machine::handle` for timed events and dispatches returned
  actions through the shell.
- The runner calls `Machine::handle_command` for external commands and returns
  typed responses to the command sender.
- Publish messages are routed through `TopicFanout`.
- The runtime is covered by focused tests using a small fake machine and shell.

## Story: Move VPN Core Onto Service Runtime

Status: To Do

### Problem

The VPN core exists, but important runtime behavior is still bridged through the
legacy event loop. For example, the core can emit a health-poll timer, but the
current shell path does not fully round-trip that timer back into the VPN core.

### User Story

As the operator user, I want VPN monitoring to be owned by the VPN service
runtime, so Gluetun health and forwarded-port changes are handled consistently.

### Acceptance Criteria

- VPN runtime uses `VpnMachine` and a VPN shell through the generic service
  runtime.
- Gluetun health polling round-trips as `VpnTimer::HealthPoll`.
- Port-file read retry round-trips as `VpnTimer::PortReadRetry`.
- File watcher and Docker health callbacks enter the VPN runtime as
  `Timed<VpnEvent>`.
- VPN publishes `Connected`, `Disconnected`, `PortReady`, and
  `PortUnavailable` through topic fanout.
- Existing VPN recovery integration tests continue to pass.

## Story: Move qBittorrent Core Onto Service Runtime

Status: To Do

### Problem

The qBittorrent core owns auth, listen-port convergence, and torrent refresh
decisions, but the runtime bridge still maps some timers through legacy wakeup
ids. `TorrentRefresh` is currently only partially wired.

### User Story

As the operator user, I want qBittorrent behavior to run through the qBit
service runtime, so authentication, port sync, retries, and torrent refreshes
are controlled by one qBit-specific core.

### Acceptance Criteria

- qBit runtime uses `QbitMachine` and a qBit shell through the generic service
  runtime.
- Auth retry, sync retry, and torrent refresh timers round-trip as
  `QbitTimer` events.
- qBit shell executes login, preference reads, listen-port updates, torrent
  listing, pause, and resume actions.
- qBit publishes availability, listen-port, and torrent updates through topic
  fanout.
- Existing qBit unit and integration tests continue to pass.

## Story: Move MAM Core Onto Service Runtime

Status: To Do

### Problem

The MAM core owns status refresh, rate limits, and seedbox convergence, but the
runtime boundary still has unclear semantics around `UpdateSeedboxPort { port }`.

### User Story

As the operator user, I want MAM behavior to run through the MAM service
runtime, so connectability, rate limits, and seedbox updates are handled by one
MAM-specific core.

### Acceptance Criteria

- MAM runtime uses `MamMachine` and a MAM shell through the generic service
  runtime.
- Status retry and rate-limit expiry timers round-trip as `MamTimer` events.
- MAM shell executes status fetch and seedbox update actions.
- Clarify whether the seedbox update action truly needs a port argument. If the
  MAM endpoint does not accept a port, rename the action to model the real
  operation.
- MAM publishes availability, connectability, and seedbox state through topic
  fanout.
- Existing MAM and operator integration tests continue to pass.

## Story: Move Domain Core Onto Service Runtime

Status: To Do

### Problem

The domain core is the main cross-system policy machine, but it is currently
driven by the `ServiceCores` bridge. That keeps service orchestration working,
but the domain core is not yet running as its own service with typed
subscriptions, command handling, timed events, and publish fanout.

### User Story

As the maintainer, I want the domain core to run as a first-class service
runtime, so cross-system policy is handled by one clear machine rather than by
bridge code.

### Acceptance Criteria

- Domain runtime uses `WindlassMachine` through the generic service runtime.
- Domain receives normalized VPN, qBit, MAM, and DB facts as
  `Timed<WindlassEvent>`.
- Domain commands can request refresh or other operator-level actions through
  `WindlassCommand`.
- Domain actions are routed to the appropriate service command channels or DB
  command channel.
- Domain publishes `SystemState` and `Activity` through topic fanout.
- Snapshot timers round-trip as `WindlassTimer::Snapshot`.
- Tests cover that service publishes enter the domain runtime and produce the
  expected service commands.

## Story: Move DB Core Onto Service Runtime

Status: To Do

### Problem

`windlass-db-core` defines a clean `DbCommand`/`DbEvent` protocol, and
`windlass-db` has a Postgres actor that executes commands. However, DB handling
is currently called directly from bridge code instead of running like the other
external-system services.

Even though DB decisions are simpler than VPN/qBit/MAM decisions, treating DB
as a full core makes the architecture uniform. That matters for debug mode:
events, commands, actions, publishes, pauses, replay, and inspection should work
the same way for every external system.

### User Story

As the maintainer, I want DB persistence to run as a full sans-I/O core on the
same service runtime as the other external systems, so debug mode can treat DB
traffic exactly like VPN, qBit, and MAM traffic.

### Acceptance Criteria

- `windlass-db-core` defines a `DbMachine` implementing `Machine`.
- DB command handling goes through `DbMachine::handle_command`.
- `DbMachine` emits typed DB actions for the shell/Postgres adapter to execute.
- The DB shell owns `PostgresDbActor` or equivalent Postgres I/O adapter.
- DB shell sends action results back as `Timed<DbEvent>`.
- `DbMachine` publishes DB success/failure facts through topic fanout.
- Domain subscribes to DB failures and turns them into `WindlassEvent::DbFailed`
  or equivalent policy events.
- Service/domain code no longer constructs a new `PostgresDbActor` per command
  dispatch.
- Debug mode can show DB commands, DB actions, DB events, and DB publishes using
  the same machinery as the other service cores.
- Existing DB unit tests and operator integration tests continue to pass.

## Story: Replace Direct Publish Bridge With Typed Subscriptions

Status: To Do

### Problem

Service publishes are currently forwarded directly into the domain core by
`ServiceCores`. This keeps the refactor working, but it bypasses the pub/sub
model and makes the domain core a hard-coded recipient rather than a
subscriber.

### User Story

As the maintainer, I want the domain core to receive service facts through typed
subscriptions, so inter-core communication uses the same pub/sub model as
external subscribers.

### Acceptance Criteria

- Domain runtime subscribes to the VPN, qBit, MAM, and DB publish topics it
  needs.
- Service runtimes do not call domain handlers directly.
- The domain core receives normalized service facts and emits service commands
  or DB commands.
- qBit and MAM do not subscribe directly to VPN facts; cross-service policy
  remains in the domain core.
- Tests cover that `VpnPublish::PortReady` causes domain commands for qBit and
  MAM through the subscription path.

## Story: Use `Timed<Event>` End To End

Status: To Do

### Problem

The shared machine model includes `Timed<Event>`, but the current bridge mostly
uses plain legacy events and `Instant::now()`. That loses the distinction
between scheduled timer fire time and actual runtime wake-up time.

### User Story

As the maintainer, I want service events to carry logical time end to end, so
cores can reason about timer slack and event-queue lag without doing I/O.

### Acceptance Criteria

- Service event channels carry `Timed<M::Event>`.
- Timer actions compute and preserve scheduled fire time.
- Timer wakeups send the scheduled fire time, not the actual Tokio wake time.
- I/O completion events use `Instant::now()` at the point the external result
  is observed.
- Machine tests cover timer events with explicit logical timestamps.
