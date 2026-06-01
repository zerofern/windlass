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
   step a specific core to decide the cross-core execution order. The
   "Pause All / Step All" buttons are convenience wrappers around the
   per-core primitive.
3. **Gate granularity: three points, all per-core.**
   1. **Event gate** — pause at the top of a core's runtime loop, before
      `machine.handle` is called.
   2. **HTTP-request gate** — pause *before* an HTTP request is sent, with
      the full request body visible. Stops the offending request from
      ever leaving the host. (This is the MAM rate-limit story re-cast
      as a first-class capability.)
   3. **Per-action gate** between dispatches — *deferred for v1.* Easy
      to add later behind the same `DebugTap` trait.
4. **Observation is always on.** HTTP exchanges and the per-core event
   timeline are recorded into bounded rings regardless of whether any
   core is currently paused. Visiting `/debug` always shows the recent
   activity. "Debug mode" is therefore no longer a global modal flag —
   it's just the set of per-core pause flags that happen to be set.
5. **`Machine::State: Serialize` bound** — each core's state struct
   implements `Serialize`; the debug subsystem can snapshot any core on
   demand. No new trait method.
6. **`DEBUG_MODE_ON_START` is kept** as an entry point — at startup,
   every runtime is constructed with its per-core pause flag pre-set,
   so nothing fires until the operator releases them in the UI.
7. **Process: discuss-first.** This document is the artifact;
   implementation stories fall out only after the target shape is
   agreed.
8. **Scope cut for v1: queue manipulation (edit / inject / reorder /
   delete) is dropped.** Reduces surface area and removes a class of
   "the debugger broke the system" risk.
9. **Separation from normal logic is a hard goal.** `Machine` and
   `Shell` traits are untouched. The debug surface lives behind two
   small traits (`DebugTap`, `HttpHook`) plugged into the runtime and
   the HTTP clients respectively. A bug in `windlass-debug` cannot
   corrupt machine state, drop or reorder events, or change action
   dispatch — its blast radius is bounded to "hangs a runtime" or
   "loses timeline visibility."

---

## Original purpose (still valid)

The debug-mode goals from the existing `debug-mode.md` are still right:

- **Step through edge cases** in development without the system racing
  ahead.
- **Operate with confidence** in production over rate-sensitive
  external services (especially MAM).
- **Transparent**: debug mode never modifies events, actions, state, or
  HTTP requests — it only controls *when* each step is allowed to
  proceed.
- **Three entry points**: env var (`DEBUG_MODE_ON_START`), web UI
  toggle, MAM rate-limit guardrail.

The redesign preserves all four. What changes is the *shape* of the
underlying loop the debugger debugs.

---

## What changed in §36

Before §36, Windlass had one event loop and one `SystemState`. Debug
mode wrapped that loop: every `Event` paused at the loop's intake, every
`Action` paused at the loop's dispatcher, the trace recorded
before/after `SystemState`, and "the current pause point" was always one
of two things.

After §36 (closed 2026-06-01), the live decision-making runs on six
per-system cores — VPN, qBit, MAM, DB, disk, Docker — each on its own
generic `ServiceRuntime`. Each runtime has:

- Its own typed `Event` / `Command` channels.
- Its own machine `state` (sans-I/O).
- Its own shell that dispatches typed actions and emits typed publishes
  via `TopicFanout`.

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

Surfaces, in roughly this order: header (Live / Disconnected, "Debug
Mode" badge, "Paused on …" pill, Step / Skip / Enable-or-Disable
buttons); timeline of pending + current + recent trace events; detail
pane with state-diff before/after, action timeline, HTTP exchanges;
queue manipulation (delete, reorder, edit, inject); dryrun; breakpoint
lists; log panel.

### What works honestly today

- Event-side intake pause, step, skip, log panel, HTTP exchange
  recording for actions that still run through `make_http_observer`.
- Event breakpoints at the central intake.

### What is stale / misleading

- **`StateDiff` panel** renders `vpn / qbit / mam / known_torrents`
  fields off the legacy `SystemState` which is frozen at `initial()`.
- **`trace[].state_before` and `state_after`** are both
  `SystemState::initial()` — the operator-readable "what changed"
  story is gone.
- **`ActionTimeline` / "pending action batch"** shows what the legacy
  loop *would* have dispatched. The new per-core actions never
  populate this panel.
- **Action breakpoints** are wired into `DebugDispatcher`, which is
  no longer on the live path. They don't fire.
- **`PausedOn::Action { variant, index, of }`** is structurally
  unreachable — there is no central batch index to count against.
- **Dryrun** runs against legacy `SystemState`.
- **Queue edit / inject** still works on the *legacy event* stream,
  but the operator's mental model is broken because actions now happen
  inside per-core runtimes.

### Net assessment

About 40% of the operator-facing UI is now lying to the operator. The
plumbing underneath is mostly fine; the model it presents is wrong.

---

## Architecture: where the debug surface plugs in

The whole redesign is two new traits plus one new field on the existing
generic `ServiceRuntime<M, S>` struct. No `Machine` trait change. No
`Shell` trait change. There is no `Runtime` trait — there is one
generic struct, instantiated six times.

### `DebugTap` (runtime-side)

```rust
#[async_trait]
pub trait DebugTap: Send + Sync {
    /// Park here until the controller releases us. Called at the top of
    /// the runtime loop, before machine.handle. Returns immediately when
    /// this core's pause flag is not set.
    async fn gate_event(&self, core: CoreId, event_variant: &str);

    /// Fire-and-forget: broadcast what just happened to the central
    /// timeline. Always called, always pushes to the bounded ring.
    fn observed_outcome(
        &self,
        core: CoreId,
        event_variant: &str,
        actions: &[&dyn erased_serde::Serialize],
        publishes: &[&dyn erased_serde::Serialize],
    );

    /// Snapshot the machine's state on demand. The runtime calls this
    /// when the controller asks; default impl in the runtime delegates
    /// to the machine's `Serialize` impl.
    fn observed_state(&self, core: CoreId, state: &dyn erased_serde::Serialize);
}
```

One field added to `ServiceRuntime<M, S>`:

```rust
tap: Arc<dyn DebugTap>,  // NullDebugTap by default — both methods are no-ops
```

The run loop grows three lines, all reading like a clearly-separated
side concern:

```rust
let event = event_rx.recv().await?;
self.tap.gate_event(self.core_id, event.variant()).await;   // returns instantly when not paused
let outcome = self.machine.handle(Instant::now(), event);
self.tap.observed_outcome(self.core_id, ..., &outcome.actions, &outcome.publish);
self.apply(outcome.actions, outcome.publish);
```

### `HttpHook` (client-side)

Replaces today's `HttpObserver = Arc<dyn Fn(HttpExchange)>`:

```rust
#[async_trait]
pub trait HttpHook: Send + Sync {
    /// Park here until the controller releases us. Called between
    /// building the request and calling .execute(). The full request
    /// (method, URL, headers, body) is visible to the operator while
    /// parked.
    async fn gate_request(&self, core: CoreId, req: &HttpRequestView<'_>);

    /// Fire-and-forget: push the completed exchange to the always-on
    /// HTTP ring.
    fn observed_exchange(&self, core: CoreId, ex: &HttpExchange);
}
```

Each client (`MamClient`, `QbitClient`, future VPN-IP client, …) takes
one `Arc<dyn HttpHook>` at construction, tagged with its owning core's
`CoreId`. The default `NullHttpHook` is a no-op for `gate_request`;
`observed_exchange` always pushes to the central ring.

Inside a client, the call shape becomes:

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
- A bounded ring of recent events / outcomes (default ~500 per core, or
  one merged ring of ~3000 — open question).
- A bounded ring of recent HTTP exchanges (default ~500, always-on).
- An SSE broadcast for the `/debug` page.

Per-core pause is the primitive. Global pause = set every core's flag.
Per-core step = release one permit on that core's semaphore. Global
step = release one on every paused core. Same primitive, different UI
affordance.

### What this trait split buys us

- **`Machine` and `Shell` traits are untouched.** Property tests stay
  pointing at pure functions.
- **The runtime gains four lines, all using one trait object.** A
  reader of the runtime sees normal-path code with debug as a clearly
  separated side concern.
- **Clients gain two lines, both behind a trait object they don't have
  to understand.** A reader of `MamClient` sees "build request, gate,
  send, record" — the gate is one obvious step.
- **A bug in `windlass-debug` cannot corrupt machine state, misroute
  an event, drop an action, or reorder a dispatch.** The trait objects
  do not run inside the machine, the shell's dispatch, or the action
  application. The worst a tap bug can do is hang a runtime
  (`gate_event` never returns) or lose timeline visibility.

---

## Redesign principles

Each principle stands alone — if any one is wrong we argue about it
before code lands.

### P1 — Per-core gate is the primitive; global is convenience

Three gate points, each per-core, each independently toggleable:

| Gate          | Lives in                | Fires before                              |
| ------------- | ----------------------- | ----------------------------------------- |
| Event gate    | `ServiceRuntime` loop   | `machine.handle(event)` is called         |
| HTTP gate     | each HTTP client        | `execute()` sends the request             |
| Action gate   | *(deferred for v1)*     | *(between dispatches inside an outcome)*  |

Global Pause = "set every core's event-gate flag." Global Step = "add a
permit to every paused core's semaphore." Per-core Pause and Step are
the same operation scoped to one `CoreId`.

The UI exposes both — a global Pause All / Step All bar at the top, and
per-core Pause / Step buttons in each core's panel. This is the
"decide cross-core execution order" capability you asked for.

### P2 — Observation is always on; gating is sometimes on

There is no global `debug_mode: bool` in the hot path. The
`DebugTap.observed_*` and `HttpHook.observed_exchange` methods always
run and always push to the bounded rings. The `gate_*` methods always
run but return immediately when the relevant pause flag isn't set.

Practical consequence: the operator visits `/debug` and immediately
sees the recent event timeline and the last N HTTP exchanges, with no
"flip debug mode on first" step. Pausing is a separate action that
turns the page into a debugger.

Bounded ring sizes (initial): 500 events per-core (or one merged ring
of ~3000), 500 HTTP exchanges. Memory budget: ~5–10 MB peak. Tunable
later.

### P3 — Unified timeline, causally threaded by `action_id`

The operator sees one timeline, not six. Each entry is tagged with the
owning core. Rows are events, actions, publishes, and HTTP exchanges
from any runtime, ordered by their observation time at the controller.

Causal threading is by `action_id` (already supported by `CausalTx`):
an action emitted by core A whose I/O completion is observed by core
B's shell groups under the original action. The maintainer view can
collapse / expand by core; the operator view collapses to one
"what's happening right now" line per causal chain.

### P4 — Per-core machine state replaces `SystemState`

Each `M::State` implements `Serialize`. The controller asks each
runtime for its current state on demand (via a snapshot request on the
runtime's command channel, or by holding an `Arc<ArcSwap<M::State>>`
the runtime updates after each handle — open question).

The UI shows one collapsible panel per core, each rendering that core's
state as JSON. State-diff before/after a step is per-core: if a step
advanced the MAM runtime, the MAM panel shows the diff; the other
panels are unchanged. The global before/after `SystemState` in
`TraceEntry` goes away.

### P5 — Breakpoints are per-variant; presented as one flat list

The operator does not need to know that `MamAction::UpdateSeedbox` is
owned by the MAM runtime — they just want to break on it. The UI
shows one flat searchable list of every event variant + action variant
+ HTTP-call descriptor across every runtime; internally, the controller
routes each breakpoint to the owning core's pause path.

A "break on outbound HTTP to `/jsonLoad.php`" entry is supported
naturally — the HTTP hook checks the request URL against an
HTTP-breakpoint set before calling `gate_request`.

### P6 — MAM rate-limit guardrail becomes "HTTP hook flips MAM's pause"

When the MAM client detects two requests issued within the minimum
allowed interval, it calls `controller.pause(CoreId::Mam)` from inside
its `HttpHook::gate_request` and the second request parks before it
goes out. The operator sees the full request body of the would-be
violator and decides whether to step forward or discard. Other cores
keep running normally.

This is a strict win over the legacy behavior: today the violation
fires *after* the second request is already issued; with the HTTP gate
it parks *before* `execute()` is called. The bad request never leaves
the host.

### P7 — Operator vs maintainer split is a *view* concern

The debug controller exposes one underlying timeline + state model.
The `/debug` page renders two presets:

- **Operator view (default).** Pause All / Step All, the unified
  timeline collapsed by causal chain, per-core pause buttons, current
  pause point per core, latest per-core state summary, log tail, HTTP
  exchange ring. No breakpoint management. No per-core JSON state dump.
- **Maintainer view (toggle).** Adds breakpoint lists, per-core JSON
  state, expanded HTTP exchanges with full headers, the merged event
  ring with no collapsing.

The toggle is a frontend affordance only. Both views read the same
`DebugState`.

### P8 — Things we delete on purpose

- **`DebugDispatcher`** — dead code, no central action dispatcher.
- **`DebuggableEventStream`** — gating moves into each `ServiceRuntime`
  via `DebugTap`. The central legacy-event intake gate goes away.
- **`DebugState.debug_mode: bool`** — the global on/off flag no longer
  exists; replaced by per-core pause flags.
- **`DebugState.latest_state: SystemState`** — replaced by per-core
  serialized state.
- **`TraceEntry.state_before / state_after`** as global `SystemState`
  — replaced by per-core state at the affected core.
- **`PausedOn::Action { index, of }`** — there is no central batch;
  the pause-point model becomes
  `Paused { core: CoreId, kind: Event | Http, variant_or_url: String }`.
- **Dryrun against legacy `SystemState`** — drop entirely. Re-introduce
  per-core dryrun later if anyone misses it.
- **Queue manipulation (edit / inject / reorder / delete)** — dropped.
- **Action breakpoints in their current shape** — replaced by
  per-variant breakpoints routed by the controller to the owning core.
- **`HttpObserver = Arc<dyn Fn(HttpExchange)>`** — replaced by the
  richer `HttpHook` trait with both `gate_request` and
  `observed_exchange`.

---

## Open questions

Now narrower — most v1 decisions are made.

1. **Per-core ring vs one merged ring?** Per-core gives 500 × N
   events; merged gives 3000 events with explicit ordering. Merged is
   easier to render as a timeline; per-core is easier to inspect a
   specific machine's history. Probably merged at the controller, with
   the UI offering per-core filtering. Confirm.
2. **HTTP-body redaction.** Some MAM and qBit requests carry session
   cookies / API keys in headers, and possibly secrets in bodies.
   The ring stores these verbatim today. For an *operator-facing*
   surface that's borderline OK (single-user system), but worth a
   deliberate decision: redact at capture, redact at display, or trust
   the operator. Lean toward "redact obvious secrets at capture
   (Authorization headers, MAM cookie), display the rest verbatim."
3. **State snapshot delivery.** Two options:
   - *Pull:* runtime serializes state on each handle into an
     `Arc<ArcSwap<Value>>`; controller reads on demand. One alloc per
     event.
   - *Push:* controller sends a `Snapshot` command on the runtime's
     command channel; runtime replies. No per-event cost, but adds a
     command variant per machine.
   Probably pull — the cost is tiny and the model is simpler.
4. **Naming.** "Debug mode" is now misleading (it's an
   always-on observability surface that can also gate). Rename to
   "Inspector" / "Watch" / something? Or just keep "Debug" and
   document the framing shift? Lean toward keeping "Debug" — renaming
   churns code and the operator already knows the page.
5. **Per-action gate revival.** Deferred for v1. The trait surface
   already supports it (add `gate_action(...)` to `DebugTap`). Question
   is whether anyone wants it once event-gate + HTTP-gate is in.

---

## Proposed next steps

Once this doc is reviewed, the implementation falls out as small
follow-up stories:

1. **§37a — `DebugTap` trait + per-core event gate.** Add the trait in
   `windlass-debug`, wire `Arc<dyn DebugTap>` into `ServiceRuntime`,
   default to `NullDebugTap`. Implement per-core pause/step in the
   controller. Drop `DebugDispatcher` and `DebuggableEventStream`.
2. **§37b — `HttpHook` trait + per-client HTTP gate.** Replace
   `HttpObserver` with `HttpHook` in `windlass-types`. Update every
   client (`MamClient`, `QbitClient`, …) to take and call it. Wire
   the MAM rate-limit guardrail through `gate_request`.
3. **§37c — Always-on rings.** Move the event/outcome ring and HTTP
   ring into the controller and populate them unconditionally. Build
   the SSE shape that backs the new `/debug` page.
4. **§37d — Per-core state snapshots.** `M::State: Serialize` bound on
   the `Machine` trait. Replace the legacy `SystemState` slot in
   `DebugState` with a map of `CoreId → serialized state`.
5. **§37e — Variant-keyed breakpoint registry.** Flat breakpoint list
   on the controller; the controller routes each variant to the owning
   core's gate.
6. **§37f — Operator vs maintainer view toggle.** Frontend only;
   reorganize the existing `/debug` page into two presets, drop the
   stale panels (state-diff, action timeline, dryrun, queue
   manipulation).
7. **§37g — `DEBUG_MODE_ON_START` re-wiring.** Boot path constructs
   every runtime with its per-core pause flag pre-set. Unchanged in
   intent; trivial in the new shape.
8. **§37h — Rewrite `docs/debug-mode.md`** as the canonical spec
   reflecting the post-§37 reality.

Each story is small. The discuss-first cost is paid in this doc; the
implementation stories should not need their own design rounds.
