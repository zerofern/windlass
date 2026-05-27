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
10. Add property-test scaffolding and cover already-implemented core invariants.
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
29. Enforce fail-closed download admission control (composite gate).
30. Block MAM automation on VPN IP non-compliance.
31. Make dependent-container orchestration safe under Gluetun.

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

## Story: Property-Test Scaffolding And Already-Implemented Core Invariants

Status: To Do

### Problem

The operator is a set of small `Machine` state machines connected by typed
messages. Example-based tests cover specific workflows, but they do not explore
large event sequences, repeated retries, duplicated publishes, timer races, or
unusual interleavings.

This story does two things: it establishes the reusable property-test
scaffolding each core needs, and it backfills property tests for the invariants
that are **already implemented** and catalogued in `docs/invariants.md`
(VPN-1..7, QBIT-1..7, MAM-1..6, DB-1..3, DOM-1..7). Invariants added by later
stories are tested **within those stories** using this scaffolding, not here.

### User Story

As the maintainer, I want property-test scaffolding plus coverage of the
currently-implemented core invariants, so the existing machines are protected
against regressions and future invariant stories have a harness to build on.

### Acceptance Criteria

- Add `proptest` as a dev-dependency to each core crate that gains tests
  (`windlass-vpn-core`, `windlass-qbit-core`, `windlass-mam-core`,
  `windlass-db-core`, `windlass-domain-core`).
- Per crate, add inline `#[cfg(test)]` property tests next to the existing
  example tests, with crate-local generators for that machine's `Event` and
  `Command` (primitive strategies for shared `windlass-types` are duplicated
  per crate — no shared strategy crate for now).
- Primary test style is **direct `(state, event)` generation**: generate a
  machine state (via a `#[cfg(test)]` from-parts constructor), generate an event
  or command, run `handle` / `handle_command` once, and assert the invariants on
  the outcome and post-state. This covers the full `(state × event)` cross-
  product, including states a fixed event history would rarely reach.
- Each machine exposes a test-only constructor that builds its state from parts
  (its fields are private), plus a state `Strategy`.
- Sequence-folding from `Machine::new` is kept as a **secondary** tool for the
  few genuinely history-shaped properties; it is no longer the default.
- Generic baseline per machine: no generated `(state, event)` panics.
- Backfill property tests for every invariant currently in `docs/invariants.md`,
  expressed as per-step output assertions where possible. The high-value targets
  are DOM-1 (no port-converge command without a forwarded port), QBIT-1
  (no cookie-bearing action while unauthenticated), QBIT-4 / MAM-1 (never
  advertise a port that disagrees with the desired target), and DB-3 (DB failure
  handling emits no action, so it cannot recurse).
- Tests run as part of `just check` (via `cargo test`); keep proptest case
  counts and any folded-sequence lengths modest. Add a separate recipe only if
  they become slow.

### Testing Approach

**Layer.** There is no single global `SystemState` in the new architecture, so
tests target two layers: each of the five machines individually (per-machine
invariants on its own small state), and the **domain machine** for cross-system
policy (it receives the other machines' publishes as events). No composed
multi-machine harness and no tests against the legacy `SystemState` (it is
retiring).

**Three invariant classes** (every catalogued invariant is one of these):

- *Hard safety* — must never be violated. Asserted on the outcome/post-state of a
  single generated `(state, event)` step (e.g. DOM-1, QBIT-1).
- *State-machine* — lifecycle transitions stay valid. Asserted as "this state
  field only changes via its allowed event" (e.g. `cookie` becomes `Some` only on
  `AuthSucceeded`).
- *Policy* — choose the conservative action under uncertainty / fail closed
  (e.g. no redundant write when already converged; unknown ⇒ don't act). Mostly
  domain-machine and the future admission gate.

**Total vs reachable-only — the key classification.** Direct state generation can
produce states the machine could never actually reach, so each invariant is
labelled:

- *Total* — must hold for **any** state, even an impossible one. Most safety/
  output invariants are total (they constrain what the handler emits, regardless
  of history): DOM-1, QBIT-1, QBIT-4/MAM-1, DB-3. → tested against a
  **fully-arbitrary** state generator (every field combination, including
  unreachable ones).
- *Reachable-only* — only meaningful on states the machine can be in (some
  lifecycle/transition claims). → tested against a **valid-by-construction**
  generator that emits only plausible states.

When a *total* property fails only on an unreachable state, that is precisely the
story-18 fork: either make the machine defend (keep it total) or scope the
property to valid states because the shell guarantees reachability.

**Generator tiers map onto the two generators:** Tier A (valid) and Tier B
(messy-but-possible field combos) are the valid-by-construction generator; the
fully-arbitrary generator extends into Tier C *state*. Genuinely malformed Tier C
*data* (ratio NaN, unknown API enum, forbidden LLM output) stays unrepresentable
in these `nutype`/enum-typed machines and is deferred to the parse/boundary layer
and the later stories that add rich operator state.

### Implementation Notes

- Direct `(state, event)` generation is the default because it covers the full
  cross-product; the false-positive risk is handled by the total/reachable-only
  classification above (total invariants welcome impossible states; reachable-only
  ones use the valid-by-construction generator). Folding is reserved for the few
  history-shaped properties.
- Each machine needs a `#[cfg(test)]` from-parts constructor since its fields are
  private — this is the main code change direct generation requires.
- Small enumerable input shapes are clearer as **example unit tests** than as
  properties — e.g. `VpnEvent::StateRead`'s four `connected × port` combinations
  each get an explicit test asserting the exact publishes (this is also where the
  disconnected-with-port shell-contract edge from story 18 is pinned).
- The four external-system cores ignore `now` in `handle`, so pass a fixed
  `Instant`; assert on output *shape*, not the `Utc::now()` timestamps embedded
  in snapshot/DB actions.
- Two small per-crate helpers keep property bodies readable: a one-shot
  `run(state, event)` and, for the secondary style, a `fold(events)`.
- Out of scope (deferred): property tests for the async `ServiceRuntime` (RT-*),
  the liveness invariants (qBit/MAM convergence eventually re-issued), and
  debug-mode replay/step determinism. These wait until the runtime/debug
  integration is ready; the pure machine layer is where proptest pays off first.

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

- This decision directly shapes the core property-test generators: defending
  means unconstrained generators; trusting means the generators must encode the
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
- Add the new invariant(s) to `docs/invariants.md` and cover them with a
  property test in the owning core crate (reusing the scaffolding from story 10):
  no generated sequence produces a delete action for an HnR-unsatisfied torrent.

Core invariant (property test):

```
for every known torrent t:
  if t.downloaded_bytes > 0 and t.seed_time < 72h
  then no emitted action is Action::DeleteTorrent { hash } with hash == t.hash
```

This must hold under *any* event, including: disk critically low, qBit queue
full, user-requested disk cleanup, torrent stalled, VPN broken, free space
negative, and a huge download queue.

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

Core invariant (property test):

```
if an emitted action is Action::DeleteTorrent { hash } for torrent t
then t.downloaded_bytes == 0 or t.seed_time >= required_seed_time
```

i.e. automatic deletion is allowed only for a zero-byte torrent or one that is
already HnR-satisfied; forbidden in every other case.

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

Core invariant (property test):

```
for any Action::AddTorrent { file_selection, .. } for a MAM torrent:
  file_selection == FileSelection::All   (never FileSelection::Partial)
```

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
- Deleting a torrent's media files never cascades to its history: no
  `Action::DeleteReadingLedger` or `Action::DeleteReview` is emitted alongside a
  media delete. Preferably this is structural (no such action exists).
- The decision lives in the core; the shell executes deletes.
- Add the invariant to `docs/invariants.md` and cover it with property tests:
  eviction never targets an HnR-unsatisfied torrent and never deletes more than
  needed to clear the floor.

Core invariants (property test):

```
# Disk pressure never overrides the HnR lock
if disk_free_bytes < hard_floor:
  eviction candidates exclude every torrent where
    downloaded_bytes > 0 and seed_time < 72h

# Proactive deletion-suggestion list respects deletion-value ordering
for any adjacent pair (left, right) in deletion_suggestions:
  rank_class(left) <= rank_class(right)   # lower rank == more deletable
```

Deletion-value rank classes (most → least deletable): (1) completed + low
rating (≤2★) + HnR-satisfied, (2) DNF + HnR-satisfied, (3) completed + high
rating but long since listened + HnR-satisfied, (4) unstarted + long wait + low
AI score.

### Implementation Notes

- This depends on the HnR seed-time lock story for the satisfied/unsatisfied
  classification.
- The user-directed (proactive) deletion-suggestion flow is separate operator-UI
  work; this story is only the silent emergency brake, but the ordering
  invariant above applies wherever the suggestion list is built.

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

Core invariant (property test):

```
if unsatisfied_count >= class_limit
then no emitted action is Action::AddTorrent
```

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

Core invariant (property test):

```
if candidate.freeleech == false
and (global_ratio < 2.0 or upload_buffer_gb < 25)
then no emitted action is Action::AddTorrent
```

Freeleech bypasses *only* the ratio portion — it never bypasses the HnR, disk,
qBit-privacy, port-sync, VPN-IP-compliance, or freeleech-timing gates.

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

## Story: Enforce Fail-Closed Download Admission Control

Status: To Do

### Problem

Most operator tracker-safety rules share one shape: *do not autonomously add a
torrent while some unsafe condition holds*. Today the operator cores have no
`Action::AddTorrent` at all, so there is nowhere these preconditions are
enforced. Scattering them across feature code risks one path forgetting a gate.

The safe design is a single fail-closed admission predicate: an autonomous
download is emitted **only if every gate passes**. If any gate is unknown or
false, the default is to *not* download.

### User Story

As the operator user, I want autonomous downloads to be admitted only when every
tracker-safety, network, and account condition is satisfied, so the operator
fails closed and never snatches under an unsafe condition.

### Acceptance Criteria

- The core owns one admission decision that gates every autonomous
  `Action::AddTorrent`. The decision lives in the core; the shell only executes
  an authorised add.
- The admission predicate is fail-closed: a gate whose input is unknown/stale
  counts as *not satisfied*.
- The following gates are each enforced (cross-referencing their own stories
  where they have one):
  - **Upload health / ratio** — non-freeleech requires `ratio ≥ 2.0` and
    `buffer ≥ 25 GB` (§26).
  - **Unsatisfied quota** — `unsatisfied_count < class_limit` (§25).
  - **qBit privacy clean** — `!(dht || pex || lsd)` enabled (§23).
  - **qBit port synced** — `qbit.listen_port == gluetun.forwarded_port`.
  - **MAM healthy** — `mam_health == Healthy`, where `MamHealth ∈ {Healthy,
    AuthFailed, RateLimited, Unreachable, ParseChanged, Stale}`; only `Healthy`
    permits a snatch (composes the §27/§28 health signals).
  - **VPN IP compliant** — `observed_vpn_ip == expected_vpn_ip` (§30); this gate
    beats every other signal, freeleech included.
  - **Not already-snatched** — `candidate.my_snatched == false` *(owned by
    downloader/librarian discovery work; consumed here as an external gate)*.
  - **Not a collection** — `numfiles <= 20` unless `source == ManualMamUrl`
    *(owned by downloader/librarian discovery work)*.
  - **Freeleech window fits** — `now + est_download_duration + safety_buffer <=
    freeleech_window_end` for freeleech candidates *(owned by
    downloader/librarian discovery work)*.
- When a gate blocks, the allowed outcomes are limited to non-snatch actions
  (e.g. skip, `RequestManualReview`, `RejectCandidate`, `BlockDownloads`,
  `FixQbitPrivacy`, `FireAlert`) — never an autonomous add.
- Add the composite invariant to `docs/invariants.md` and cover it with property
  tests.

Core invariant (property test):

```
# Composite, fail-closed: an autonomous add implies every gate held.
if an emitted action is Action::AddTorrent(c)
then upload_health_ok(c) and under_quota() and qbit_privacy_clean()
 and qbit_port_synced() and mam_health == Healthy and vpn_ip_compliant()
 and !c.my_snatched and (c.numfiles <= 20 or c.source == ManualMamUrl)
 and freeleech_window_fits(c)

# And the contrapositive: any single gate false => no autonomous add.
```

### Implementation Notes

- Implement each gate in its own story (§§23, 25, 26, 30, plus the pending
  discovery rules); this story is the composite predicate and the single
  enforcement point that ANDs them together.
- Manual, user-initiated downloads are out of scope for the *autonomous* gate,
  but the UI must still warn explicitly where a gate would have blocked.

## Story: Block MAM Automation On VPN IP Non-Compliance

Status: To Do

### Problem

MAM Rule 1.2 locks Gluetun to a single static server IP registered with MAM
staff. If the observed VPN IP drifts from the registered one, continuing MAM
automation risks a compliance violation. The spec says Windlass monitors the IP
and alerts, but there is no rule that *blocks* automation on a mismatch, and the
VPN core does not yet compare observed vs expected IP.

### User Story

As the operator user, I want all MAM automation to stop immediately if my VPN IP
no longer matches the IP registered with MAM, so a VPN server change can never
put my account out of compliance.

### Acceptance Criteria

- The VPN core knows the expected (registered) IP and the observed public IP.
- On mismatch, the core blocks all MAM automation — most importantly it is a
  hard gate in download admission (§29) that beats every other signal, freeleech
  included.
- A mismatch fires a `Critical` alert.
- The block clears only when the observed IP matches the expected IP again.
- Add the invariant to `docs/invariants.md` and cover it with property tests.

Core invariant (property test):

```
if observed_vpn_ip != expected_vpn_ip
then no emitted action is Action::AddTorrent   (and a Critical alert is expected)
```

## Story: Make Dependent-Container Orchestration Safe Under Gluetun

Status: To Do

### Problem

qBittorrent, MLM, and Mousehole share Gluetun's network namespace. Several
orchestration hazards are not yet modelled by the operator: a dependent
container that started before the current healthy Gluetun instance may be on a
stale namespace; dependents must not start before the VPN is healthy and
compliant; restart loops can storm; and a single incident can spew duplicate
crash dumps. These are grouped because they are one orchestration concern.

### User Story

As the operator user, I want stack orchestration to be safe under Gluetun — no
trusting stale namespaces, no premature starts, no restart storms, and no
crash-dump spam — so VPN/Docker edge cases never silently break my network
isolation.

### Acceptance Criteria

- **Gluetun root / stale namespace:** a dependent whose `started_at` predates the
  current healthy Gluetun instance is treated as untrusted and a restart of that
  dependent is eventually emitted.

  ```
  if dependent.started_at < gluetun.healthy_since
  then dependent.network_trusted == false
       and Action::RestartContainer(dependent) is eventually emitted
  ```

- **No premature start:** dependents are not started while Gluetun is unhealthy
  or the VPN IP is non-compliant.

  ```
  if !gluetun.healthy or !vpn_ip_compliant
  then no Action::StartContainer(dependent)
       (allowed: RestartGluetun, Wait, FireAlert, WriteCrashDump)
  ```

- **Restart circuit breaker:** the stack is not restarted indefinitely.

  ```
  if restarts_in_window >= max_restarts_per_window
  then no Action::RestartContainer and Action::FireAlert(Critical)
  ```

- **Crash dump once per incident:** at most one crash dump per incident.

  ```
  for one incident_id: count(Action::WriteCrashDump) <= 1
  ```

- All decisions live in the core; the shell performs Docker operations.
- Add these invariants to `docs/invariants.md` and cover them with property
  tests, including sequences that would otherwise storm restarts or duplicate
  dumps.

### Implementation Notes

- This expands the VPN/Gluetun core well beyond its current connectivity + port
  scope (it must track per-dependent start times, Gluetun `healthy_since`, a
  restart window counter, and incident identity).
- The static VPN IP compliance gate is its own story (§30); this story consumes
  the compliance signal but does not own it.
