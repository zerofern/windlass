# Windlass Debug Mode

## Implementation Plan

Six sequential commits. Each leaves the system compiling, tested, and green.
Steps 1–4 are pure infrastructure with no behaviour changes (except the MAM halt
in step 1). Steps 5–6 add new capabilities.

---

### Step 1 — `DebuggableEventStream`

**Goal:** The shell's event loop has ~50 lines of inline queue/pause/step logic
tangled with MAM rate-limit special-casing. After this commit the main loop is a
clean 5-line while-let. The MAM guardrail no longer hard-freezes the system
forever — it enters debug mode instead, which is recoverable.

**What changes:**
- Add `DebuggableEventStream` to `windlass-debug` with an intake task
  (broadcasts `Observation::EventArrived` before forwarding to the internal
  channel) and a `recv()` method that owns all pause/step logic.
- `MamRateLimitViolation` in `recv()` calls `enable_debug()` then falls through
  — event still reaches the core unchanged.
- Shell `run()` replaces its 50-line event-loop body with a clean while-let over
  `debug_stream.recv()`.
- Remove `frozen`/`freeze()`/`unfreeze()`/`is_frozen()` from `DebugController`
  — `is_debug_mode()` covers the same gate.
- Remove `enqueue_event`/`dequeue_event` from `DebugController` — the stream
  owns its internal channel exclusively.
- `DEBUG_MODE_ON_START` check moves into the `DebuggableEventStream` constructor,
  resolving the awkward two-phase init in the shell.

**Files touched:** `windlass-debug/src/lib.rs`, `windlass/src/shell/mod.rs`,
`windlass-web/src/routes/debug.rs` (remove freeze endpoints).

---

### Step 2 — `DebuggableShell` + unified semaphore

**Goal:** The action dispatch side gets the same treatment as step 1. Two
separate semaphores become one. Skip works for both events and actions.

**What changes:**
- Add `ShellContext::execute(&mut self, action: Action)` that contains what the
  existing `dispatch` match currently inlines — no logic change, just extraction.
- Add `DebuggableShell` to `windlass-debug` wrapping a `ShellContext`. Its
  `dispatch(actions)` stores the full batch upfront in debug mode (so the UI can
  see all pending actions before any are dispatched), then pauses before each
  action if debug mode is on or the variant is breakpointed.
- Replace `step_event: Semaphore` + `step_action: Semaphore` in `DebugController`
  with a single `step: Semaphore`. The loop is sequential and only ever blocked
  at one point.
- Add `skip: AtomicBool` to `DebugController`. `POST /debug/skip` sets it;
  both `DebuggableEventStream::recv()` and `DebuggableShell::dispatch()` check
  and clear it after waking.
- Replace `/api/v1/debug/step/event` + `/api/v1/debug/step/action` with a
  single `POST /api/v1/debug/step` that releases one permit regardless of what
  is currently paused.
- Remove `enqueue_action`/`dequeue_action`/`acquire_action_step`/
  `release_action_step` from `DebugController`.

**Files touched:** `windlass-debug/src/lib.rs`, `windlass/src/shell/mod.rs`,
`windlass-web/src/routes/debug.rs`.

---

### Step 3 — Rich debug state (`paused_on`, `pending_actions`, enable/disable)

**Goal:** `GET /debug` currently returns the contents of queues. After this it
returns what the system is paused on *right now* and the full pending action
batch — the two things the UI actually needs. Adds enable/disable endpoints.

**What changes:**
- Add `paused_on: ArcSwap<Option<PausedOn>>` to `DebugController`.
  `DebuggableEventStream::recv()` and `DebuggableShell::dispatch()` store the
  current pause point before blocking and clear it on wake.
- Add `pending_actions: ArcSwap<Arc<Vec<Action>>>` to `DebugController`.
  `DebuggableShell::dispatch()` stores the full batch on entry and clears it
  when the batch is fully dispatched.
- Update `DebugState` / `GET /api/v1/debug` to return `paused_on` and
  `pending_actions` instead of the old queue snapshots.
- Add `POST /api/v1/debug/enable` and `POST /api/v1/debug/disable` so the UI
  can toggle debug mode at runtime without a restart.

**Files touched:** `windlass-debug/src/lib.rs`, `windlass-web/src/routes/debug.rs`.

---

### Step 4 — Lock-free `DebugController` + `ArcSwap` shared state + variant helpers

**Goal:** `DebugController` uses `Mutex` throughout, violating the no-mutex
rule. After this it is fully lock-free. Variant helpers move to `windlass-debug`.
`SystemState` sharing drops the `RwLock`.

**What changes:**
- Replace `Mutex<HashSet<String>>` for `event_breakpoints` and
  `action_breakpoints` with `ArcSwap<HashSet<String>>`.
- Replace `Mutex<Option<broadcast::Sender<Observation>>>` for `obs_tx` with
  `ArcSwap<Option<broadcast::Sender<Observation>>>`.
- Move `event_variant()`, `action_variant()`, `EVENT_VARIANTS`, and
  `ACTION_VARIANTS` from `windlass/src/shell/mod.rs` into `windlass-debug`.
  Add `GET /api/v1/debug/events` and `GET /api/v1/debug/actions` endpoints.
- Replace `Arc<RwLock<SystemState>>` in `windlass/src/shell/mod.rs` with
  `Arc<ArcSwap<SystemState>>`. The main loop stores a new `Arc` after each
  `process_event`; the SSE handler and `GET /state` load the current `Arc`
  with a single atomic operation — no lock contention.

**Files touched:** `windlass-debug/src/lib.rs`, `windlass/src/shell/mod.rs`,
`windlass-web/src/routes/operator.rs`, `windlass-web/src/routes/debug.rs`,
`windlass-web/src/app_state.rs`.

---

### Step 5 — Remove `RunMode::Fatal`, `hard_recoveries`, and `ManualReset`

**Goal:** The core has a permanent halting state (`RunMode::Fatal`) that requires a
process restart to escape, plus a recovery counter (`hard_recoveries`) and a
`ManualReset` event used to escape it. All three are removed. Death-loop prevention
is handled entirely by the existing MAM rate-limit guardrail: if the stack keeps
failing, MAM will be queried too frequently, `MamRateLimitViolation` will fire, and
`DebuggableEventStream` will enter debug mode. The operator resumes via the debug UI.

**What changes:**
- Remove `RunMode` enum and `run_mode` field from `SystemState`.
- Remove `hard_recoveries: RetryCount` from `SystemState`.
- Remove `Event::ManualReset`.
- `on_mam_not_connectable` hard-recovery path: remove the counter and limit check.
  Emit `FetchAndDumpAllLogs` + `SendGotifyAlert` unconditionally (single hard
  recovery attempt). The rate-limit guardrail is the only death-loop prevention.
- Remove `on_manual_reset` handler.
- Remove the `HARD_RECOVERY_LIMIT` constant.
- Remove `POST /api/v1/operator/reset` endpoint — no longer meaningful.
- Remove all unit and prop tests that assert on `Fatal`, `hard_recoveries`, or
  `ManualReset` behaviour.

**Files touched:** `windlass-core/src/types.rs`, `windlass-core/src/events.rs`,
`windlass-core/src/handlers/mam.rs`, `windlass-core/src/handlers/vpn.rs`,
`windlass-core/src/lib.rs`, `windlass-core/src/tests/mam.rs`,
`windlass-core/src/tests/init.rs`, `windlass-core/src/prop_tests.rs`,
`windlass-web/src/routes/operator.rs`, `windlass-web/src/routes/debug.rs`,
`windlass-debug/src/stream.rs`, `windlass/src/shell/mod.rs`,
`windlass/tests/integration.rs`.

---

### Step 6 — HTTP Observation Callback

**Goal:** In debug mode, every outbound HTTP call emits a full
request/response observation on the SSE stream. Clients stay unaware of debug
mode — they receive an optional callback at construction and call it if present.

**What changes:**
- Add `on_http: Option<Arc<dyn Fn(Observation) + Send + Sync>>` field to
  `QbitClient`, `MamClient`, and `GotifyClient`. Each client calls it (if
  `Some`) after every HTTP response, passing an `Observation::HttpExchange`
  with module name, method, URL, optional request body, response status, and
  response body.
- Add `Observation::HttpExchange { module, method, url, request_body,
  status, response_body }` variant to `windlass-core/src/observation.rs`.
- In `windlass/src/shell/mod.rs`, construct the closure from `obs_tx` and pass
  it to each client constructor. When debug mode is off, pass `None`.
  `windlass-debug` provides a helper to build the closure from an `obs_tx`.
- Remove `obs_sender()` from `DebugController` — clients no longer poll for it.

**Files touched:** `windlass-clients/src/qbit.rs`, `windlass-clients/src/mam.rs`,
`windlass-clients/src/gotify.rs`, `windlass-core/src/observation.rs`,
`windlass-debug/src/lib.rs`, `windlass/src/shell/mod.rs`.

---

## Purpose

Debug mode gives the operator a **debugger-like experience** over the Windlass event loop
in both development and production.

**In development:** Step through edge cases and specific scenarios without the system
racing ahead. Test how the core responds to unusual event sequences. Verify that MAM and
qBittorrent interactions look exactly right before committing a change.

**In production:** Deploy and operate with confidence. Some of the external services
Windlass talks to (especially MAM) are rate-sensitive and must never be spammed. Debug mode
provides controlled execution and full visibility before anything is sent to an external
service.

---

## Core Principle

Debug mode is **transparent**. It does not modify events, actions, state, or HTTP
requests in any way. The system executes exactly as it would in normal operation —
debug mode only controls *when* each step is allowed to proceed, not *what* happens.
Every event the core would receive, it still receives. Every action the shell would
dispatch, it still dispatches. Every HTTP request a client would make, it still makes.
The user observes and gates execution; they do not alter it.

---

## The Unified Model

There is exactly **one debug mode**. It is either on or off. When it is on, the system
is paused: no events are processed, no actions are dispatched, and no outbound HTTP
requests are made until the user explicitly advances execution.

The historical `frozen` emergency-halt was a temporary workaround built before debug mode
existed. It is now replaced entirely by debug mode — the MAM guardrail (see Entry Point 3)
enters debug mode rather than hard-dropping all events.

---

## Three Ways to Enter Debug Mode

### 1. Environment Variable — Pause from the Very Start

```
DEBUG_MODE_ON_START=true
```

Set this when you need to inspect the system before it does anything at all. The event loop
starts in debug mode before `Event::Init` is processed. Nothing moves — no port file read,
no Docker inspection, no `QbitClient` authentication, no HTTP requests — until the user
opens the web UI and steps through.

**Use cases:** Testing a cold-start scenario with full visibility. Verifying the initial
action sequence before any external service is contacted. Running in a new environment
without accidentally hitting MAM before the VPN IP is confirmed.

### 2. Web UI Toggle — Pause a Running System in Place

The `/debug` page has an **Enable Debug Mode** button. Clicking it pauses the system at
the next event boundary: the currently-in-flight event (if any) completes normally, and
the following event is the first one to be queued for manual stepping.

The system stays in debug mode until the user explicitly disables it. On disable, any
queued events and pending actions are executed in order and the system resumes normal
operation. The user leaves debug mode knowing the full current batch has been dispatched —
there are no silently dropped items.

**Use cases:** Investigating why Windlass is behaving unexpectedly in production without
a restart. Pausing before a sensitive operation (e.g. a MAM update) to inspect state
first. Testing a specific in-flight scenario without spinning up a dev environment.

### 3. MAM Rate-Limit Guardrail — Automatic Emergency Pause

If the MAM client detects that two requests were issued within the minimum allowed
interval, the system **automatically enters debug mode**.

This should never happen in normal operation. It exists as a circuit-breaker: if a bug
causes the system to hammer MAM, it catches itself before doing damage. The system
pauses exactly as if the user had clicked **Enable Debug Mode** — all queued events are
visible, nothing further is dispatched, and the user can inspect exactly what triggered
the rapid requests before deciding whether to step forward or restart.

---

## What the User Sees and Can Do

The debug experience is entirely browser-based at the `/debug` route.

### Visibility

While debug mode is active the user has full visibility into:

- **All pending events** — every event that has arrived in the system, in order, whether
  or not the loop has reached them yet. Events appear in real-time as they arrive from
  monitors, timers, and Docker watchers — even while the loop is paused mid-step. The
  client maintains this list from the SSE stream (`EventArrived` observations).
- **Current pause point** — which event or action the loop is currently paused on,
  with its full JSON payload. The user always knows exactly what will execute next.
- **Pending action batch** — all actions produced by the last `process_event` call,
  displayed as formatted JSON. The full batch is visible before any action is dispatched.
- **System state snapshot** — the current `SystemState` as formatted JSON, updated
  after each event is processed.
- **HTTP request/response detail** — full request and response bodies for every
  outbound call made by `QbitClient`, `MamClient`, and `GotifyClient`. Emitted as
  `Observation::HttpExchange` on the SSE stream only while debug mode is active.
- **Active breakpoints** — which event and action variants are currently breakpointed.

### Controls

- **Step** — advance the system one pause point. If an event is queued, it is processed
  through the core and the resulting actions become visible. If an action is pending, it
  is dispatched through the shell. The UI determines which by reading `GET /api/v1/debug`
  — the user never needs to distinguish between "step event" and "step action."
- **Skip** — discard the currently paused event or action without executing it.
- **Step All** — release all pending actions in sequence without individual clicks.
  Useful once you have inspected the current batch and are confident it is safe.
- **Disable Debug Mode** — execute all remaining queued events and pending actions in
  order, then resume normal operation. No items are silently discarded.

### Breakpoints — Jump to a Specific Point

Breakpoints work independently of full debug mode. You name a specific event or action
variant (e.g. `QbitAuthFailed`, `UpdateMam`) and the system runs at full speed —
processing events, dispatching actions, making HTTP requests — until that exact variant
arrives. The system then pauses right before executing it, exactly as if debug mode had
been enabled at that moment.

This is a "jump to" mechanism: you skip over everything you don't care about and land
precisely at the point you want to inspect. It is the right tool when you know which
event or action you want to observe but don't want to slow down normal operation to get
there.

Breakpoints survive the debug mode toggle — they remain set until explicitly cleared.

---

## Execution Flow

Two concurrent tasks are always running when Windlass is up:

**Intake task** — continuously drains the mpsc channel, broadcasting each event as it
arrives. Runs independently of whether the main loop is paused.

**Main loop** — pops events from the intake's internal channel, processes them, and
dispatches the resulting actions. Blocked by the step semaphore when paused.

```
External monitors / timers / Docker watcher
  │  (mpsc::Sender<Event>, cap 128)
  ▼
Intake task
  ├─ broadcasts Observation::EventArrived(event)  → SSE → client adds to visible list
  └─ forwards event to internal channel
        │
        ▼
DebuggableEventStream.recv()
  ├─ MamRateLimitViolation? → enable_debug() → pause (awaits step semaphore) → return event
  │
  ├─ debug mode on, or variant breakpointed?
  │   ├─ YES → store as currently_paused_on → await step semaphore
  │   │          ├─ skip flag set? → clear flag, broadcast EventSkipped → loop
  │   │          └─ otherwise → return event
  │   └─ NO  → return event
  │
Main loop
  ├─ broadcast Observation::EventReceived(event)
  ├─ state.process_event(event) → actions        [pure, no I/O]
  ├─ shared_state.store(Arc::new(state))
  ├─ broadcast Observation::StateSnapshot(state)
  ├─ store pending_actions snapshot for GET /debug
  │
  └─ DebuggableShell.dispatch(actions)
       ├─ (enqueues full action batch upfront for visibility)
       └─ for each action:
            ├─ debug mode on, or variant breakpointed?
            │   ├─ YES → store as currently_paused_on → await step semaphore
            │   │          ├─ skip flag set? → clear flag, broadcast ActionSkipped → next
            │   │          └─ otherwise → ShellContext.execute(action)
            │   └─ NO  → ShellContext.execute(action)
            │
            ShellContext.execute(action)
              └─ may make HTTP requests
                   └─ on_http callback → broadcast HttpExchange  [debug mode only]
```

The main loop is paused at exactly one point at any time — either waiting to receive
the next event, or waiting to dispatch the current action. The single step semaphore
covers both: `POST /debug/step` releases one permit, advancing whatever is currently
blocked.

---

## HTTP Observation Detail

Each HTTP client (`QbitClient`, `MamClient`, `GotifyClient`) receives an
`on_http: Option<Arc<dyn Fn(Observation) + Send + Sync>>` callback at construction.
When `Some`, it is called after every HTTP response. When `None`, the call is skipped —
zero overhead in normal operation.

The callback broadcasts an `Observation::HttpExchange` containing module name, method,
URL, optional request body, response status, and full response body. These appear in the
SSE stream, giving full traceability from action → HTTP call → resulting event.

---

## Disabling Debug Mode — Flush and Resume

When debug mode is disabled, queued events and pending actions are not discarded. The
mechanism:

1. `debug_mode` flag set to `false`.
2. `obs_tx` swapped to `None` via `ArcSwap` — clients stop emitting `HttpExchange`.
3. The step semaphore is released — the main loop wakes and continues executing
   whatever it was paused on, then proceeds at full speed.

Because the event inbox lives in the intake task's internal channel (not in
`DebugController`), there is nothing to drain or clear — the loop just resumes
processing events naturally.

---

## API Reference

| Method   | Path                                         | Description                                              |
|----------|----------------------------------------------|----------------------------------------------------------|
| `GET`    | `/api/v1/debug`                              | Debug state: mode, breakpoints, current pause point, pending actions |
| `POST`   | `/api/v1/debug/enable`                       | Enter debug mode                                         |
| `POST`   | `/api/v1/debug/disable`                      | Exit debug mode; resume from current pause point         |
| `GET`    | `/api/v1/debug/events`                       | All valid event variant names                            |
| `GET`    | `/api/v1/debug/actions`                      | All valid action variant names                           |
| `POST`   | `/api/v1/debug/breakpoints/event/{variant}`  | Set event breakpoint                                     |
| `DELETE` | `/api/v1/debug/breakpoints/event/{variant}`  | Clear event breakpoint                                   |
| `POST`   | `/api/v1/debug/breakpoints/action/{variant}` | Set action breakpoint                                    |
| `DELETE` | `/api/v1/debug/breakpoints/action/{variant}` | Clear action breakpoint                                  |
| `POST`   | `/api/v1/debug/step`                         | Advance one pause point (next event or next action)      |
| `POST`   | `/api/v1/debug/skip`                         | Discard the currently paused event or action             |

---

## Implementation Notes

### Separation of Concerns — Interceptor Pattern

The shell, core, and HTTP clients must not know that debug mode exists. All debug
behaviour is isolated in `windlass-debug` and wired in at the binary level.

This is achieved with two thin wrappers that intercept the normal data flow:

**`DebuggableEventStream`** consists of two parts:

An **intake task** (spawned once at startup) that continuously drains the external mpsc
channel regardless of whether the main loop is paused:

```rust
// Spawned task — runs independently of the main loop
while let Some(event) = external_rx.recv().await {
    let _ = obs_tx.send(Observation::EventArrived(event.clone())); // UI sees it immediately
    let _ = internal_tx.send(event).await;                          // forward to main loop
}
```

A **`recv()` method** that pops from the intake's internal channel, pausing when needed:

```rust
pub async fn recv(&mut self) -> Option<Event> {
    loop {
        let event = self.internal_rx.recv().await?;

        if matches!(event, Event::MamRateLimitViolation) {
            self.debug_ctrl.enable_debug(self.obs_tx.clone());
            // fall through — event still reaches the core unchanged
        }

        if self.debug_ctrl.should_pause_on_event(event_variant(&event)) {
            self.debug_ctrl.paused_on.store(Arc::new(Some(PausedOn::Event(event.clone()))));
            self.debug_ctrl.step.acquire().await.forget();
            self.debug_ctrl.paused_on.store(Arc::new(None));

            if self.debug_ctrl.skip.swap(false, Ordering::SeqCst) {
                let _ = self.obs_tx.send(Observation::EventSkipped(event));
                continue; // fetch next
            }
        }

        let _ = self.obs_tx.send(Observation::EventReceived(event.clone()));
        return Some(event);
    }
}
```

The main loop becomes clean:

```rust
while let Some(event) = debug_stream.recv().await {
    let actions = state.process_event(event);
    shared_state.store(Arc::new(state.clone()));
    let _ = obs_tx.send(Observation::StateSnapshot(state.clone()));
    debug_shell.dispatch(actions).await;
}
```

**`DebuggableShell`** wraps `ShellContext`. When debug mode is active, the full action
batch is stored upfront so the UI can see all pending actions before any are dispatched:

```rust
impl<'a> DebuggableShell<'a> {
    pub async fn dispatch(&mut self, actions: Vec<Action>) {
        if self.debug_ctrl.is_debug_mode() {
            // Store full batch upfront — UI sees all actions immediately.
            self.debug_ctrl.pending_actions.store(Arc::new(Arc::new(actions.clone())));
        }

        for action in actions {
            if self.debug_ctrl.should_pause_on_action(action_variant(&action)) {
                self.debug_ctrl.paused_on.store(Arc::new(Some(PausedOn::Action(action.clone()))));
                self.debug_ctrl.step.acquire().await.forget();
                self.debug_ctrl.paused_on.store(Arc::new(None));

                if self.debug_ctrl.skip.swap(false, Ordering::SeqCst) {
                    let _ = self.obs_tx.send(Observation::ActionSkipped(action));
                    continue; // skip this action
                }
            }

            let _ = self.obs_tx.send(Observation::ActionDispatched(action.clone()));
            self.inner.execute(action).await;
        }

        // Batch fully dispatched — clear the snapshot.
        self.debug_ctrl.pending_actions.store(Arc::new(Arc::new(vec![])));
    }
}
```

`ShellContext` gains a single `execute(&mut self, action: Action)` method that the
existing `dispatch` loop currently inlines. No other changes to `ShellContext`.

The `obs_tx` broadcast channel is **not** a debug concept — it is the general SSE bus
used by `GET /api/v1/stream` in normal operation. Both wrappers use it because they are
the points where events and actions become observable. `StateSnapshot` is emitted by
the main loop for the same reason.

### `DebugController` (`windlass-debug`)

`DebugController` is an `Arc<Inner>` clone shared by `DebuggableEventStream`,
`DebuggableShell`, and the axum web handlers via `AppState`. HTTP clients no longer
hold it (see HTTP Observation Callback below).

The event and action queues are **not** in `DebugController`. They are owned exclusively
by their respective tasks (the intake task and the main loop). `DebugController` holds
only signals, snapshots, and configuration — things that multiple owners need to read
or write concurrently. Everything uses lock-free primitives.

| Field                | Type                                               | Role                                                        |
|----------------------|----------------------------------------------------|-------------------------------------------------------------|
| `debug_mode`         | `AtomicBool`                                       | On/off flag                                                 |
| `step`               | `Semaphore` (starts at 0)                          | Single semaphore — one permit advances one pause point      |
| `skip`               | `AtomicBool`                                       | Set by `POST /debug/skip`; checked after step wakes         |
| `paused_on`          | `ArcSwap<Option<PausedOn>>`                        | Current pause point (event or action JSON); read by UI      |
| `pending_actions`    | `ArcSwap<Arc<Vec<Action>>>`                        | Full action batch from last `process_event`; read by UI     |
| `event_breakpoints`  | `ArcSwap<HashSet<String>>`                         | Variant names that pause regardless of `debug_mode`         |
| `action_breakpoints` | `ArcSwap<HashSet<String>>`                         | Same for actions                                            |
| `obs_tx`             | `ArcSwap<Option<broadcast::Sender<Observation>>>` | Present only while debug mode is active                     |

`ArcSwap` gives wait-free reads on the hot path and copy-on-write semantics for rare
writes. The single `step` semaphore replaces the two separate event/action semaphores —
the main loop is sequential and only ever blocked at one point at a time.

**`GET /api/v1/debug`** serves `{ debug_mode, breakpoints, paused_on, pending_actions }`.
It does **not** serve the event inbox — that is maintained client-side from the SSE
stream. On connect, the client receives a `StateSnapshot` and the stream of
`EventArrived`, `EventProcessed`, and `EventSkipped` observations, from which it
reconstructs the full pending event list.

### Variant Name Coupling

`event_variant()` and `action_variant()` are `const fn`s that return a static string
for a given event or action value. They live in `windlass-debug` (not in `shell/`) —
the shell has no business caring about stringified variant names; that is strictly a
debug and UI concern.

The static `EVENT_VARIANTS` and `ACTION_VARIANTS` slices (served by `GET /debug/events`
and `/debug/actions`) also live in `windlass-debug` alongside the functions.

When adding a new event or action variant, two places must be kept in sync:
1. The `event_variant()` / `action_variant()` match arms in `windlass-debug`
2. The static variant slices in `windlass-debug`
3. The enum definitions in `core/events.rs` / `core/actions.rs`

There is currently no compile-time enforcement of this.

### HTTP Observation Callback

HTTP clients (`QbitClient`, `MamClient`, `GotifyClient`) must not import
`windlass-debug`. They receive an observation callback at construction time:

```rust
on_http: Option<Arc<dyn Fn(Observation) + Send + Sync>>
```

When `Some`, the client calls it after every HTTP response. When `None`, the `if let`
arm is skipped — zero overhead in normal operation.

`windlass-debug` is responsible for constructing the closure:

```rust
let obs_tx = obs_tx.clone();
let on_http = Arc::new(move |obs| { let _ = obs_tx.send(obs); });
```

The closure is passed into each client constructor. The clients remain completely
unaware of how observations are routed or stored.

### MAM Guardrail

`MamClient` no longer calls any debug function. Its internal rate-limit check returns
`false` when triggered; the method returns early and the shell task sends
`Event::MamRateLimitViolation` onto the event channel as it does today.

`DebuggableEventStream` intercepts `MamRateLimitViolation` before it reaches the main
loop. It calls `enable_debug()` and queues the event — **but does not discard it**.
When the user steps, the event flows to the core exactly as any other event would.

This is consistent with the core principle: debug mode controls the rate of execution,
not what gets executed. The interceptor entering debug mode is the guardrail mechanism;
it does not change what the core receives.

The core's `MamRateLimitViolation` match arm is updated from its current "shouldn't
reach here" warning to a proper no-op handler — log the event, produce no actions. The
core is pure and has no concept of debug mode; it simply acknowledges the event and
moves on.

### Core Changes

Several changes to `windlass-core` are motivated by this refactor:

**`process_event` signature.** The method mutates state in place and returns only
actions, eliminating the tuple and the re-assignment boilerplate at every call site:

```rust
// Before
pub fn process_event(mut self, event: Event) -> (Self, Vec<Action>)

// After
pub fn process_event(&mut self, event: Event) -> Vec<Action>
```

**`RunMode::Fatal` removed.** `Fatal` is an irrecoverable halting state that requires
a restart. Debug mode is a better replacement: when the hard recovery limit is reached,
the system enters debug mode instead of `Fatal`. The user sees the system state, can
inspect what triggered the limit, and decides whether to reset or restart. The
`ManualReset` escape hatch that existed for `Fatal` is preserved — it now disables
debug mode and resets `hard_recoveries`.

**`process_event` refactored into handler methods.** The large `match` in `lib.rs` is
split into focused private methods on `SystemState` (one per event group: vpn, qbit,
mam, monitoring, wakeups). This removes the `#[allow(clippy::too_many_lines)]`
suppression. If the handlers file exceeds 300 lines it is split by area.

**Shared state uses `ArcSwap`.** `Arc<RwLock<SystemState>>` is replaced with
`Arc<ArcSwap<SystemState>>`. The main loop stores a new `Arc` after each
`process_event` call; readers (SSE handler, `GET /state`) load the current `Arc`
with a single atomic operation — no lock contention.

### `DEBUG_MODE_ON_START` — Two-Phase Init

`DebuggableEventStream` is constructed after the broadcast channel exists, so the
two-phase init problem is resolved naturally. The env var check moves inside the
`DebuggableEventStream` constructor:

```rust
let (obs_tx, _) = broadcast::channel(256);
let debug_stream = DebuggableEventStream::new(rx, debug_ctrl.clone(), obs_tx.clone());
// debug_ctrl is already in debug mode if DEBUG_MODE_ON_START=true
```

`Event::Init` is the first event in the channel. Because `DebuggableEventStream`
intercepts before yielding to the main loop, Init is subject to stepping when debug
mode is active at startup.
