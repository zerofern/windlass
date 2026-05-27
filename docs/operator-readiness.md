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

## Implementation Order

Implement these stories one at a time, in this order:

1. Fix initial UI state snapshot on SSE connect.
2. Introduce the generic service runtime for `Machine + Shell + TopicFanout`.
3. Use `Timed<Event>` end to end.
4. Move the DB core onto the service runtime.
5. Move the VPN core onto the service runtime.
6. Move the qBittorrent core onto the service runtime.
7. Move the MAM core onto the service runtime.
8. Move the domain core onto the service runtime.
9. Replace the direct publish bridge with typed subscriptions.
10. Add property-based tests for operator invariants.
11. Automatically donate to the pot on every pot cycle.
12. Make the dashboard and chaos page use one shared state display model.
13. Keep route state and background data fresh when tabs/pages are not active.
14. Improve debug page event/action queue visibility.
15. Allow clicking an event or action in debug view to set a breakpoint for that
    variant.
16. Clarify the manual download happy path and blocked states in the UI.
17. Make HnR/compliance risk visible enough that unsafe manual deletion is hard
    to miss.
18. Decide whether cores must defend against dishonest shell events.
19. Enforce the HnR seed-time lock on automatic torrent deletion.
20. Restrict automatic deletion and blacklisting to zero-byte dead torrents.
21. Force qBittorrent to download every file in a torrent (no partials).
22. Rank and gate disk auto-eviction by HnR-satisfied deletion value.
23. Auto-revert banned qBittorrent privacy settings (DHT, PeX, LPD).
24. Orchestrate qBittorrent queue limits to protect unsatisfied torrents.
25. Pause new automated downloads near the unsatisfied class limit.
26. Gate new downloads on upload health (ratio and credit buffer).
27. Keep the MAM account alive with a routine homepage heartbeat.
28. Distinguish MAM `Unreachable` from `NotConnectable`.

## Story: Fix Initial UI State Snapshot On SSE Connect

Status: Done

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

## Story: Introduce Generic Service Runtime

Status: Done

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

## Story: Use `Timed<Event>` End To End

Status: Done

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

## Story: Move DB Core Onto Service Runtime

Status: Done

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

## Story: Move VPN Core Onto Service Runtime

Status: Done

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

Status: Done

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

Status: Done

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

Status: Done

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

## Story: Replace Direct Publish Bridge With Typed Subscriptions

Status: Done

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

## Story: Add Property-Based Tests For Operator Invariants

Status: To Do

### Problem

The operator is becoming a set of small state machines connected by typed
messages. Example-based tests cover specific workflows, but they do not explore
large event sequences, repeated retries, duplicated publishes, timer races, or
unusual interleavings between VPN, qBit, MAM, DB, and domain events.

Property-based tests are needed to prove core invariants stay true across many
generated event sequences.

### User Story

As the maintainer, I want property-based tests for the operator cores and
runtime, so large refactors do not accidentally break safety invariants.

### Acceptance Criteria

- Add property-based tests for each external-system machine: VPN, qBit, MAM,
  DB, and domain.
- Add property-based tests for the generic service runtime once it exists.
- Generated event sequences must never panic.
- Generated event sequences must keep machines in valid states.
- qBit and MAM convergence commands are eventually re-issued after retryable
  failures while desired state is still known.
- Domain policy never commands qBit or MAM to converge on a port when VPN has
  no forwarded port.
- DB failure handling does not recurse indefinitely.
- Debug-mode replay/step behavior preserves machine determinism once debug mode
  is integrated into the runtime.
- Property tests run as part of `just check` unless they become too slow; if so,
  add a separate explicit recipe and document when to run it.

### Implementation Notes

- Use focused state/event generators instead of arbitrary JSON payloads.
- Keep properties tied to operator safety: no unsafe deletes, no port sync
  without VPN port, no retry storms, no invalid service state transitions.
- Prefer shrinking-friendly event enums and small sequence lengths at first.

## Story: Automatically Donate To The Pot On Every Pot Cycle

Status: To Do

### Problem

The operator should handle recurring MAM housekeeping tasks that are easy to
forget. One desired behavior is automatic donation to the pot on every pot
cycle.

Assumption: "pot" refers to the MAM millionaire's/vault pot. The exact endpoint,
cycle detection rule, and donation amount still need to be confirmed before
implementation.

### User Story

As the operator user, I want Windlass to automatically donate to the pot every
pot cycle, so I do not need to remember this recurring tracker task manually.

### Acceptance Criteria

- Add MAM-core state for pot-cycle observation and donation eligibility.
- Add configuration for enabling/disabling automatic pot donation.
- Add configuration for the donation amount or policy.
- Windlass detects a new pot cycle without donating more than once per cycle.
- Donation attempts go through the MAM service core and shell, not ad hoc web
  handler code.
- Donation success and failure are recorded in activity log.
- Donation failures retry conservatively and never create a tight loop.
- The UI exposes current pot donation status and last donation result.
- Integration tests use a mocked MAM pot endpoint/scenario.
- Property-based tests cover the no-double-donation-per-cycle invariant.

### Implementation Notes

- Treat this as MAM operator automation, not recommendation/librarian work.
- The MAM core should own the decision: whether a cycle is new, whether
  donation is enabled, whether donation was already attempted, and whether a
  retry is allowed.
- The MAM shell should own only HTTP details and return typed events.

## Story: Decide Whether Cores Must Defend Against Dishonest Shell Events

Status: To Do

### Problem

While cataloging operator invariants (see `docs/invariants.md`), two cases
surfaced where a core's published facts are only correct because the shell is
trusted to send well-formed events. Today these are documented as shell
contracts, not enforced by the machine:

1. VPN `StateRead { connected: false, port: Some(_) }` would make `VpnMachine`
   publish `Disconnected` and `PortReady` together, advertising a forwarded port
   for a VPN it just reported as down. The machine assumes the shell never
   reports a disconnected VPN that still has a port.

2. qBit's `ListenPortSet { port }` arm publishes `ListenPortReady { port }`
   directly, bypassing the desired-port filter that the other qBit port-publish
   paths use (invariant QBIT-4). It is only consistent because the shell only
   emits `ListenPortSet` as the success result of a `SetListenPort` action the
   machine itself issued for the desired port.

Neither is a known live bug, because the real shells uphold the contracts. The
question is whether the cores should defend against these inputs anyway, so the
invariants hold for *any* event sequence rather than only well-formed ones.

### User Story

As the maintainer, I want a deliberate decision on whether cores must stay
correct under arbitrary (including dishonest) shell events, so the property
tests either constrain their generators to the shell contract or prove the
machines are robust without that assumption.

### Acceptance Criteria

- Decide, per case, between "defend in the machine" and "trust the shell
  contract".
- If defending: VPN drops/ignores a port when reporting disconnected, and qBit
  routes `ListenPortSet` through the desired-port filter (or an equivalent fix),
  with unit tests for the dishonest input.
- If trusting: the shell contract is documented at the shell boundary (not only
  in `docs/invariants.md`), and the property-test generators are constrained to
  exclude the disallowed event combinations.
- `docs/invariants.md` is updated to reflect the decision (shell contract vs.
  enforced invariant).

### Implementation Notes

- This decision directly shapes story 10's generators: defending means
  unconstrained generators; trusting means the generators must encode the
  contract.
- Prefer the cheaper option unless defending removes a real class of operator
  risk.

## Story: Enforce The HnR Seed-Time Lock On Automatic Torrent Deletion

Status: To Do

### Problem

MAM Rules 2.5 & 2.7 prohibit hit-and-run: a torrent that has downloaded any
data must keep seeding until it reaches 72 hours of seed time. The operator
cores do not yet track per-torrent seed time, and there is no deletion decision
that is gated on it. Any future automatic deletion path could evict a torrent
mid-HnR and risk an account ban.

This is the highest-stakes operator safety rule. See `docs/invariants.md`.

### User Story

As the operator user, I want automatic torrent deletion to be mathematically
incapable of evicting a torrent that has downloaded data before it reaches 72
hours of seed time, so the operator can never cause an HnR violation on my
behalf.

### Acceptance Criteria

- The qBit core tracks, per known torrent, at least: downloaded bytes and seed
  time (sourced from qBittorrent torrent listings).
- A torrent is classified `HnR-satisfied` only when `seed_time >= 72h` or
  `downloaded_bytes == 0`.
- No automatic-deletion action is ever emitted for an HnR-unsatisfied torrent,
  for any state or event sequence.
- The deletion decision lives in the core, not the shell; the shell only
  executes the typed delete action the core authorises.
- Add the new invariant(s) to `docs/invariants.md` and cover them with
  property-based tests (story 10): no generated sequence produces a delete
  action for an HnR-unsatisfied torrent.

### Implementation Notes

- The 72-hour threshold should be configurable but default to the MAM rule.
- This story only establishes the lock. Choosing *which* satisfied torrents to
  evict is the disk-eviction story.

## Story: Restrict Automatic Deletion And Blacklisting To Zero-Byte Dead Torrents

Status: To Do

### Problem

MAM rules allow cleaning up stalled or dead torrents, but only when nothing has
been downloaded. The operator has no concept of "dead torrent" cleanup yet, and
without an explicit rule a cleanup path could delete and blacklist a torrent
that already pulled data — both wasting the download and risking HnR.

### User Story

As the operator user, I want stalled or dead torrents to be automatically
deleted and blacklisted only when they have downloaded exactly zero bytes, so
cleanup never throws away real progress or triggers HnR.

### Acceptance Criteria

- The core can identify a stalled/dead torrent from qBittorrent state.
- An automatic delete-and-blacklist action is emitted only when
  `downloaded_bytes == 0`.
- A dead torrent with any downloaded data is never auto-deleted by this path; it
  falls under the HnR seed-time lock instead.
- The decision lives in the core; the shell executes the typed action.
- Add the invariant to `docs/invariants.md` and cover it with property tests.

## Story: Force qBittorrent To Download Every File In A Torrent

Status: To Do

### Problem

MAM Rule 2.5 (No Partials) prohibits stopping a download partway and keeping
only some files — every file in a torrent must be downloaded in full. The
operator does not currently enforce per-file selection, so a torrent added with
some files deselected (or qBittorrent defaulting to partial selection) would
violate the rule.

### User Story

As the operator user, I want Windlass to ensure every file in a torrent is set
to download, so I never accidentally keep a partial torrent and breach MAM
rules.

### Acceptance Criteria

- When a torrent is added or observed with deselected files, the qBit core emits
  an action to set all files to download.
- The core treats "all files selected" as the only compliant state and converges
  toward it, retrying on failure like other qBit convergence loops.
- A torrent is never published as healthy/ready while it has deselected files.
- The decision lives in the core; the shell performs the qBittorrent file-
  priority call.
- Add the invariant to `docs/invariants.md` and cover it with property tests.

## Story: Rank And Gate Disk Auto-Eviction By HnR-Satisfied Deletion Value

Status: To Do

### Problem

The operator must keep the mounted volume from filling up. The spec defines an
emergency auto-evict that, below a hard floor, silently removes the lowest-value
torrents — but only HnR-satisfied ones, never HnR-unsatisfied. No disk-eviction
decision exists yet, and the disk core only observes free space.

### User Story

As the operator user, I want the operator to free disk space automatically when
it drops below a hard floor by evicting only the lowest-value, HnR-satisfied
torrents, so my disk never fills up and the operator never deletes something it
must keep seeding.

### Acceptance Criteria

- The core observes free disk space and a configurable hard floor.
- When free space is below the floor, the core emits delete actions only for
  HnR-satisfied torrents (per the HnR seed-time lock).
- Eviction candidates are ranked by deletion value (e.g. completed + low rating
  + longest time since last play first); HnR-unsatisfied torrents are never
  eligible.
- Eviction stops once free space is back above the floor (no over-deletion).
- The decision lives in the core; the shell executes deletes.
- Add the invariant to `docs/invariants.md` and cover it with property tests:
  eviction never targets an HnR-unsatisfied torrent and never deletes more than
  needed to clear the floor.

### Implementation Notes

- This depends on the HnR seed-time lock story for the satisfied/unsatisfied
  classification.
- The user-directed (proactive) deletion-suggestion flow is separate operator-UI
  work; this story is only the silent emergency brake.

## Story: Auto-Revert Banned qBittorrent Privacy Settings

Status: To Do

### Problem

MAM Rule 6.1 forbids DHT, PeX, and Local Peer Discovery on private trackers;
these carry an immediate ban risk. Windlass actively manages qBittorrent's
config, but the operator does not yet detect or correct these settings.

### User Story

As the operator user, I want Windlass to immediately revert DHT, PeX, or Local
Peer Discovery whenever it finds them enabled, so my account is never exposed to
a ban from a stray client setting.

### Acceptance Criteria

- The qBit core observes the DHT/PeX/LPD preference values.
- If any is enabled, the core emits an action to disable it immediately, with no
  wait for user confirmation.
- The intervention is recorded in the activity log and fires a `Critical`
  alert.
- The core converges back to the safe state and retries on failure.
- The decision lives in the core; the shell performs the preference write.
- Add the invariant to `docs/invariants.md` and cover it with property tests:
  observing any privacy setting enabled always yields a disable action.

## Story: Orchestrate qBittorrent Queue Limits To Protect Unsatisfied Torrents

Status: To Do

### Problem

qBittorrent's `max_active_downloads`, `max_active_uploads`, and
`max_active_torrents` limits can park torrents when reached. If an
HnR-unsatisfied torrent gets parked (stops seeding), it risks an HnR violation.
The operator does not yet orchestrate the queue to keep unsatisfied torrents
active.

### User Story

As the operator user, I want Windlass to keep my HnR-unsatisfied torrents
actively seeding by temporarily pausing fully satisfied torrents when qBittorrent
limits would otherwise park them, so queue limits never cause an HnR violation.

### Acceptance Criteria

- The qBit core knows each torrent's HnR-satisfied status and qBittorrent's
  active limits.
- When limits would park an unsatisfied torrent, the core emits actions to pause
  satisfied torrents to make room, preferring config-free orchestration.
- The core never pauses an HnR-unsatisfied torrent to make room for another.
- If orchestration alone cannot prevent a violation, the core escalates per the
  next story (queue-limit config auto-correction) rather than allowing the
  parking.
- The decision lives in the core; the shell performs pause/resume.
- Add the invariant to `docs/invariants.md` and cover it with property tests.

### Implementation Notes

- The config-escalation path (auto-raising a limit and firing a `Critical`
  `Action` notification) can be folded in here or tracked as its own follow-up;
  keep orchestration-first as the rule.

## Story: Pause New Automated Downloads Near The Unsatisfied Class Limit

Status: To Do

### Problem

MAM Rule 2.8 caps the number of unsatisfied torrents by user class (e.g. 50 for
User, 100 for Power User). Exceeding it risks penalties. The operator does not
yet track the unsatisfied count or gate new automated downloads on it.

### User Story

As the operator user, I want Windlass to stop starting new automated downloads
as my unsatisfied-torrent count approaches the class limit, so I never blow past
my quota.

### Acceptance Criteria

- The core tracks the current count of unsatisfied torrents and the configured
  class limit.
- When the unsatisfied count is at or near the limit, the core suppresses new
  automated download actions.
- The gate is released once the unsatisfied count falls back under the
  threshold.
- Manual/user-initiated downloads are out of scope for this automatic gate
  (operator-UI safety is handled elsewhere).
- The decision lives in the core; add the invariant to `docs/invariants.md` and
  cover it with property tests: no new automated download is started while the
  unsatisfied count is at or above the limit.

## Story: Gate New Downloads On Upload Health

Status: To Do

### Problem

MAM Rule 1.4 (upload health) requires staying well clear of the ratio minimum.
The spec sets the operator gate at global ratio ≥ 2.0 and an upload-credit
buffer ≥ 25 GB before queueing new downloads. The operator does not yet observe
ratio/credit or gate downloads on them.

### User Story

As the operator user, I want Windlass to refuse to start new (non-freeleech)
downloads when my global ratio or upload-credit buffer is too low, so the
operator never erodes my account health.

### Acceptance Criteria

- The core observes global ratio and upload-credit buffer from MAM.
- New automated downloads are gated when `ratio < 2.0` or `buffer < 25 GB`
  (thresholds configurable).
- Freeleech grabs are exempt from the ratio portion of the gate (they do not
  spend ratio), consistent with §7.4.
- The gate is released when both metrics recover.
- The decision lives in the core; add the invariant to `docs/invariants.md` and
  cover it with property tests.

## Story: Keep The MAM Account Alive With A Routine Homepage Heartbeat

Status: To Do

### Problem

MAM Rule 1.6: accounts can be disabled for inactivity. The MAM core has no
keep-alive behavior, so a quiet period with no status/seedbox traffic could let
the account lapse.

### User Story

As the operator user, I want Windlass to routinely touch the MAM homepage so my
account is never disabled for inactivity.

### Acceptance Criteria

- The MAM core schedules a recurring keep-alive timer that round-trips like the
  other MAM timers.
- On fire, the core emits an action for the shell to hit the MAM homepage and
  re-schedules the timer (a self-perpetuating chain, like qBit `TorrentRefresh`).
- The keep-alive interval is configurable.
- Keep-alive failures are handled conservatively and never create a tight loop.
- Add the invariant to `docs/invariants.md` (keep-alive chain never dies) and
  cover it with property tests.

## Story: Distinguish MAM Unreachable From NotConnectable

Status: To Do

### Problem

The MAM connectability heartbeat should distinguish a network failure
(`Unreachable` — the tracker could not be reached at all) from a genuine
connectivity problem (`NotConnectable` — the tracker reached qBit and reports it
is not connectable). The MAM core currently collapses these: `StatusFailed` is a
generic failure and `StatusFetched { connectable: false }` always publishes
`NotConnectable`, so operators cannot tell a transient network blip from a real
seedbox/port problem.

### User Story

As the operator user, I want the operator to tell me whether MAM is genuinely
reporting my client as not connectable versus simply being unreachable right
now, so I can distinguish a real port/seedbox problem from a transient network
issue.

### Acceptance Criteria

- The MAM core models `Unreachable` and `NotConnectable` as distinct outcomes.
- A failure to reach MAM at all surfaces as `Unreachable`; a successful status
  read that reports the client not connectable surfaces as `NotConnectable`.
- The two map to distinct publishes and distinct activity/alert messaging.
- `Unreachable` is treated as a transient/retryable condition; `NotConnectable`
  is surfaced as a genuine connectivity problem.
- Update `docs/invariants.md` for the refined MAM connectability model and cover
  the distinction with tests.
