# Windlass Debug Mode

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
debug mode only controls _when_ each step is allowed to proceed, not _what_ happens.
Every event the core would receive, it still receives. Every action the shell would
dispatch, it still dispatches. Every HTTP request a client would make, it still makes.
The user observes and gates execution; they do not alter it.

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
                   └─ on_http callback → broadcast HttpExchange  (no-op when debug mode off)
```

The main loop is paused at exactly one point at any time — either waiting to receive
the next event, or waiting to dispatch the current action. The single step semaphore
covers both: `POST /debug/step` releases one permit, advancing whatever is currently
blocked.

---

## HTTP Observation Detail

Each HTTP client (`QbitClient`, `MamClient`, `GotifyClient`) receives an
`on_http: HttpObserver` (i.e. `Arc<dyn Fn(Observation) + Send + Sync>`) callback at
construction. It is called unconditionally after every HTTP response. The callback
implementation in `windlass-debug` routes the observation to the SSE channel when debug
mode is active and is a no-op otherwise — zero overhead in normal operation.

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

| Method   | Path                                         | Description                                                          |
| -------- | -------------------------------------------- | -------------------------------------------------------------------- |
| `GET`    | `/api/v1/debug`                              | Debug state: mode, breakpoints, current pause point, pending actions |
| `POST`   | `/api/v1/debug/enable`                       | Enter debug mode                                                     |
| `POST`   | `/api/v1/debug/disable`                      | Exit debug mode; resume from current pause point                     |
| `GET`    | `/api/v1/debug/events`                       | All valid event variant names                                        |
| `GET`    | `/api/v1/debug/actions`                      | All valid action variant names                                       |
| `POST`   | `/api/v1/debug/breakpoints/event/{variant}`  | Set event breakpoint                                                 |
| `DELETE` | `/api/v1/debug/breakpoints/event/{variant}`  | Clear event breakpoint                                               |
| `POST`   | `/api/v1/debug/breakpoints/action/{variant}` | Set action breakpoint                                                |
| `DELETE` | `/api/v1/debug/breakpoints/action/{variant}` | Clear action breakpoint                                              |
| `POST`   | `/api/v1/debug/step`                         | Advance one pause point (next event or next action)                  |
| `POST`   | `/api/v1/debug/skip`                         | Discard the currently paused event or action                         |

---
