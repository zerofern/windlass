# Debug-Mode Review and Redesign — §37 Discussion Doc

This is the discussion artifact for operator-readiness §37 ("Review and
Consolidate Debug-Mode Workflow"). It audits the debug-mode surface as it
exists after the §36 per-system core migration, names what is stale or
actively misleading now, and sketches the shape of a redesign at the
principle level.

The canonical operator-facing spec, `docs/debug-mode.md`, is the *old*
spec. It still reads as authoritative but describes a system that no
longer exists: one event loop, one `SystemState`, one ordered action
batch per event. This doc is what we work from until `debug-mode.md` is
rewritten.

---

## Anchoring decisions (agreed)

1. **Audience: both, equally.** Operator-facing surface (MAM rate-limit
   safety net, "pause before this and let me look") *and* maintainer
   surface (per-core stream detail for diagnosing core behavior).
2. **Pause model: per-core is the primitive; global is "do it to every
   core."** The operator can stop one or more cores independently, and
   step a specific core to decide cross-core execution order. The
   "Pause All / Step All" buttons are convenience wrappers around the
   per-core primitive.
3. **Gate granularity: three points, all per-core.**
   1. **Event gate** — pause at the top of a core's runtime loop, before
      `machine.handle` is called.
   2. **HTTP-request gate** — pause *before* an HTTP request is sent,
      with the full request body visible.
   3. **Per-action gate** between dispatches — *deferred for v1.*
4. **Observation is always on, for everything.** Debug is a general
   observability layer, not a mode you toggle. Events, actions,
   publishes, per-core state snapshots, HTTP exchanges, and log lines
   are recorded into bounded rings unconditionally. "Debug mode" is
   no longer a global flag — it's just the set of per-core pause
   flags that happen to be set.
5. **Timeline primitive: the per-core `StepRecord`.** One event +
   the actions it produced + the publishes it produced + the state
   snapshot at the end + the duration + the threaded `action_id`s.
   The operator collapses each record to a one-liner or expands to
   see everything that one event caused. HTTP exchanges live in a
   parallel ring and join back to a step record via `action_id` when
   rendered.
6. **`Machine` gains a state accessor** so the runtime can snapshot
   on demand. Shape is one of two options (open question 1 below):
   either `Machine: Serialize` (whole machine) or
   `type State: Serialize + Clone` + `fn state(&self) -> &Self::State`.
   Either way, no debug-awareness leaks into `Machine` — the
   accessor is general purpose.
7. **`Shell` trait is untouched.** HTTP capture lives at client
   construction via the new `HttpHook` callback.
8. **`DEBUG_MODE_ON_START` is kept** as an entry point — every runtime
   is constructed with its per-core pause flag pre-set.
9. **Scope cut for v1: queue manipulation (edit / inject / reorder /
   delete) is dropped.** Reduces surface area and removes a class of
   "the debugger broke the system" risk.
10. **The frontend `/debug` page is fully redesigned.** The current
    page is not iterated — it is replaced. New layout sketched in
    "Frontend layout" below.
11. **Separation from normal logic is a hard goal.** A bug in
    `windlass-debug` cannot corrupt machine state, drop or reorder
    events, or change action dispatch — its blast radius is bounded
    to "hangs a runtime" or "loses timeline visibility."
12. **Process: discuss-first.** This document is the artifact;
    implementation stories fall out only after the target shape is
    agreed.

---

## Original purpose (still valid)

The debug-mode goals from the existing `debug-mode.md` are still right:
step through edge cases in development without the system racing ahead;
operate with confidence in production over rate-sensitive external
services (especially MAM); transparent — never modify events, actions,
state, or HTTP requests, only control *when* each step proceeds; three
entry points (env var, web UI toggle, MAM rate-limit guardrail).

The redesign preserves all four. What changes is the *shape* of the
underlying loop the debugger debugs, and the *surface* the operator
sees.

---

## What changed in §36

Before §36, Windlass had one event loop and one `SystemState`. Debug
mode wrapped that loop: every `Event` paused at the loop's intake, every
`Action` paused at the loop's dispatcher, the trace recorded
before/after `SystemState`, and "the current pause point" was always one
of two things.

After §36 (closed 2026-06-01), the live decision-making runs on six
per-system cores — VPN, qBit, MAM, DB, disk, Docker — each on its own
generic `ServiceRuntime`. Each runtime has its own typed `Event` /
`Command` channels, its own machine `state` (sans-I/O), and its own
shell that dispatches typed actions and emits typed publishes via
`TopicFanout`.

The legacy `windlass-core::SystemState` still exists as the *bridge
protocol* between the few remaining I/O sites and the service-events
bridge, but its `process_event` is gone and the dashboard view of it
is frozen at `initial()`. The central shell loop is now:

```
recv legacy Event → debug!(?event) → service_cores.observe(&event)
```

Everything interesting happens inside the per-system runtimes that
`observe` fans out to. There is no central action batch and no central
state.

---

## Audit: the debug-mode surface today

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

The loop has been gutted as part of §36 step 8. There is no central
action dispatch. The only event-side debug hook left is the intake
pause inside `dequeue_debug` / `DebuggableEventStream`. Once an event
is forwarded to `service_cores.observe(&event)`, the central loop has
no visibility into what happens next.

### Backend — per-core runtimes

The per-core `ServiceRuntime`s currently have **no debug-mode
integration at all**. They process events at full speed regardless of
the global debug flag. This is the single biggest gap.

### Frontend — `/debug` route (`app/src/routes/Debug.tsx`, 769 LOC)

To be replaced wholesale (see "Frontend layout" below). About 40% of
what it shows is now lying to the operator:

- **`StateDiff` panel** renders fields off the legacy `SystemState`
  which is frozen at `initial()` — every event shows "no change."
- **`ActionTimeline` / "pending action batch"** shows what the legacy
  loop *would* have dispatched. New per-core actions never populate it.
- **Action breakpoints** wired into the dead `DebugDispatcher` —
  don't fire.
- **`PausedOn::Action { variant, index, of }`** structurally
  unreachable.
- **Dryrun** runs against legacy `SystemState`.
- **Queue edit / inject / reorder** still works on the legacy event
  stream but the operator's mental model is broken because actions
  happen inside per-core runtimes.

---

## Architecture: where the debug surface plugs in

The whole redesign is two new traits, one new field on
`ServiceRuntime<M, S>`, one new accessor on `Machine`. No `Shell`
trait change. There is no `Runtime` trait — there is one generic
struct, instantiated six times.

### `DebugTap` (runtime-side)

```rust
#[async_trait]
pub trait DebugTap: Send + Sync {
    /// Park here until the controller releases us. Called at the top
    /// of the runtime loop, before machine.handle. Returns immediately
    /// when this core's pause flag is not set.
    async fn gate_event(&self, core: CoreId, event_variant: &str);

    /// Fire-and-forget: emit one StepRecord into the per-core ring.
    /// Always called, always populates the ring.
    fn observed_step(&self, core: CoreId, step: StepRecord<'_>);
}

pub struct StepRecord<'a> {
    pub at: Instant,                            // event.at (logical time)
    pub duration: Duration,                     // handle() wall time
    pub kind: StepKind,                         // Event | Command
    pub event_variant: &'a str,
    pub event: &'a dyn erased_serde::Serialize, // the Timed<E> payload
    pub state_after: &'a dyn erased_serde::Serialize, // M::State snapshot
    pub actions: &'a [(Uuid, &'a dyn erased_serde::Serialize)], // action_id + payload
    pub publishes: &'a [&'a dyn erased_serde::Serialize],
}
```

One field added to `ServiceRuntime<M, S>`:

```rust
tap: Arc<dyn DebugTap>,  // NullDebugTap by default — both methods are no-ops
```

The run loop grows four lines, all reading like a clearly-separated
side concern:

```rust
let event = event_rx.recv().await?;
self.tap.gate_event(self.core_id, event.variant_name()).await;
let t0 = Instant::now();
let outcome = self.machine.handle(t0, event);
let duration = t0.elapsed();
self.tap.observed_step(self.core_id, StepRecord {
    at: event.at, duration, kind: StepKind::Event,
    event_variant: event.variant_name(),
    event: &event, state_after: self.machine.state(),
    actions: &outcome.actions_with_ids,
    publishes: &outcome.publish,
});
self.apply(outcome.actions, outcome.publish);
```

State-before-step-N is just state-after-step-N−1. The very first step
record's "before" is the machine's `new()` state.

### `HttpHook` (client-side)

Replaces today's `HttpObserver = Arc<dyn Fn(HttpExchange)>`:

```rust
#[async_trait]
pub trait HttpHook: Send + Sync {
    /// Park here until the controller releases us. Called between
    /// building the request and calling .execute(). The full request
    /// (method, URL, headers, body) is visible while parked.
    async fn gate_request(&self, core: CoreId, req: &HttpRequestView<'_>);

    /// Fire-and-forget: push the completed exchange to the always-on
    /// HTTP ring. The action_id (read from the task-local CausalTx)
    /// threads it back to the StepRecord that emitted the action.
    fn observed_exchange(&self, core: CoreId, ex: &HttpExchange);
}
```

Each client (`MamClient`, `QbitClient`, future VPN-IP client, …) takes
one `Arc<dyn HttpHook>` at construction, tagged with its owning core.
Inside a client:

```rust
let req = self.client.post(&url).json(&body).build()?;
self.hook.gate_request(self.core, &HttpRequestView::from(&req)).await;
let res = self.client.execute(req).await?;
self.hook.observed_exchange(self.core, &HttpExchange { /* … */ });
```

### Shared controller (`windlass-debug::DebugController`)

One controller, attached to every runtime's `DebugTap` and every
client's `HttpHook`. Holds:

- Per-core `paused: AtomicBool` (one per core).
- Per-core `step_permits: Semaphore` (one per core).
- **Per-core ring of `StepRecord`s** (~500 deep each, always-on).
- **One cross-core ring of HTTP exchanges** (~500 deep, always-on).
- **One cross-core ring of log lines** (already exists, kept as-is).
- An SSE broadcast for the `/debug` page.

Per-core pause is the primitive. Global pause = set every core's flag.
Per-core step = release one permit on that core's semaphore. Global
step = release one on every paused core.

### What this trait split buys us

- **`Machine::handle` is untouched.** Property tests stay pointing at
  pure functions. The only new method on `Machine` is the read-only
  state accessor (see open question 1 for the exact shape).
- **`Shell` is untouched.**
- **The runtime gains four lines, all using one trait object.** A
  reader of the runtime sees normal-path code with debug as a clearly
  separated side concern.
- **Clients gain two lines, behind a trait object they don't have to
  understand.** A reader of `MamClient` sees "build request, gate,
  send, record."
- **A bug in `windlass-debug` cannot corrupt machine state, misroute
  an event, drop an action, or reorder a dispatch.** The trait objects
  do not run inside the machine, the shell's dispatch, or action
  application. Worst case: a tap bug hangs a runtime or loses
  timeline visibility.

---

## Redesign principles

### P1 — Per-core gate is the primitive; global is convenience

Three gate points, each per-core, each independently toggleable:

| Gate          | Lives in              | Fires before                              |
| ------------- | --------------------- | ----------------------------------------- |
| Event gate    | `ServiceRuntime` loop | `machine.handle(event)` is called         |
| HTTP gate     | each HTTP client      | `execute()` sends the request             |
| Action gate   | *(deferred for v1)*   | *(between dispatches inside an outcome)*  |

Global Pause = set every core's event-gate flag. Global Step = add a
permit to every paused core's semaphore. Per-core Pause / Step are
the same operation scoped to one `CoreId`.

### P2 — Observation is always on; gating is sometimes on

Five always-populated streams: per-core StepRecord rings, the
cross-core HTTP ring, and the cross-core log ring. No global
`debug_mode: bool` in the hot path. `tap.observed_step(...)` and
`hook.observed_exchange(...)` always run. `gate_event(...)` and
`gate_request(...)` always run too, but return immediately when the
relevant pause flag isn't set.

Practical consequence: visit `/debug` and you immediately see the
recent event flow and last N HTTP exchanges, no toggle. Pausing is a
separate action that turns the page into a debugger.

Bounded ring sizes (initial): 500 StepRecords per-core, 500 HTTP
exchanges. Memory budget: ~10 MB peak. Tunable later.

### P3 — Each per-core StepRecord binds event + actions + publishes + state

The data primitive is the StepRecord (shape above): one event in, all
side effects out, state after, threaded action_ids. The operator
collapses each record to a one-liner ("`StatusFetched` → 0 actions, 1
publish, no state change") or expands to see the full event payload,
each action with its dispatched-or-pending status, each publish, the
state diff against the previous record, and the duration.

HTTP exchanges land in the cross-core HTTP ring with the action_id from
`CausalTx`. When rendering a step record, the UI looks up matching
exchanges and nests them under their parent action. The cross-core
view of "all HTTP" stays usable on its own.

This is the headline UX change. Today the operator has to mentally pair
an event with its actions and resulting HTTP. The step record does it
structurally.

### P4 — Per-core machine state replaces `SystemState`

Each runtime snapshots its machine's state after every handle, via the
`Machine` state accessor (open question 1 — `Serialize` or
`type State`). The snapshot is part of the StepRecord. State-diff
is computed in the UI by diffing one record's `state_after` against
the previous record's.

The global `SystemState` view is deleted. No "current system state"
panel; just one collapsible per-core state pane in the maintainer view.

### P5 — Breakpoints are per-variant; presented as one flat list

The operator does not need to know that `MamAction::UpdateSeedbox` is
owned by the MAM runtime. The UI shows one flat searchable list of
event variants + action variants + HTTP-call descriptors across every
runtime; the controller routes each breakpoint to the owning core's
gate.

Breakpoints live in the maintainer view, not the operator view.

### P6 — MAM rate-limit guardrail becomes "HTTP hook flips MAM's pause"

When the MAM client detects two requests issued within the minimum
allowed interval, it calls `controller.pause(CoreId::Mam)` from inside
its `HttpHook::gate_request` and the second request parks before it
goes out. The operator sees the full request body of the would-be
violator. Other cores keep running normally.

Strict win over legacy: today the violation fires *after* the second
request is already issued; with the HTTP gate it parks *before*
`execute()` is called. The bad request never leaves the host.

### P7 — Operator vs maintainer split is a *view* concern

One underlying data model. The `/debug` page renders two presets — see
"Frontend layout" below. The toggle is a frontend affordance only.

### P8 — Things we delete on purpose

- **`DebugDispatcher`** — dead code, no central action dispatcher.
- **`DebuggableEventStream`** — gating moves into each `ServiceRuntime`.
- **`DebugState.debug_mode: bool`** — global flag replaced by per-core
  pause flags.
- **`DebugState.latest_state: SystemState`** — replaced by per-core
  state inside StepRecords.
- **`TraceEntry.state_before / state_after`** as global `SystemState`
  — replaced by per-core StepRecord state.
- **`PausedOn::Action { index, of }`** — there is no central batch;
  the pause-point model becomes
  `Paused { core: CoreId, kind: Event | Http, what: String }`.
- **Dryrun against legacy `SystemState`** — drop entirely.
- **Queue manipulation (edit / inject / reorder / delete)** — dropped.
- **Action breakpoints in their current shape** — replaced by
  per-variant breakpoints routed by the controller.
- **`HttpObserver = Arc<dyn Fn(HttpExchange)>`** — replaced by
  `HttpHook` with both `gate_request` and `observed_exchange`.
- **The entire current `/debug` React route** — replaced wholesale.

---

## Frontend layout (v1 sketch)

The new page has three regions plus a header. The current 340 px
timeline + detail-pane split is dropped.

```
┌─ Header ──────────────────────────────────────────────────────────┐
│ Debug  ●Live   [Pause All]  [Step All]   View: ◉Operator ○Maintainer│
└───────────────────────────────────────────────────────────────────┘

┌─ Cores rail (left, ~220 px) ─┐  ┌─ Selected core: StepRecord stream ─┐
│ VPN     ▶ running             │  │ ─ MAM ─                             │
│ qBit    ‖ paused @ event      │  │                                     │
│ ▶ MAM   ‖ paused @ http       │  │ 18:42:17.013  StatusFetched  0.2ms ▶│
│ DB      ▶ running             │  │   actions: 0   publishes: 1         │
│ Disk    ▶ running             │  │   state Δ: connectable: true → true │
│ Docker  ▶ running             │  │                                     │
│ Domain  ▶ running             │  │ 18:42:16.002  TimerFired(KeepAlive)▼│
│                               │  │   actions:                          │
│ Selected core controls:       │  │   • FetchStatus  → MAM /jsonLoad.php│
│ [Pause MAM]   [Step MAM]      │  │       200, 75ms  [view req/res]     │
│ [Pause all]   [Step all]      │  │   publishes: 0                      │
│                               │  │   state Δ: last_status_at +1s       │
│                               │  │                                     │
│                               │  │ 18:42:15.001  AuthSucceeded  …      │
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

### Operator view (default)

- Header with Live/Disconnected indicator, Pause All / Step All.
- Cores rail with per-core run/paused-where state and per-core
  Pause / Step buttons.
- Center: the selected core's StepRecord stream, each row collapsible
  to "event → N actions, M publishes, state Δ summary" or expandable
  to show every field. The selected pause point (if any) is the row
  the gate is parked on, highlighted at the top.
- Bottom strip: HTTP ring and log tail as tabs.

### Maintainer view (toggle)

Adds:

- A flat breakpoint manager (event variants, action variants, HTTP
  URL patterns), with the breakpoints active across all cores.
- Full state JSON pane (collapsible) per core.
- HTTP exchanges expand to show all headers, exact body, timing
  breakdown.
- A merged cross-core StepRecord stream (interleaved view), in
  addition to the per-core stream.

### What is *not* on the new page

- No queue editing, no inject, no reorder, no delete.
- No dryrun.
- No state-diff against the legacy `SystemState`.
- No global "Enable Debug Mode" button (debug observation is always
  on; pausing is the action).

---

## Open questions

1. **`Machine` state accessor shape.** Two options:
   - *(a)* `Machine: Serialize` — whole machine serializes,
     includes config and any caches. Zero new trait surface.
   - *(b)* `type State: Serialize + Clone; fn state(&self) -> &Self::State`
     — clean read-only view of *just* the state, but requires every
     machine to factor state into its own struct.
   Lean toward (b): the snapshot is meant to be the "what is this
   machine's current mind," not "the entire object." Most machines
   already have an internal state struct; the ones that don't get
   refactored as part of §37d.
2. **HTTP-body redaction.** MAM and qBit requests carry session
   cookies / API keys. The ring stores these verbatim today. For an
   operator-facing surface on a single-user system this is borderline
   OK, but worth a deliberate decision: redact `Authorization` /
   `Cookie` / known MAM cookie at capture, display the rest verbatim.
3. **Per-action gate revival.** Deferred for v1. The trait surface
   supports adding `gate_action(...)` later. Question is whether
   anyone wants it once event-gate + HTTP-gate is in.
4. **Cross-core merged stream rendering.** Operator view shows
   per-core only; maintainer view adds a merged stream. Order by
   `StepRecord.at` (logical) or by observation time at the
   controller? Probably `at` — matches the per-core view's ordering.

---

## Proposed next steps

Once this doc is reviewed, the implementation falls out as small
follow-up stories:

1. **§37a — `DebugTap` trait + per-core event gate.** Add the trait
   in `windlass-debug`, wire `Arc<dyn DebugTap>` into `ServiceRuntime`,
   default to `NullDebugTap`. Implement per-core pause/step in the
   controller. Drop `DebugDispatcher` and `DebuggableEventStream`.
2. **§37b — `HttpHook` trait + per-client HTTP gate.** Replace
   `HttpObserver` with `HttpHook`. Update every client to take and
   call it. Wire the MAM rate-limit guardrail through `gate_request`.
3. **§37c — StepRecord + always-on per-core rings.** Implement the
   StepRecord shape in `windlass-debug`. Populate from `observed_step`.
   Build the SSE shape that backs the new `/debug` page.
4. **§37d — `Machine` state accessor.** Add the state accessor (form
   per open question 1). Each machine refactors its state into the
   accessor's shape. Snapshot is taken inside the runtime after each
   handle and included in the emitted StepRecord.
5. **§37e — Variant-keyed breakpoint registry.** Flat breakpoint list
   on the controller; the controller routes each variant to the
   owning core's gate. Includes HTTP-URL-pattern breakpoints used by
   the HTTP gate.
6. **§37f — New `/debug` frontend.** Wholesale replacement of
   `app/src/routes/Debug.tsx` with the layout above. New
   StepRecord-shaped types, new SSE consumer, new components. Both
   Operator and Maintainer presets in the same build.
7. **§37g — `DEBUG_MODE_ON_START` re-wiring.** Boot path constructs
   every runtime with its per-core pause flag pre-set. Trivial in
   the new shape.
8. **§37h — Rewrite `docs/debug-mode.md`** as the canonical spec
   reflecting the post-§37 reality.

Sequencing: 37a + 37b can land in either order; 37c depends on 37a;
37d depends on 37c; 37e depends on 37a + 37b; 37f depends on 37c +
37d (it consumes the new SSE shape); 37g is independent and tiny;
37h is the last step.

Each story is small. The discuss-first cost is paid in this doc; the
implementation stories should not need their own design rounds.
