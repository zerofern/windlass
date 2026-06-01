# Observability Redesign — §37 Discussion Doc

This is the discussion artifact for operator-readiness §37 (originally
"Review and Consolidate Debug-Mode Workflow"). It audits the
debug-mode surface as it exists after the §36 per-system core
migration, names what is stale or actively misleading now, and
sketches the shape of a redesign at the principle level.

The framing is shifting from **debug mode** (a modal toggle wrapped
around a single event loop) to **observability** (an always-on view
of the running system, with optional per-core pause for stepping).
"Debug" disappears as a name throughout. The canonical
operator-facing spec, `docs/debug-mode.md`, still reads as
authoritative but describes a system that no longer exists; it will
be retired and replaced by `docs/observability.md` as part of this
work.

---

## Anchoring decisions (agreed)

1. **Naming: observability, not debug.** Crate becomes
   `windlass-observability`. Route becomes `/observability`. Two
   traits: `RuntimeTap` (runtime-side) and `HttpTap` (client-side).
   Controller is `ObservabilityController`. The framing is "this is
   how you watch the running system; pausing is one thing you can do
   from this view."
2. **Audience: one combined surface.** No operator-vs-maintainer
   view toggle. One page, one layout, used the same way by everyone.
3. **Pause model: per-core is the primitive; global is "do it to
   every core."** The operator can stop one or more cores
   independently, and step a specific core to decide cross-core
   execution order. "Pause All / Step All" are convenience wrappers.
4. **Gate granularity: three points, all per-core.**
   1. **Event gate** — pause at the top of a core's runtime loop,
      before `machine.handle` is called.
   2. **HTTP-request gate** — pause *before* an HTTP request is
      sent, with the full request body visible.
   3. **Per-action gate** between dispatches — *deferred for v1.*
5. **Observation is always on, for everything.** Events, actions,
   publishes, per-core state snapshots, HTTP exchanges, and log
   lines flow into bounded rings unconditionally. There is no
   global on/off flag in the hot path.
6. **Timeline primitive: the per-core `StepRecord`.** One event +
   the actions it produced + the publishes it produced + the state
   snapshot at the end + the duration + threaded `action_id`s and
   `publish_id`s. The operator collapses each record to a one-liner
   or expands to see everything that one event caused.
7. **Full causal graph by ID, bidirectional.** Every action gets an
   `action_id`, every publish gets a `publish_id`, every event
   carries `cause: EventCause` (= `Action(uuid)`, `Publish(uuid)`,
   or `External`). Subscriber bridges that translate publish →
   event in another core set the cause. Backward links (action →
   its parent event, publish → its parent event) are O(1) via
   indices the controller maintains alongside the rings. Forward
   links (action → resulting events, publish → resulting events)
   are filters by cause. The whole graph is navigable from any
   node by click.
8. **State delta is a UI concern.** The runtime ships full
   `state_after` in every StepRecord; the React side diffs against
   the previous record to render the Δ summary and the expanded
   side-by-side. No backend diff logic, no per-machine diff schema.
9. **`Machine` gains a state accessor.** The trait extends with
   `type State: Serialize + Clone` and `fn state(&self) -> &Self::State`.
   No debug-awareness in `Machine` — the accessor is general purpose.
10. **`Shell` trait is untouched.** HTTP capture lives at client
    construction via the `HttpTap`.
11. **`PAUSE_ON_START` is kept** as a boot-time entry point — every
    runtime is constructed with its per-core pause flag pre-set.
    (Renamed from `DEBUG_MODE_ON_START` to match the new framing.)
12. **Scope cut for v1: queue manipulation (edit / inject / reorder
    / delete) is dropped.** Reduces surface area and removes a
    class of "the observer broke the system" risk.
13. **The frontend page is fully redesigned.** Single page, no
    view toggle, no merged cross-core stream. The current `/debug`
    page is not iterated — it is replaced.
14. **Secrets policy.** Adopt the
    [`secrecy`](https://docs.rs/secrecy) crate for any field that
    holds a secret in code (MAM cookie, qBit password, etc.) so
    they cannot be accidentally `Debug`-logged. On the
    observability page itself, secrets are **shown verbatim** —
    headers, cookies, request bodies, state values. Future story
    adds auth on the page itself; until then the page is single-user.
15. **Separation from normal logic is a hard goal.** A bug in
    `windlass-observability` cannot corrupt machine state, drop or
    reorder events, or change action dispatch — its blast radius is
    bounded to "hangs a runtime" or "loses timeline visibility."
16. **Process: discuss-first.** This document is the artifact;
    implementation stories fall out only after the target shape is
    agreed.

---

## Original purpose (still valid)

The goals from the old `debug-mode.md` are still right: step through
edge cases in development without the system racing ahead; operate
with confidence in production over rate-sensitive external services
(especially MAM); transparent — never modify events, actions, state,
or HTTP requests, only control *when* each step proceeds; three entry
points (env var, web UI toggle, MAM rate-limit guardrail).

The redesign preserves all four. What changes is the *shape* of the
underlying loop being watched and the *surface* the operator sees.

---

## What changed in §36

Before §36, Windlass had one event loop and one `SystemState`. Debug
mode wrapped that loop: every `Event` paused at the loop's intake,
every `Action` paused at the loop's dispatcher, the trace recorded
before/after `SystemState`, and "the current pause point" was always
one of two things.

After §36 (closed 2026-06-01), the live decision-making runs on six
per-system cores — VPN, qBit, MAM, DB, disk, Docker — each on its
own generic `ServiceRuntime`. Each runtime has its own typed `Event`
/ `Command` channels, its own machine `state` (sans-I/O), and its
own shell that dispatches typed actions and emits typed publishes
via `TopicFanout`.

The legacy `windlass-core::SystemState` still exists as the *bridge
protocol* between the few remaining I/O sites and the service-events
bridge, but its `process_event` is gone and the dashboard view of
it is frozen at `initial()`. The central shell loop is now:

```
recv legacy Event → debug!(?event) → service_cores.observe(&event)
```

Everything interesting happens inside the per-system runtimes that
`observe` fans out to. There is no central action batch and no
central state.

---

## Audit: the surface today

### Backend — `windlass-debug` (~2050 LOC)

| Module                  | Status post-§36                                                                                |
| ----------------------- | ---------------------------------------------------------------------------------------------- |
| `DebugController`       | Mostly alive. `enable/disable`, `step`, `skip`, `paused_on`, breakpoints, snapshot — all work. |
| `DebuggableEventStream` | Alive. Pauses at the central legacy-event intake.                                              |
| `DebugHistory`          | Half-alive. Event queue + log capture work; per-event before/after `SystemState` is meaningless (legacy frozen at `initial()`). |
| `DebugDispatcher`       | **Dead.** Was the central action dispatcher; the central loop no longer dispatches actions.    |
| `CausalTx`              | Alive. Task-local action id for HTTP-exchange threading. Works inside any runtime that uses it. |
| `make_http_observer`    | Alive. Per-action HTTP exchange callback. No-op when debug is off; routes to `exchange_rx` when on. |
| `DebugLogLayer`         | Alive. Tracing layer captures log lines for the UI panel.                                      |
| `DebugState`            | Half-alive. The shape still serializes, but `latest_state`, `trace[].state_before/after`, and `running_actions` reflect a model that no longer matches reality. |

### Backend — main shell loop (`windlass/src/shell/mod.rs`)

Gutted as part of §36 step 8. No central action dispatch. The only
event-side hook left is the intake pause inside `dequeue_debug` /
`DebuggableEventStream`. Once an event is forwarded to
`service_cores.observe(&event)`, the central loop has no visibility
into what happens next.

### Backend — per-core runtimes

The per-core `ServiceRuntime`s currently have **no observability
integration at all**. They process events at full speed regardless
of any flag. This is the single biggest gap.

### Frontend — `/debug` route (`app/src/routes/Debug.tsx`, 769 LOC)

To be replaced wholesale. About 40% of what it shows is now lying
to the operator (state diff against frozen legacy `SystemState`,
action timeline for actions that never run through it, action
breakpoints wired to dead code, dryrun against legacy state, queue
manipulation whose mental model is broken because actions happen
inside per-core runtimes).

---

## Architecture: where the observability surface plugs in

Two traits, one new field on `ServiceRuntime<M, S>`, one new accessor
on `Machine`, one extension to `Timed<E>` for causal threading.
`Shell` trait is untouched. There is no `Runtime` trait — there is
one generic struct, instantiated six times.

### `Machine` trait extension

```rust
pub trait Machine: Sized {
    // ... existing items ...
    type State: Serialize + Clone;
    fn state(&self) -> &Self::State;
}
```

Every machine refactors its existing state fields into a `State`
struct it owns and lends. Property tests stay unchanged — they
construct states directly.

### `Timed<E>` causal extension

```rust
pub enum EventCause {
    Action(Uuid),    // event is the result of an action (e.g. HTTP completion, shell I/O)
    Publish(Uuid),   // event is the result of a subscribed publish from another core
    External,        // timer fire, file watcher, Docker event, manual command, init
}

pub struct Timed<E> {
    pub at: Instant,
    pub cause: EventCause,
    pub inner: E,
}
```

Constructors at call sites:

- `Timed::from_action(now, action_id, event)` — shell-side I/O completion.
- `Timed::from_publish(now, publish_id, event)` — subscriber bridge.
- `Timed::external(now, event)` — timer, watcher, command.

This is the smallest change that lets the UI walk the full causal
graph in either direction.

### `RuntimeTap` (runtime-side)

```rust
#[async_trait]
pub trait RuntimeTap: Send + Sync {
    /// Park here until the controller releases us. Called at the top
    /// of the runtime loop, before machine.handle. Returns immediately
    /// when this core's pause flag is not set.
    async fn gate_event(&self, core: CoreId, event_variant: &str);

    /// Fire-and-forget: emit one StepRecord into the per-core ring.
    /// Always called, always populates the ring.
    fn observed_step(&self, core: CoreId, step: StepRecord<'_>);
}

pub struct StepRecord<'a> {
    pub step_id: Uuid,
    pub at: Instant,                                  // event.at (logical time)
    pub duration: Duration,                           // handle() wall time
    pub kind: StepKind,                               // Event | Command
    pub event_variant: &'a str,
    pub event: &'a dyn erased_serde::Serialize,       // the Timed<E> payload
    pub event_cause: EventCause,                      // copied from Timed<E>
    pub state_after: &'a dyn erased_serde::Serialize, // M::State snapshot, full
    pub actions: &'a [(Uuid, &'a dyn erased_serde::Serialize)],   // action_id + payload
    pub publishes: &'a [(Uuid, &'a dyn erased_serde::Serialize)], // publish_id + payload
}
```

`Action` and `Publish` IDs are now first-class peers — each gets a
Uuid at emission time so any subscriber's resulting event can point
back at one of them via `cause`.

One field added to `ServiceRuntime<M, S>`:

```rust
tap: Arc<dyn RuntimeTap>,  // NullRuntimeTap by default — both methods are no-ops
```

Runtime loop diff (four lines, all clearly a side concern):

```rust
let event = event_rx.recv().await?;
self.tap.gate_event(self.core_id, event.variant_name()).await;
let t0 = Instant::now();
let outcome = self.machine.handle(t0, event);
let duration = t0.elapsed();
self.tap.observed_step(self.core_id, StepRecord {
    step_id: Uuid::new_v4(),
    at: event.at, duration, kind: StepKind::Event,
    event_variant: event.variant_name(), event: &event,
    event_cause: event.cause,
    state_after: self.machine.state(),
    actions: &outcome.actions_with_ids,
    publishes: &outcome.publishes_with_ids,
});
self.apply(outcome.actions, outcome.publishes);
```

`apply` is unchanged: it dispatches actions through the shell and
fans publishes out via `TopicFanout`. The new IDs flow through the
publish fanout so each subscriber bridge can quote them when it
forwards a resulting event.

### `HttpTap` (client-side)

Replaces today's `HttpObserver = Arc<dyn Fn(HttpExchange)>`:

```rust
#[async_trait]
pub trait HttpTap: Send + Sync {
    /// Park here until the controller releases us. Called between
    /// building the request and calling .execute(). The full request
    /// (method, URL, headers, body) is visible while parked.
    async fn gate_request(&self, core: CoreId, req: &HttpRequestView<'_>);

    /// Fire-and-forget: push the completed exchange to the always-on
    /// HTTP ring. The action_id (read from the task-local CausalTx)
    /// threads it back to the StepRecord whose action emitted it.
    fn observed_exchange(&self, core: CoreId, ex: &HttpExchange);
}
```

Each client takes one `Arc<dyn HttpTap>` at construction. Inside a
client:

```rust
let req = self.client.post(&url).json(&body).build()?;
self.hook.gate_request(self.core, &HttpRequestView::from(&req)).await;
let res = self.client.execute(req).await?;
self.hook.observed_exchange(self.core, &HttpExchange { /* … */ });
```

### Shared controller (`windlass-observability::ObservabilityController`)

Per-core `paused: AtomicBool` and `step_permits: Semaphore`. Six
per-core StepRecord rings (~500 deep, always-on). One cross-core
HTTP exchange ring (~500 deep, always-on). One cross-core log ring
(already exists, kept). SSE broadcast for the page.

Plus two indices for O(1) backward causal lookups, maintained as
records enter and leave the rings:

- `action_id → (core, step_id)` — find an action's parent event.
- `publish_id → (core, step_id)` — find a publish's parent event.

The React side builds the same indices from the SSE stream, so the
wire format does not need to repeat the back-references on every
HTTP exchange.

### Secrets

Adopt [`secrecy`](https://docs.rs/secrecy):

- Wrap MAM session cookie, qBit password, and any future credentials
  in `SecretString` in their config types.
- Default `Debug`/`Display`/`Serialize` impls keep them out of
  tracing logs and stray serializations.
- The observability path is the *only* path that exposes them: a
  small `expose_for_observability(&Secret<T>)` helper used by the
  state-snapshot serializer and the HTTP-header capture. The page
  shows them verbatim. (Header capture already does this implicitly
  — the cookie is in the headers we record.)

When auth is added to the `/observability` route in a future story,
no other change is needed.

### What this trait split buys us

- **`Machine::handle` is untouched** — same pure function. Only
  `type State` and `fn state(&self) -> &Self::State` added.
- **`Shell` is untouched.**
- **The runtime gains four lines, all using one trait object.**
  Reader of the runtime sees normal-path code with observability as
  a clearly separated side concern.
- **Clients gain two lines, behind a trait object they don't have
  to understand.**
- **A bug in `windlass-observability` cannot corrupt machine state,
  misroute an event, drop an action, or reorder a dispatch.** The
  trait objects do not run inside the machine, the shell's
  dispatch, or action application.

---

## Redesign principles

### P1 — Per-core gate is the primitive; global is convenience

Three gate points, each per-core, each independently toggleable:

| Gate          | Lives in              | Fires before                              |
| ------------- | --------------------- | ----------------------------------------- |
| Event gate    | `ServiceRuntime` loop | `machine.handle(event)` is called         |
| HTTP gate     | each HTTP client      | `execute()` sends the request             |
| Action gate   | *(deferred for v1)*   | *(between dispatches inside an outcome)*  |

Global Pause = set every core's event-gate flag. Global Step = add
a permit to every paused core's semaphore. Per-core Pause / Step
are the same operation scoped to one `CoreId`.

### P2 — Observation is always on; gating is sometimes on

Five always-populated streams: per-core StepRecord rings, the
cross-core HTTP ring, the cross-core log ring. No global on/off
flag. `observed_*` methods always run. `gate_*` methods always
run too but return immediately when the relevant pause flag isn't
set.

Practical consequence: visit `/observability` and you immediately
see the recent event flow and last N HTTP exchanges, no toggle.
Pausing is a separate action that turns the page into a debugger.

Bounded ring sizes (initial): 500 StepRecords per-core, 500 HTTP
exchanges. Memory budget: ~10 MB peak. Tunable later.

### P3 — Each per-core StepRecord binds event + actions + publishes + state

One event in, all side effects out, state after, threaded
`action_id`s and `publish_id`s. The operator collapses each record
to a one-liner ("`StatusFetched` → 0 actions, 1 publish, no state
change") or expands to see the full event payload, each action with
its nested HTTP exchanges, each publish, the state Δ summary
against the previous record, and the duration.

The cross-core HTTP ring stays usable on its own; rendering a step
record looks up matching exchanges by `action_id` and nests them.

This is the headline UX change.

### P4 — Causal threading: bidirectional, by ID

Every action has an `action_id`. Every publish has a `publish_id`.
Every event carries `cause: EventCause` = `Action(uuid)` /
`Publish(uuid)` / `External`. The subscriber bridges that translate
publish → event in another core set the cause when they forward.
The controller maintains `action_id → (core, step_id)` and
`publish_id → (core, step_id)` indices for O(1) back-references.

Explicit links in every direction:

| From            | To                       | How                                          |
| --------------- | ------------------------ | -------------------------------------------- |
| event           | its actions / publishes  | containment inside the StepRecord            |
| event           | its cause                | `EventCause` field                           |
| action          | its parent event         | `action_id → step_id` index                  |
| action          | resulting events         | filter events with `cause = Action(this_id)` |
| publish         | its parent event         | `publish_id → step_id` index                 |
| publish         | resulting events         | filter events with `cause = Publish(this_id)`|
| HTTP exchange   | its action               | exchange carries `action_id`                 |
| HTTP exchange   | parent event (two hops)  | `action_id → step_id` index                  |

UI affordances this unlocks:

- Click an **action** → jump to its parent event row, *and*
  highlight its HTTP exchanges in the cross-core ring, *and*
  highlight every event whose cause is this action.
- Click a **publish** → jump to its parent event row, *and*
  highlight every downstream event in every core whose cause is
  this `publish_id`. "Jump to resulting events" is one click.
- Click an **event** → jump to its cause (action's row in the
  producing core, publish's row in another core, or "external"
  with the reason). The actions and publishes the event produced
  are already visible inline in the expanded row.
- Click an **HTTP exchange** → jump straight to its action's
  parent event row in the originating core.

The full operator questions — "what did this publish actually
do?", "what caused this HTTP request?", "what event produced this
action?" — all become one click.

### P5 — Per-core machine state, full snapshot in every StepRecord

`M::State: Serialize + Clone`. The runtime calls `self.machine.state()`
after each handle, the StepRecord contains the full serialized
state. State Δ is computed in the React UI against the previous
record's `state_after`.

Δ rendering:

- **Collapsed row:** one-line summary listing changed leaf paths,
  before → after (e.g. `connectable: false → true,
  last_status_at +1.0s`). "No change" when nothing changed.
- **Expanded row:** side-by-side full state JSON with changed
  lines highlighted. Library: something like `microdiff` (~1 KB).

The runtime does no diff work. The wire format ships full state per
record. At 500 records × ~1 KB × 6 cores ≈ 3 MB peak ring memory;
SSE only sends the new record per step.

### P6 — Breakpoints are per-variant; presented as one flat list

The operator does not need to know which core owns
`MamAction::UpdateSeedbox`. The UI shows one flat searchable list
of event variants + action variants + publish variants + HTTP-URL
patterns across every runtime; the controller routes each
breakpoint to the owning core's gate (event-gate, http-gate, or
both as appropriate).

### P7 — MAM rate-limit guardrail becomes "HTTP tap flips MAM's pause"

When the MAM client detects two requests issued within the minimum
interval, it calls `controller.pause(CoreId::Mam)` from inside its
`HttpTap::gate_request` and the second request parks before it
goes out. The operator sees the full request body of the would-be
violator. Other cores keep running normally.

Strict win over legacy: today the violation fires *after* the
second request is already issued; with the HTTP gate it parks
*before* `execute()` is called. The bad request never leaves the
host.

### P8 — Things we delete on purpose

- **`DebugDispatcher`** — dead code.
- **`DebuggableEventStream`** — gating moves into each
  `ServiceRuntime` via `RuntimeTap`.
- **`DebugState.debug_mode: bool`** — global flag replaced by
  per-core pause flags.
- **`DebugState.latest_state: SystemState`** — replaced by per-core
  state inside StepRecords.
- **`TraceEntry.state_before / state_after`** as global
  `SystemState` — replaced by per-core StepRecord state.
- **`PausedOn::Action { index, of }`** — no central batch; pause-
  point model becomes `Paused { core, kind: Event | Http, what }`.
- **Dryrun against legacy `SystemState`** — dropped entirely.
- **Queue manipulation (edit / inject / reorder / delete)** —
  dropped.
- **Action breakpoints in their current shape** — replaced by the
  flat variant-keyed registry.
- **`HttpObserver = Arc<dyn Fn(HttpExchange)>`** — replaced by
  `HttpTap`.
- **The entire current `/debug` React route** — replaced
  wholesale.
- **The "debug mode" name** — replaced throughout by
  "observability."
- **Operator vs maintainer view toggle** — never built. Single page.
- **Merged cross-core stream view** — dropped from v1. (Per-core
  streams are the model; if cross-core ordering becomes useful
  later, add it then.)

---

## Frontend layout (v1 sketch)

One page, three regions plus a header.

```
┌─ Header ──────────────────────────────────────────────────────────┐
│ Observability  ●Live   [Pause All]  [Step All]                    │
└───────────────────────────────────────────────────────────────────┘

┌─ Cores rail (left, ~220 px) ─┐  ┌─ Selected core: StepRecord stream ─┐
│ VPN     ▶ running             │  │ ─ MAM ─                             │
│ qBit    ‖ paused @ event      │  │                                     │
│ ▶ MAM   ‖ paused @ http       │  │ 18:42:17.013  StatusFetched  0.2ms ▶│
│ DB      ▶ running             │  │   actions: 0   publishes: 1         │
│ Disk    ▶ running             │  │   state Δ: connectable: false → true│
│ Docker  ▶ running             │  │   ↳ Publish(MamConnectable)         │
│ Domain  ▶ running             │  │      [→ 1 resulting event in Domain]│
│                               │  │                                     │
│ Selected core controls:       │  │ 18:42:16.002  TimerFired(KeepAlive)▼│
│ [Pause MAM]   [Step MAM]      │  │   cause: External (timer)           │
│ [Pause all]   [Step all]      │  │   actions:                          │
│                               │  │   • FetchStatus  → MAM /jsonLoad.php│
│ Breakpoints: [manage…]        │  │       200, 75ms  [view req/res]     │
│                               │  │   publishes: 0                      │
│                               │  │   state Δ: last_status_at +1.0s     │
│                               │  │                                     │
│                               │  │ 18:42:15.001  AuthSucceeded   …     │
│                               │  └─────────────────────────────────────┘
└───────────────────────────────┘
┌─ Bottom strip: tabs ──────────────────────────────────────────────┐
│ [HTTP] [Logs]                                                     │
│                                                                   │
│ HTTP (cross-core, last 500):                                      │
│ 18:42:16.077  MAM   POST /update_seedbox   200  150ms             │
│ 18:42:16.002  MAM   GET  /jsonLoad.php     200   75ms             │
│ 18:42:14.500  qBit  POST /api/v2/auth      200   30ms             │
│ …                                                                 │
└───────────────────────────────────────────────────────────────────┘
```

### Click behaviors (the causal graph by hand)

- Click an **action row** → highlight its HTTP exchange(s) in the
  HTTP tab and any event in any core whose cause is this action.
  Filter chip: "showing causes/effects of `FetchStatus` (uuid…)".
  The action's parent event is the row it's nested inside, so no
  extra jump is needed.
- Click a **publish row** → scroll the cores rail and per-core
  streams to highlight every downstream event whose `cause` is
  this publish. Filter chip: "showing events caused by
  `MamConnectable` (uuid…)". The publish's parent event is the
  row it's nested inside.
- Click an **event row** → if its cause is `Action(uuid)` or
  `Publish(uuid)`, jump to the originating action's or publish's
  parent event row (may be in another core's stream); the originating
  action/publish is highlighted inside that step. If `External`,
  expand to show the source (timer name, watcher path).
- Click an **HTTP exchange** in the bottom tab → jump to its
  action's parent event row in the originating core's stream,
  expand it, highlight the action.

### Row collapsed → expanded

A collapsed StepRecord row shows: timestamp, event variant,
duration, counts (`actions: N, publishes: M`), state-Δ one-liner.

Expanded adds: full event JSON, each action with its nested HTTP
exchange (full request and response), each publish with its
"jump to resulting events" link, full `state_after` JSON side-by-
side with previous `state_after`.

### What is *not* on the page

- No queue editing, no inject, no reorder, no delete.
- No dryrun.
- No state-diff against legacy `SystemState`.
- No "Enable Debug Mode" toggle.
- No operator/maintainer mode toggle.
- No merged cross-core stream.

---

## Open questions

Narrow now.

1. **Per-action gate revival.** Deferred for v1. Trait surface
   supports adding `gate_action(...)` later. Open until anyone
   misses it.
2. **HTTP ring de-dup of repeated polling.** A 5-minute MAM
   keep-alive fills the ring with 12 `GET /jsonLoad.php 200`
   entries per hour, displacing more interesting traffic. Options:
   keep as-is, fold consecutive identical-response polls into one
   row with a count, or two rings (one bounded by count, one
   bounded by *distinct* response). Probably do nothing for v1 and
   revisit if it becomes annoying.
3. **Breakpoint persistence.** Survive across page reloads?
   Across windlass restarts? Probably across reloads only (local
   storage); a restart is a clean slate.

---

## Proposed next steps

Implementation stories falling out of this doc. Sequencing is in
the dependency notes after.

1. **§37a — `secrecy` adoption.** Wrap MAM cookie, qBit password
   and any other in-code secrets in `SecretString`. Add the
   `expose_for_observability` helper. Stand-alone, can land
   independently of the rest.
2. **§37b — `Machine::State` accessor.** Add `type State:
   Serialize + Clone` and `fn state(&self) -> &Self::State` to
   the trait; each machine refactors its existing state into a
   `State` struct.
3. **§37c — `Timed<E>` causal extension.** Add `cause:
   EventCause` to `Timed<E>`; update every call site to construct
   via `Timed::from_action` / `Timed::from_publish` /
   `Timed::external`. Subscriber bridges that translate
   publish → event set the cause.
4. **§37d — `RuntimeTap` trait + per-core event gate.** Add
   `windlass-observability` crate. Wire `Arc<dyn RuntimeTap>` into
   `ServiceRuntime`, default `NullRuntimeTap`. Implement per-core
   pause/step in the `ObservabilityController`. Drop
   `DebugDispatcher` and `DebuggableEventStream`.
5. **§37e — `HttpTap` trait + per-client HTTP gate.** Replace
   `HttpObserver` with `HttpTap`. Update every client. Wire the
   MAM rate-limit guardrail through `gate_request`.
6. **§37f — StepRecord + always-on per-core rings.** Implement
   StepRecord (action_id, publish_id, event_cause, full
   `state_after`). Populate from `observed_step`. Build the SSE
   shape that backs the new page.
7. **§37g — Variant-keyed breakpoint registry.** Flat breakpoint
   list (events, actions, publishes, HTTP URL patterns) on the
   controller; controller routes each to the owning core's gate.
8. **§37h — New `/observability` frontend.** Wholesale
   replacement of `app/src/routes/Debug.tsx`. New SSE consumer,
   new components: cores rail, per-core StepRecord stream
   (collapsed/expanded rows, state Δ via in-browser diff),
   click-to-causal-jump, HTTP and log tabs.
9. **§37i — `PAUSE_ON_START` env-var re-wire.** Boot path
   constructs every runtime with its per-core pause flag pre-set.
10. **§37j — Rename `windlass-debug` → `windlass-observability`,
    `/debug` → `/observability`, and rewrite `docs/debug-mode.md`
    as `docs/observability.md`.** Mechanical rename + canonical
    spec rewrite. Last step.

### Dependency notes

- §37a (secrecy) is independent — can land any time.
- §37b (state accessor) is independent.
- §37c (Timed cause) is independent of §37b but needs to land
  before §37f so StepRecord can include `event_cause`.
- §37d (RuntimeTap) is independent of §37c but needs §37b for the
  state-accessor side.
- §37e (HttpTap) is independent of §37d but the MAM guardrail
  re-wire on top of it depends on §37d's controller.
- §37f (StepRecord + rings) depends on §37b, §37c, §37d.
- §37g (breakpoints) depends on §37d + §37e.
- §37h (frontend) depends on §37f and §37g (it consumes both SSE
  shapes).
- §37i is independent and tiny.
- §37j is the last step.

Each story is small. The discuss-first cost is paid in this doc;
implementation stories should not need their own design rounds.
