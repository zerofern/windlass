# Debug-Mode Review and Redesign — §37 Discussion Doc

This is the discussion artifact for operator-readiness §37 ("Review and
Consolidate Debug-Mode Workflow"). It audits the debug-mode surface as it
exists after the §36 per-system core migration, names what is stale or
actively misleading now, and sketches the shape of a redesign at the
principle level. **It is not a spec.** No code changes until the audit and
target shape are agreed.

The canonical operator-facing spec, `docs/debug-mode.md`, is the *old*
spec. It still reads as authoritative but describes a system that no
longer exists: one event loop, one `SystemState`, one ordered action
batch per event. This doc is what we work from until `debug-mode.md` is
rewritten.

---

## Anchoring decisions (already agreed)

These three were settled before the audit was written; the redesign
proceeds from them.

1. **Audience: both, equally.** Debug mode stays an operator-facing
   surface (MAM rate-limit safety net, "pause before this and let me
   look") *and* a maintainer surface (per-core stream detail for
   diagnosing core behavior). The UX has to serve both — operator gets
   a simple pause/inspect; maintainer gets per-core detail behind a
   toggle.
2. **Pause model: global.** One "Pause" button halts every core at its
   next event boundary. Per-core pause is rejected; causal/scoped pause
   is deferred. Step semantics still need to be designed but operate on
   the same global model.
3. **Process: audit/redesign doc first.** Per the project's
   discuss-first docs workflow. This document is that artifact. Code
   stories fall out only after the target shape is agreed.

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
- Its own internal pause/step opportunities, if any are added.

The legacy `windlass-core::SystemState` still exists as the *bridge
protocol* between the few remaining I/O sites and the service-events
bridge, but its `process_event` is gone and the dashboard view of it
is frozen at `initial()`. The central shell loop is now:

```
recv legacy Event → debug!(?event) → service_cores.observe(&event)
```

Everything interesting happens inside the per-system runtimes that
`observe` fans out to.

---

## Audit: the debug-mode surface today

### Backend — `windlass-debug` (`~2050 LOC`)

| Module             | Status post-§36                                                                                |
| ------------------ | ---------------------------------------------------------------------------------------------- |
| `DebugController`  | Mostly alive. `enable/disable`, `step`, `skip`, `paused_on`, breakpoints, snapshot — all work. |
| `DebuggableEventStream` | Alive. Pauses at the central legacy-event intake.                                         |
| `DebugHistory`     | Half-alive. Event queue + log capture work; per-event before/after `SystemState` is meaningless (legacy frozen at `initial()`). |
| `DebugDispatcher`  | **Dead.** Was the central action dispatcher; the central loop no longer dispatches actions.    |
| `CausalTx`         | Alive. Task-local action id for HTTP-exchange threading. Works inside any runtime that uses it. |
| `make_http_observer` | Alive. Per-action HTTP exchange callback. No-op when debug is off; routes to `exchange_rx` when on. |
| `DebugLogLayer`    | Alive. Tracing layer captures log lines for the UI panel.                                      |
| `DebugState`       | Half-alive. The shape still serializes, but `latest_state`, `trace[].state_before/after`, and `running_actions` reflect a model that no longer matches reality. |

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

Surfaces, in roughly this order:

- **Header.** Live/Disconnected, "Debug Mode" badge, "Paused on …"
  pill, Step / Skip / Enable-or-Disable buttons.
- **Timeline (left, 340px).** Queue items (pending events), the
  current event, and the recent `trace`. Each timeline row can be
  selected.
- **Detail pane (right).** For a queue item: payload, edit / dryrun
  controls. For a trace or current event: payload, **state-diff
  before/after**, action timeline with per-action HTTP exchanges.
- **Queue manipulation.** Delete, reorder (move up/down), edit
  payload, inject a new event at a specific position.
- **Dryrun.** "Run this event against current state and show the
  resulting actions / next-state — but don't dispatch it."
- **Breakpoint lists.** Two columns: Event Breakpoints, Action
  Breakpoints. Click a variant to toggle.
- **Log panel** at the bottom.

### What works honestly today

- **Event-side intake pause**, step, skip, queue manipulation, log
  panel, HTTP exchange recording for actions that *do* still run
  through `make_http_observer`.
- **Event breakpoints** at the central intake.
- **The MAM rate-limit guardrail**'s intent is still valid; whether
  the *trigger path* still works needs verifying (it lived in the
  legacy MAM client; the new MAM core may or may not still call
  `enable_debug()`).

### What is stale / misleading

- **`StateDiff` panel** renders `vpn / qbit / mam / known_torrents`
  fields off the legacy `SystemState` which is frozen at `initial()`.
  Every event shows "no change" because there is no change.
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
- **Dryrun** runs against legacy `SystemState`. It returns
  uninteresting results for the same reason `StateDiff` does.
- **Queue edit / inject** still works on the *legacy event* stream,
  but the operator's mental model is "this event will produce these
  actions next" — and that prediction no longer holds because the
  actions happen inside whichever per-core runtime the event is
  bridged into.

### Net assessment

About 40% of the operator-facing UI is now lying to the operator. The
plumbing underneath is mostly fine; the model it presents is wrong.

---

## Redesign principles

Below is the target shape, derived from the anchoring decisions plus
the audit. Each principle stands alone — if any one is wrong we want
to argue about it before code lands.

### P1 — Global pause is a barrier across every runtime

Enabling debug mode raises a single global pause flag. At the start of
every runtime's event loop iteration, the runtime checks the flag and,
if set, awaits the global step semaphore before processing its next
event. The central legacy-event intake checks the same flag.

Concretely: `DebugController` exposes an `await_step_if_paused()`
helper used by every `ServiceRuntime`'s event loop. When the user
clicks Pause, within one event boundary per runtime, every runtime is
parked. When the user clicks Step, every parked runtime is released
exactly once and re-parks on its next iteration.

This is the cheapest design that delivers "the operator hits Pause and
nothing else happens until they say so." It does not pretend per-core
pause is a goal.

### P2 — The unified timeline is *one* causally-threaded stream

The operator sees one timeline, not six. Each entry is tagged with the
owning core (`Vpn` / `Qbit` / `Mam` / `Db` / `Disk` / `Docker` /
`Domain`). Rows are events, actions, and publishes from any runtime,
ordered by their observation time at the debug controller.

Causal threading is by `action_id` (already supported by `CausalTx`):
an action emitted by core A whose I/O completion is observed by core
B's shell groups under the original action. The maintainer view can
collapse / expand by core; the operator view can collapse to one
"what's happening right now" line per timestamp.

### P3 — Per-core machine state is the new "state snapshot"

`SystemState` is gone as a debug artifact. Replacement: each core
serializes its machine state on request. The debug UI shows one
collapsible panel per core, each rendering that core's `state` as
JSON. State-diff before/after a step is per-core: if a step advanced
the MAM runtime, the MAM panel shows the diff; the other panels are
unchanged.

This rules out the global `before_state` / `after_state` shape in
`TraceEntry`.

### P4 — Breakpoints are per-variant, per-runtime, but selected
**from a single flat list**

The operator does not need to know that `MamAction::UpdateSeedbox` is
owned by the MAM runtime — they just want to break on it. The breakpoint
list is one flat searchable list of every event variant + action
variant across every runtime; internally, the controller routes the
breakpoint to the owning runtime's `await_step_if_paused()` check.

This keeps the operator-facing UX simple and makes the maintainer
view's per-core grouping a presentation concern, not a model concern.

### P5 — The MAM rate-limit guardrail still calls `enable_debug()`

This is the load-bearing operator-safety feature and survives the
redesign unchanged in intent: when the MAM shell detects two calls
within the minimum interval, it flips the global pause flag. P1
guarantees every other runtime parks at its next event boundary.
The MAM runtime parks before the offending second call goes out.

Explicit non-goal: the guardrail does not try to be smarter (e.g.
"pause only MAM"). The whole system halts; the operator picks up.

### P6 — HTTP-exchange recording and `CausalTx` are unchanged

The `make_http_observer` / `CausalTx` mechanism does not depend on a
central loop; it only depends on the action being executed inside a
`CausalTx::run` scope with the action's UUID. Every per-core shell
already runs actions; the redesign requires each per-core shell to
wrap its dispatch in `CausalTx::run` so the existing observer routes
exchanges correctly. This is a small wiring change, not a model
change.

### P7 — Operator vs maintainer split is a *view* concern

The debug controller exposes one underlying timeline + state model.
The `/debug` page renders two presets:

- **Operator view (default).** Pause, Step, Skip, the unified
  timeline collapsed to one line per causal chain, current pause
  point, latest per-core state summary, log tail. No queue
  manipulation. No dryrun. No per-action HTTP exchange browser.
- **Maintainer view (toggle).** Adds breakpoint lists, queue
  manipulation, per-core state JSON, expanded HTTP exchanges,
  dryrun-against-a-machine.

The toggle is a UI affordance, not a backend mode. Both views read
the same `DebugState`.

### P8 — Things we delete on purpose

- `DebugDispatcher` (central action dispatcher) — dead code.
- `DebugState.latest_state: SystemState` — replaced by per-core
  serialized state.
- `TraceEntry.state_before / state_after` as global `SystemState` —
  replaced by per-core state at the affected core.
- `PausedOn::Action { index, of }` — there is no central batch; the
  pause-point model becomes `PausedOn { core: CoreId, kind:
  Event|Action, variant: String }`.
- Dryrun against legacy `SystemState` — replaced by dryrun against a
  chosen core's machine (or deferred entirely; see open questions).

---

## Open questions

Listed so they get argued, not so I pre-decide them.

1. **What does Step mean in the multi-runtime world?**
   - *Option A:* Step releases every parked runtime exactly once.
     Simple and obvious; may release "uninteresting" runtimes (e.g.
     a Docker watcher tick) alongside the one the operator cares
     about.
   - *Option B:* Step releases only the runtime whose next event is
     "first" by some ordering (arrival time? topological causal
     order?). More useful per click, harder to define.
   - *Option C:* The maintainer view exposes per-core Step buttons
     in addition to the global one. Operator view only has global
     Step.
2. **Do we still want queue manipulation (edit / inject / reorder)?**
   It exists today; if it stays, it operates on per-runtime event
   channels, which means picking the target runtime per inject. If
   it goes, the operator loses a tool for "make the system pretend
   this event happened."
3. **Do we still want dryrun?** Same question — dryrun against a
   single core's machine is well-defined; dryrun "against the
   system" no longer is.
4. **Where does the operator-vs-maintainer toggle persist?** Local
   storage / URL param / not at all (always show maintainer view in
   dev builds)?
5. **Should `/debug` move behind a build flag in production?** §37
   listed this as a candidate. Argument for: most production
   operators are the same person as the maintainer (just you), so a
   single UI is fine. Argument against: surface area / build
   complexity.
6. **Replay.** §37 listed "revisit replay." The §36 architecture
   makes replay strictly harder (six concurrent runtimes, no single
   event order to replay). Proposal: drop replay from §37 scope and
   open a separate story if/when we want it. (No one is asking for
   replay today.)
7. **Where does the "unified timeline" actually get assembled?**
   Each runtime would need to publish its events / actions /
   publishes onto a single bus the debug controller subscribes to.
   Likely a `DebugTap` channel each `ServiceRuntime` writes to —
   cheap when no one is listening, populated when debug mode is on
   *or* a breakpoint is set on a relevant variant.

---

## Proposed next steps

Once this doc is reviewed and the open questions are answered, the
implementation falls out as a small number of follow-up stories. A
plausible first cut:

1. **§37a — Global pause across runtimes.** Add
   `await_step_if_paused()` to `ServiceRuntime`. Make `enable_debug`
   actually park every runtime. Rewrite the central-loop pause path
   to share the same flag. Drop `DebugDispatcher`.
2. **§37b — Unified DebugTap timeline.** Each runtime writes
   `{core, event|action|publish, payload, action_id?}` to a shared
   debug bus when debug mode is on or a relevant breakpoint is set.
   `DebugHistory` consumes the bus.
3. **§37c — Per-core state snapshots.** Replace the legacy
   `SystemState` slot in `DebugState` with a map of `core_id →
   serialized machine state`. Update the React UI to render one
   collapsible panel per core.
4. **§37d — Breakpoints route per-variant to owning runtime.**
   Variants → runtime registry; the controller's
   `should_pause_on_*` becomes per-runtime.
5. **§37e — Operator-vs-maintainer view toggle.** Frontend only;
   reorganize the existing `/debug` page into two presets.
6. **§37f — MAM rate-limit guardrail re-wiring.** Confirm the MAM
   runtime/shell still calls `enable_debug()` on the violation;
   move the trigger if §36 dropped it.
7. **§37g — Rewrite `docs/debug-mode.md`** as the canonical spec
   reflecting the post-§37 reality.

Each story is small. The discuss-first cost is paid in this doc; the
implementation stories should not need their own design rounds.
