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
31. Mousehole-style proactive dynamic-seedbox update on IP change.
32. Parse MAM's registered IP from `/jsonLoad.php` and dedup updates against it.
33. Review and harden the integration-test suite.
34. Make dependent-container orchestration safe under Gluetun.
35. Fully port legacy `windlass-core` to the per-system cores and remove it.

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

Status: Done

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
  machine state, generate an event or command, run `handle` / `handle_command`
  once, and assert the invariants on the outcome and post-state. This covers the
  full `(state × event)` cross-product, including states a fixed event history
  would rarely reach.
- Each machine has a state `Strategy`. Inline `#[cfg(test)] mod prop_tests` is a
  child of the crate root, so it can set the machine's private fields directly
  (build via `Machine::new` then override the state fields) — no production
  from-parts constructor is needed.
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
- No production code change is needed to build arbitrary states: an inline
  `#[cfg(test)] mod prop_tests` is a descendant of the crate root and can set the
  machine's private fields directly (construct via `Machine::new`, then override
  the state fields in the generator).
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

Status: Done

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

Status: Done

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

Status: Done

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

Status: Done

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

Status: Done

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

Status: Done

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

Status: Done

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

Status: Done

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

Status: Done

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

## Story: Keep The MAM Account Alive With A Routine Status-Fetch Heartbeat

Status: To Do

### Problem

MAM Rule 1.6: accounts can be disabled for inactivity. The MAM core has no
keep-alive behavior. Today `FetchStatus` only fires on `Init`, on the failure-
retry timer, and on rate-limit expiry, so an extended healthy period with no
externally-driven refresh can let the account lapse.

The Mousehole project mirrors this with a recurring status check (default
300 s) that doubles as IP/ASN drift detection. We take the same shape: the
heartbeat is the recurring `FetchStatus`, not a separate homepage hit. This
keeps the MAM core's action surface unchanged and means every heartbeat also
refreshes ratio, upload-credit, connectability, and seedbox observation.

### User Story

As the operator user, I want Windlass to routinely fetch MAM status on a
self-perpetuating cadence so my account is never disabled for inactivity and
my MAM-derived state never goes silently stale.

### Acceptance Criteria

- The MAM core schedules a recurring `KeepAlive` timer that round-trips like
  the other MAM timers.
- The chain is started **at most once per machine lifetime**, on
  `AuthSucceeded` (mirrors qBit `TorrentRefresh`).
- On `TimerFired(KeepAlive)` the core emits exactly one `FetchStatus` action
  **and** re-schedules `KeepAlive` unconditionally — a self-perpetuating chain
  whose next tick is booked at the start of the current tick, so a dropped
  result or shell error cannot kill it.
- The keep-alive interval is configurable; default `300 s` (matches Mousehole).
- The core tracks `consecutive_status_failures: u32` and the most recent
  failure reason. The counter increments on **all three** retryable failure
  events (`AuthFailed`, `StatusFailed`, `SeedboxUpdateFailed`) and resets on
  `StatusFetched`.
- When the counter **crosses** the configured threshold (default `3`),
  the core publishes exactly one `MamPublish::KeepAliveDegraded { consecutive_failures, last_reason }`
  on the rising edge — not on every subsequent failure while still degraded.
- The domain core routes `KeepAliveDegraded` to exactly one
  `Db(RecordAlert { priority: Warning, title: "MAM heartbeat failing", … })`
  action and one `Activity` publish, with the failure reason in the body.
- Keep-alive failures emit no extra retry action of their own: the existing
  failure-retry chain (`StatusRetry`) and the next scheduled `KeepAlive` tick
  jointly handle retry; the keep-alive arm never tight-loops.
- Add invariants to `docs/invariants.md` (`MAM-8` chain-once, `MAM-9` always
  re-schedules, `MAM-10` degraded rising-edge, `DOM-14` alert routing) and
  cover them with property tests.

### Implementation Notes

- No new shell action is needed: `FetchStatus` already exists. Only the MAM
  shell's existing `MamAction::FetchStatus` handler is exercised.
- Every heartbeat already appears in the debug event/action stream (the
  `FetchStatus` action and resulting `StatusFetched` event flow through the
  existing debug machinery), so "see every heartbeat in debug mode" is free —
  we do **not** add a per-success activity-log entry.
- The failure counter intentionally counts all three retryable failures
  because the operator-visible signal is "we can't reliably reach MAM right
  now", not specifically "the status endpoint is broken".

## Story: Distinguish MAM Unreachable From NotConnectable

Status: Done

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

Status: Done

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
    librarian-readiness A2; consumed here as an external gate)*.
  - **Not a collection** — `numfiles <= 20` unless `source == ManualMamUrl`
    *(owned by librarian-readiness A2)*.
  - **Freeleech window fits** — `now + est_download_duration + safety_buffer <=
    freeleech_window_end` for freeleech candidates *(owned by
    librarian-readiness A2)*.
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

## Story: Block MAM Automation On MAM ASN-Mismatch Rejection

Status: Done

### Problem

MAM Rule 1.2 requires the seedbox to come from an ASN (effectively, a VPN
provider) the user has registered with MAM staff. The dynamic-seedbox
endpoint (`update_seedbox`) accepts arbitrary IPs within registered ASNs —
this is **intentional** and supports rotating VPN exits. The risk is *not*
that the IP changes; it is that the new IP belongs to an ASN MAM has not
authorised for this account, in which case MAM rejects the update with
"ASN mismatch".

Today that rejection arrives as `Event::MamAsnMismatch { ip }`, gets
collapsed by the MAM shell into a generic `MamEvent::SeedboxUpdateFailed`,
and the operator never learns it is a compliance event. Continuing
autograb after an ASN-mismatch can pile up unsuccessful torrent activity
from an unrecognised IP — exactly the pattern that draws staff attention.

The static-IP framing from earlier drafts of this story is wrong for the
dynamic-seedbox model. The actual signal is from MAM, not from a local
expected-vs-observed comparison.

### User Story

As the operator user, I want all autograb activity to stop immediately if
MAM tells me my current IP is on an unrecognised ASN, and I want a
`Critical` alert so I know to register the new ASN with MAM staff (or, in
the future, let Windlass register it for me automatically).

### Acceptance Criteria

- The MAM core distinguishes `MamEvent::AsnMismatch { ip }` from generic
  retryable failures. The shell maps `Event::MamAsnMismatch` to this new
  event instead of `MamEvent::SeedboxUpdateFailed`.
- The MAM core tracks ASN-compliance state with three values: `Unknown`
  (initial, before any seedbox interaction), `Accepted` (MAM successfully
  accepted our last update), and `Mismatched` (MAM rejected with ASN
  mismatch).
- On the rising edge into `Mismatched`, the MAM core publishes exactly one
  `MamPublish::AsnMismatch { ip }` on a new `MamTopic::Compliance` topic.
  Re-emitted only on a subsequent rising edge.
- On the rising edge into `Accepted` (next successful `SeedboxUpdated`),
  the MAM core publishes exactly one `MamPublish::AsnAccepted`.
- The domain core consumes both publishes to flip
  `admission.vpn_ip_compliant` between `Some(true)`, `Some(false)`, and
  `None`. The initial admission default is `None` — admission fail-closed
  until MAM confirms acceptance.
- `Mam(AsnMismatch { ip })` emits exactly one `Db(RecordAlert{Critical,
  "MAM ASN mismatch"})`, one `Db(RecordActivity)`, and one `Activity`
  publish. The alert body names the offending IP.
- The §29 admission predicate's `vpn_ip_compliant` gate now consumes a
  real signal — the §29 stub `Some(true)` default is replaced with a real
  fail-closed `None`.
- ASN-mismatch counts toward the §27 keep-alive failure counter — a
  persistent compliance problem also shows up as a degraded heartbeat
  (consistent with how other retryable failures behave).
- Add invariants to `docs/invariants.md` and cover them with property
  tests.

Core invariants (property tests):

```
# MAM-14: rising-edge AsnMismatch publish
if MamEvent::AsnMismatch arrives while asn_state != Some(Mismatched):
  publishes exactly one MamPublish::AsnMismatch { ip }
  asn_state == Some(Mismatched) afterwards
if MamEvent::AsnMismatch arrives while asn_state == Some(Mismatched):
  publishes zero MamPublish::AsnMismatch

# MAM-15: rising-edge AsnAccepted publish
if MamEvent::SeedboxUpdated arrives while asn_state != Some(Accepted):
  publishes exactly one MamPublish::AsnAccepted
  asn_state == Some(Accepted) afterwards
if MamEvent::SeedboxUpdated arrives while asn_state == Some(Accepted):
  publishes zero MamPublish::AsnAccepted

# DOM-20: AsnMismatch alert routing
Mam(AsnMismatch { ip }) emits:
  exactly one Db(RecordAlert{Critical, title: "MAM ASN mismatch"})
  exactly one Db(RecordActivity)
  exactly one WindlassPublish::Activity
```

### Implementation Notes

- Multi-ASN MAM accounts are common — the user can register several ASNs
  and rotate between them freely without triggering this gate. Automating
  ASN registration on a fresh mismatch (so Windlass adds the new ASN for
  the user) is a future story; this story only detects + alerts.
- The dynamic-seedbox `update_seedbox` call is the only MAM endpoint that
  returns an explicit ASN-mismatch response, so this is the only path
  that flips the gate to `Mismatched`. `fetch_mam_status` is read-only
  and doesn't carry the signal.
- Leak prevention ("no traffic outside the VPN") is a different concern,
  owned by §32 (Gluetun namespace ownership and dependent orchestration).
- This story does not implement automatic ASN registration. When the
  alert fires, the operator manually adds the new ASN via the MAM web UI.

## Story: Mousehole-Style Proactive Dynamic-Seedbox Update On IP Change

Status: Done

### Problem

§30 made Windlass survive a MAM ASN-mismatch rejection (block admission,
alert the operator). It is the reactive half of the compliance story. The
proactive half — *push an `update_seedbox` call whenever the public IP
changes so MAM never sees the mismatch in the first place* — is still
missing.

Today the only path that calls `MamAction::UpdateSeedbox` is
`MamCommand::EnsureSeedboxPort`, which the domain emits on **port**
change. There is no IP-change path. If Gluetun rotates exits while the
forwarded port stays the same, MAM keeps thinking we're on the old IP
until either §27's keep-alive heartbeat fails (after the threshold) or
§30 fires on the next port-driven update. Both are reactive.

The mature reference for this is [Mousehole][m] — the project the user
is migrating from. Mousehole's `getUpdateReason()` is invoked every
`CHECK_INTERVAL_SECONDS` (default 300 s). It compares the host's current
IP and ASN against the MAM `/jsonLoad.php` response and triggers
`update_seedbox` only when something changed — or when the last response
is older than `STALE_RESPONSE_SECONDS` (default 86 400 s ≈ 1 day) so the
session cookie stays fresh. That dedup is the standard the dynamic-
seedbox endpoint expects.

[m]: https://github.com/t-mart/mousehole

### User Story

As the operator user, I want Windlass to push a `update_seedbox` call as
soon as my VPN exit IP changes, and refresh the registration at least
once a day even when nothing changes, so MAM always knows where I am and
my session cookie never expires.

### Acceptance Criteria

- The VPN core observes the public IP through a new shell path (e.g. a
  periodic GET to `https://api.ipify.org` routed through Gluetun).
  Emits `VpnEvent::PublicIpChanged { ip }` only when the value
  changes — never on a re-observation of the same IP.
- The VPN core publishes `VpnPublish::PublicIpObserved { ip }` on a new
  `VpnTopic::PublicIp` topic, rising-edge on any change.
- The MAM client's `JsonLoadResponse` is extended to parse the
  registered IP from the MAM response body (the field MAM exposes in
  `/jsonLoad.php`).
- `MamStatusResult` gains a `registered_ip: Option<Ipv4Addr>` field
  carrying that value. The MAM machine stores it.
- The MAM machine compares the latest VPN-observed IP against the
  registered IP. When they differ, it emits `UpdateSeedbox`. When they
  agree, it does **not** emit `UpdateSeedbox` even if the keep-alive
  tick fires — the FetchStatus heartbeat is enough.
- A new `MamTimer::StaleRegistrationRefresh` fires once per
  `MamConfig::stale_registration_interval` (default 24 h) and emits
  `UpdateSeedbox` unconditionally, so the cookie/registration stays
  fresh even when the IP is stable.
- A successful `SeedboxUpdated` resets the stale-registration timer.
- Add invariants to `docs/invariants.md`: rising-edge
  `PublicIpChanged`, no `UpdateSeedbox` when observed == registered
  (outside the stale-refresh tick), `UpdateSeedbox` always emitted on
  observed != registered or on stale-refresh.

Core invariants (property tests):

```
# VPN-8: PublicIpChanged is rising-edge only
PublicIpObserved { ip } is emitted iff pre.observed_ip != Some(ip)
                                      and post.observed_ip == Some(ip)

# MAM-16: dedup against registered IP
if MamEvent indicates the periodic keep-alive tick fired
and machine.registered_ip == Some(observed) and !stale_due:
  no emitted action is MamAction::UpdateSeedbox

# MAM-17: stale-refresh forces an update
if MamEvent::TimerFired(StaleRegistrationRefresh):
  emitted actions contain exactly one MamAction::UpdateSeedbox
  the timer is re-scheduled for `stale_registration_interval`
```

### Implementation Notes

- The host-IP observation shell is a new piece of plumbing — it needs
  to use the VPN-routed `reqwest::Client` so the observed IP is the
  VPN exit IP, not the host's bare public IP. The Mousehole reference
  hits `api.ipify.org`; we should pick a similar low-overhead endpoint
  (consider one that also reports ASN, e.g. `https://ifconfig.co/json`,
  so a later story can add ASN-side dedup too).
- ASN-aware dedup is intentionally left for a follow-up: it requires
  also tracking ASN in `MamStatusResult` and is mostly redundant given
  §30 catches the mismatch reactively.
- The §30 retry-path tightening (skip `UpdateSeedbox` when desired ==
  current) is the first step of this dedup work — that small change
  shipped with §30.
- This story narrows the §30 reactive window: instead of the operator
  noticing an alert and registering an ASN, Windlass calls `update_seedbox`
  as soon as the IP moves, so MAM never sees a mismatch in normal
  rotations.

## Story: Parse MAM's Registered IP And Dedup Updates Against It

Status: To Do

### Problem

§31 dedups `UpdateSeedbox` calls against the *VPN-observed* IP, but it does
not yet dedup against what MAM has *registered* for our account. Mousehole's
`getUpdateReason` compares `hostInfo.ip` against
`lastMamResponse.response.body.ip` (and `body.ASN`). To match that, we need
the MAM core to know what MAM thinks our current IP is, so we can skip the
update on the cases where the file IP, the verified IP, and the MAM-recorded
IP all already agree.

The blocker is that we don't know which field in MAM's `/jsonLoad.php`
response carries the registered IP. Mousehole's source references
`response.body.ip`; we'd want to confirm by inspecting a real response with
the user before parsing.

### User Story

As the operator user, I want Windlass to skip the `UpdateSeedbox` call on
keep-alive ticks when the IP I'm coming from already matches what MAM has
recorded for me, so the dynamic-seedbox endpoint is only hit when something
actually needs updating.

### Acceptance Criteria

- Extend `JsonLoadResponse` with the MAM-registered IP field (and ASN if
  available — useful for the future ASN-aware dedup story).
- Extend `MamStatusResult` with `registered_ip: Option<Ipv4Addr>` and
  `registered_asn: Option<String>`.
- The MAM core stores `registered_ip` whenever `StatusFetched` arrives.
- `MamCommand::ObservedIpChanged { ip }` only emits `UpdateSeedbox` when
  `observed_ip != registered_ip`. When they match, the command is a no-op
  (still re-arms the stale-registration timer on first observation).
- The `StaleRegistrationRefresh` timer continues to force an update once
  per 24h regardless of dedup state.
- Add invariants to `docs/invariants.md` and cover them with property
  tests.

### Implementation Notes

- The field name needs an investigation step. Plan: run a real
  `fetch_mam_status` against the user's account, capture the JSON, identify
  the IP field together. Likely candidates based on the Mousehole codebase:
  `ip`, `IP`, `host_ip`. ASN field may be `ASN`, `asn`, `asn_org`.
- Until the field name is confirmed, the §31 implementation is correct and
  safe — it just doesn't take the full Mousehole-equivalent dedup shortcut.
- The §31 retry-path tightening already covers the
  "desired_seedbox_port == seedbox_port" case; this story adds the IP-side
  dedup on top.

## Story: Review And Harden The Integration-Test Suite

Status: To Do

### Problem

The cross-system invariants implemented through §29–§31 (admission gate,
ASN-mismatch detection, public-IP observation, leak-detection mismatch)
rely on many small per-machine property tests. The end-to-end integration
tests (`just integration`) have not been reviewed for coverage in a while
and may not exercise the new control flow: a Gluetun IP change driving a
MAM seedbox update, an ifconfig.co mismatch flipping the §29 gate, the
full TryAddTorrent → admission predicate → `Qbit(AddTorrent)` path.

Before §34 (the Gluetun orchestration story) and §35 (the legacy
`windlass-core` retirement), the integration tests should be hardened so
the cutover doesn't regress live behavior.

### User Story

As the maintainer, I want the integration-test suite reviewed and
extended so it covers the operator behaviors introduced in §27–§31, so
the §35 legacy-removal cutover can be performed with confidence.

### Acceptance Criteria

- Audit existing integration tests in `windlass/tests` (and any other
  `tests/` directories) for coverage gaps against the operator-readiness
  stories that have already shipped.
- Identify untested cross-system flows; produce a punch list. Likely
  candidates: §27 keep-alive heartbeat under simulated outages, §28
  Unreachable vs NotConnectable distinction, §29 admission-gate
  composition under realistic publish sequences, §30 ASN-mismatch
  rejection round-trip, §31 IP-change-driven UpdateSeedbox + leak-
  detection mismatch.
- Add the missing scenarios as integration tests (or document why a
  particular flow is intentionally tested only at the property-test
  layer).
- Confirm `just integration` runs clean on every covered scenario.
- Update `docs/invariants.md` with any new invariants that emerge from
  the audit.

### Implementation Notes

- Cross-reference the per-story property tests so we don't duplicate
  what's already exercised at the machine layer; the integration suite
  is about wiring + ordering across machines.
- This story should land before §34/§35 so the cutover has a safety
  net.

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

## Story: Fully Port Legacy `windlass-core` To Per-System Cores And Remove It

Status: To Do

### Problem

The operator's live decision-maker is still the legacy monolith
`windlass-core::SystemState`. The new per-system `Machine` cores
(`VpnMachine`, `QbitMachine`, `MamMachine`, `DbMachine`, `WindlassMachine`)
exist and were each moved onto the generic service runtime (stories 2-9), but
they currently run in **shadow**: the shell loop calls
`service_cores.observe(&event)` for them while it dispatches the *legacy*
`SystemState::process_event` actions as the real ones.

Concretely, all of `windlass-core/src/handlers/` (`vpn`, `qbit`, `mam`,
`monitoring`, `compliance`, `download`) still owns live behavior. The
compliance handler in particular runs the whole torrent pass in one
DB-persisting step (HnR lock, dead-torrent cleanup, no-partials, quota,
active-limit, HnR-at-risk alerts) and writes the `torrents` rows that the
`/api/v1/torrents` endpoint and the Torrent Monitor UI read. Until the live
loop runs on the new cores and the legacy crate is deleted, the operator runs
on un-migrated code and the new cores' invariants/property tests do not protect
production behavior.

### User Story

As the maintainer, I want the live operator to run entirely on the new
per-system cores with `windlass-core` removed from the workspace, so there is a
single source of truth for operator behavior and the invariant/property tests
actually guard what runs in production.

### Acceptance Criteria

- The shell event loop dispatches the actions produced by the new cores (the
  domain runtime and the per-system service runtimes), not
  `windlass-core::SystemState`.
- Every decision in the `windlass-core` handlers (`vpn`, `qbit`, `mam`,
  `monitoring`, `compliance`, `download`) has an equivalent in the appropriate
  new core, each covered by tests.
- The torrent persistence that backs `/api/v1/torrents` (and the HnR fields the
  UI shows) is produced through the new architecture (qBit core → DB core), so
  the Torrent Monitor keeps working after legacy removal.
- The `service_cores.observe(...)` shadow path is replaced by the new cores
  being the live decision-makers; no event is processed by both legacy and new
  logic at once (no double deletes, alerts, or DB writes).
- The `windlass-core` crate is removed from the workspace and no crate depends
  on it.
- Debug mode (event/action history, replay, pause, breakpoints) works against
  the new cores.
- `just check` passes, the frontend build passes, and `just integration`
  passes.

### Implementation Notes

- This is the capstone of the per-system migration: stories 2-9 moved each core
  onto the service runtime in shadow, and the per-feature stories (§§19-26,
  §§30-32) build the torrent/download/compliance/orchestration decisions in the
  new cores. This story flips the live loop onto those cores and deletes the
  legacy crate.
- Sequence the flip *after* the per-feature decision stories have reached
  behavior parity in the new cores, so the cutover does not regress live
  behavior or run both paths.
- The biggest cutover risk is the coupled compliance/torrent pass: move the
  whole torrent-details pipeline (fetch → new core → DB upsert → UI read) in one
  coordinated step rather than per-decision, to avoid legacy and new cores both
  acting on the same torrent list.
