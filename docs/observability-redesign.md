# Observability Redesign — §37 Discussion Doc

This is the discussion artifact for operator-readiness §37 (originally
"Review and Consolidate Debug-Mode Workflow"). It audits the
debug-mode surface as it exists after the §36 per-system core
migration, names what is stale or misleading now, and specifies the
redesign at a level of detail sufficient for implementation stories
to be cut without further design rounds.

The framing shifts from **debug mode** (a modal toggle wrapped around
a single event loop) to **observability** (an always-on view of the
running system, with optional per-core gates for stepping). "Debug"
disappears as a name throughout. The canonical operator-facing spec
`docs/debug-mode.md` describes a system that no longer exists; it
retires and is replaced by `docs/observability.md` as part of this
work (§37j).

---

## Anchoring decisions (agreed)

Each item is one line — engineering detail lives in "Architecture"
and "Engineering contracts" below.

1. **Naming**: crate `windlass-observability`, route `/observability`,
   traits `RuntimeTap` + `HttpTap`, `ObservabilityController`.
2. **Audience**: one combined surface. No operator/maintainer toggle.
3. **Pause model**: per-core is the primitive; global is "do it to
   every core."
4. **Gate granularity (v1)**: three gates per core —
   **event-gate** (before `handle`), **outcome-gate** (after
   `handle`, before `apply`), **HTTP-gate** (before `execute`).
   Per-action gate (between dispatches inside one outcome) deferred.
5. **Observation always on**: events, actions, publishes, state
   snapshots, HTTP exchanges, log lines populate bounded rings
   unconditionally. No hot-path on/off flag.
6. **Timeline primitive**: per-core `StoredStepRecord`. One event +
   the actions and publishes it produced + the state snapshot at the
   end + duration + threaded IDs.
7. **Bidirectional causal graph by ID**: every action and publish
   carries a Uuid (assigned by the runtime, *not* by `Machine`);
   every event carries `cause: EventCause`. Backward lookups
   (`action_id`/`publish_id` → parent step) are O(1) via indices.
8. **State delta is a UI concern**: runtime ships full `state_after`
   in every record; the page diffs against the previous record.
9. **`Machine` gains `type StateSnapshot: Serialize`** and
   `fn state_snapshot(&self) -> Self::StateSnapshot` — owned
   snapshot, smaller than internal state if a machine chooses.
10. **`Shell` trait untouched.** HTTP capture lives at client
    construction via `HttpTap`.
11. **`PAUSE_ON_START` env var** accepts both `true` (all cores) and
    a comma-separated list (`mam,qbit`).
12. **Queue manipulation (edit / inject / reorder / delete) dropped.**
13. **Single-page frontend, fully redesigned.** No view toggle, no
    merged cross-core stream.
14. **Secrets policy (B2)**: `secrecy::Secret<T>` everywhere in code;
    **redacted by default on the wire**; UI has a per-field
    **Reveal** button; revealed values are session-only and never
    persisted. The default `/observability` page is safe to
    screenshot. Auth on the route is a separate later story.
15. **Separation is a hard goal.** A bug in `windlass-observability`
    cannot corrupt machine state, drop or reorder events, or block
    action/publish dispatch — its blast radius is bounded to "hang
    a runtime at a gate" or "lose timeline visibility."
16. **Seven cores, not six**: VPN, qBit, MAM, DB, disk, Docker,
    Domain — Domain runs on the service runtime (per
    operator-readiness §8).
17. **Wire/storage uses owned records and `DateTime<Utc>`**, not
    borrowed views or `Instant`.
18. **Byte budgets, not just record counts.** Bodies capped with
    `truncated: true, original_len: N`. Truncation and drop counters
    surfaced in the UI.
19. **Ring sizes and body caps are config-driven** from day one with
    sensible defaults (see "Configuration").
20. **Operator-readiness §37 stays as the umbrella story.** Substories
    §37pre + §37a–j live underneath, each small.
21. **Process: discuss-first.** This document is the artifact; §37pre
    consolidates the engineering contracts before any other story
    starts.

---

## Original purpose (still valid)

Step through edge cases in development without the system racing
ahead; operate with confidence in production over rate-sensitive
external services (especially MAM); transparent — never modify
events, actions, state, or HTTP, only control *when* each step
proceeds; three entry points (`PAUSE_ON_START` env var, per-core
UI controls, MAM rate-limit guardrail).

---

## What changed in §36

Before §36, Windlass had one event loop and one `SystemState`. After
§36 (closed 2026-06-01), seven per-system cores — VPN, qBit, MAM,
DB, disk, Docker, Domain — each run on their own `ServiceRuntime`.
The central legacy shell loop is now:

```
recv legacy Event → debug!(?event) → service_cores.observe(&event)
```

Everything interesting happens inside the per-system runtimes that
`observe` fans out to. There is no central action batch and no
central state. The legacy `SystemState` exists only as a bridge
protocol; its view in the dashboard is frozen at `initial()`.

---

## Audit: the surface today

### Backend — `windlass-debug` (~2050 LOC)

| Module                  | Status post-§36                                                                                |
| ----------------------- | ---------------------------------------------------------------------------------------------- |
| `DebugController`       | Mostly alive. `enable/disable`, `step`, `skip`, `paused_on`, breakpoints, snapshot — all work. |
| `DebuggableEventStream` | Alive. Pauses at the central legacy-event intake.                                              |
| `DebugHistory`          | Half-alive. Event queue + log capture work; per-event before/after `SystemState` is meaningless. |
| `DebugDispatcher`       | **Dead.** No central action dispatcher.                                                        |
| `CausalTx`              | Alive. Task-local action id for HTTP-exchange threading.                                       |
| `make_http_observer`    | Alive. Per-action HTTP exchange callback.                                                      |
| `DebugLogLayer`         | Alive. Tracing layer captures log lines.                                                       |
| `DebugState`            | Half-alive. Shape serializes but `latest_state`, `trace[].state_before/after`, and `running_actions` reflect a model that no longer matches reality. |

### Frontend — `/debug` route (769 LOC)

To be replaced wholesale. About 40% of what it shows is now lying
(state diff against frozen legacy `SystemState`, action timeline for
actions that never run through it, dead action breakpoints, dryrun
against legacy state, queue manipulation whose mental model is
broken because actions happen inside per-core runtimes).

### Per-core runtimes

**No observability integration at all today.** They process events
at full speed regardless of any flag. This is the single biggest gap.

---

## Architecture

Two traits, one new field on `ServiceRuntime<M, S>`, two new methods
on `Machine`, one extension to `Timed<E>`, and two envelope types.
No `Shell` change. No `Runtime` trait — the `ServiceRuntime` is one
generic struct instantiated seven times.

### `Machine` trait extension

```rust
pub trait Machine: Sized {
    // ... existing items ...
    type StateSnapshot: Serialize + Send + 'static;
    fn state_snapshot(&self) -> Self::StateSnapshot;
}
```

Owned snapshot, not a borrow. A machine may expose a smaller
projection than its full internal state — e.g. omit caches, large
buffers, or fields that hold secrets. No `Clone` on the state.

### `Timed<E>` causal extension

```rust
pub enum EventCause {
    Action(Uuid),
    Publish(Uuid),
    External(ExternalCause),
}

pub enum ExternalCause {
    Timer { name: &'static str },
    FileWatcher { path: PathBuf },
    DockerEvent { kind: &'static str },
    ManualCommand,
    Init,
    Unknown,
}

pub struct Timed<E> {
    pub at: Instant,           // monotonic, internal only
    pub cause: EventCause,
    pub inner: E,
}
```

Constructors at call sites: `Timed::from_action(now, action_id, e)`,
`Timed::from_publish(now, publish_id, e)`,
`Timed::external(now, cause, e)`.

### Envelopes — IDs assigned by the runtime, after `handle`

`Machine::handle` stays pure. The runtime envelopes after it
returns:

```rust
pub struct ActionEnvelope<A>  { pub id: Uuid, pub payload: A }
pub struct PublishEnvelope<P> { pub id: Uuid, pub payload: P }
```

`Outcome<A, P>` continues to hold raw `Vec<A>` / `Vec<P>` —
property tests stay unchanged.

### Runtime loop diff

```rust
let timed = event_rx.recv().await?;
self.tap.gate_event(self.core_id, &EventGateView {
    variant: timed.variant_name(),
    cause: timed.cause,
    event: &timed.inner,
}).await;

let t0 = Instant::now();
let outcome = self.machine.handle(t0, timed);
let duration = t0.elapsed();

let actions = envelope_each(outcome.actions);     // Uuid::new_v4 per item
let publishes = envelope_each(outcome.publish);
self.tap.gate_outcome(self.core_id, &OutcomeGateView {
    source_event_variant: timed.variant_name(),
    actions: &actions, publishes: &publishes,
}).await;

self.apply(&actions, &publishes);                 // threads IDs into shell + fanout

let snapshot = self.machine.state_snapshot();
self.tap.observed_step(self.core_id, &StepRecordView {
    step_id: Uuid::new_v4(),
    at: Utc::now(), duration,
    kind: StepKind::Event,
    event_variant: timed.variant_name(),
    event: &timed.inner, event_cause: timed.cause,
    state_after: &snapshot,
    actions: &actions, publishes: &publishes,
});
```

Order matters: **apply happens before observed_step.** A panic, slow
serialization, or full ring inside `observed_step` cannot prevent the
outcome from being applied. The gates (`gate_event`, `gate_outcome`,
`gate_request`) are the explicit "stop before this happens" points;
`observed_step` is post-hoc record only.

### `RuntimeTap` (runtime-side)

```rust
#[async_trait]
pub trait RuntimeTap: Send + Sync {
    /// Park until the controller releases us. Returns immediately
    /// when this core's pause flag is not set and no event-variant
    /// breakpoint matches.
    async fn gate_event(&self, core: CoreId, view: &EventGateView<'_>);

    /// Park between handle and apply, with the full outcome visible.
    /// Returns immediately when no outcome-variant breakpoint matches.
    async fn gate_outcome(&self, core: CoreId, view: &OutcomeGateView<'_>);

    /// Fire-and-forget. Must not block, must not panic, must drop
    /// (not backpressure) when overloaded. See "Engineering contracts."
    fn observed_step(&self, core: CoreId, view: &StepRecordView<'_>);
}
```

### `HttpTap` (client-side)

```rust
#[async_trait]
pub trait HttpTap: Send + Sync {
    async fn gate_request(&self, core: CoreId, view: &HttpRequestView<'_>);
    fn observed_exchange(&self, core: CoreId, view: &HttpExchangeView<'_>);
}
```

Clients build the `HttpRequestView` from typed inputs *before*
`reqwest::Request::build()`:

```rust
let body = serde_json::to_value(&payload)?;
let view = HttpRequestView::json("POST", &url, &headers, &body);
self.hook.gate_request(self.core, &view).await;
let req = self.client.post(&url).headers(headers).json(&payload).build()?;
let res = self.client.execute(req).await?;
self.hook.observed_exchange(self.core, &HttpExchangeView { /* … */ });
```

Reverse-engineering a built `reqwest::Request` is unreliable for
streaming, multipart, or compressed bodies.

### Borrowed views vs owned stored records

The trait inputs are borrowed views (zero copy from the runtime).
The controller serializes immediately into owned records for the
ring and the SSE stream:

```rust
pub struct StoredStepRecord {
    pub step_id: Uuid,
    pub core: CoreId,
    pub at: DateTime<Utc>,
    pub duration_ms: u64,
    pub kind: StepKind,
    pub event_variant: String,
    pub event_cause: StoredEventCause,
    pub event: serde_json::Value,
    pub state_after: serde_json::Value,
    pub actions: Vec<StoredAction>,
    pub publishes: Vec<StoredPublish>,
}

pub struct StoredAction {
    pub action_id: Uuid,
    pub variant: String,
    pub payload: serde_json::Value,
}

pub struct StoredPublish {
    pub publish_id: Uuid,
    pub topic: String,
    pub variant: String,
    pub payload: serde_json::Value,
}

pub struct StoredHttpExchange {
    pub exchange_id: Uuid,
    pub action_id: Option<Uuid>,
    pub core: CoreId,
    pub at: DateTime<Utc>,
    pub method: String,
    pub url: String,
    pub request_headers: Vec<(String, RedactedString)>,
    pub request_body: BodyCapture,
    pub response_status: u16,
    pub response_headers: Vec<(String, RedactedString)>,
    pub response_body: BodyCapture,
    pub duration_ms: u64,
}

pub enum BodyCapture {
    Inline(serde_json::Value),
    Text(String),
    Bytes(usize),  // size only
    Truncated { kind: BodyKind, captured: serde_json::Value, original_len: usize },
    None,
}
```

`RedactedString` is the wire form: by default serializes to
`"[REDACTED]"` with an opaque `reveal_id`; the UI uses `reveal_id`
to request cleartext via a dedicated endpoint (see "Secrets").

### `CoreStatus` — explicit pause-state machine

```rust
pub enum CoreStatus {
    Running,
    PauseRequested,                                                   // flag set, no event yet
    ParkedAtEvent { variant: String, cause: StoredEventCause, since: DateTime<Utc>, preview: serde_json::Value },
    ParkedAtOutcome { source_variant: String, since: DateTime<Utc>, action_variants: Vec<String>, publish_variants: Vec<String> },
    ParkedAtHttp { method: String, url: String, since: DateTime<Utc>, request_preview: serde_json::Value },
    Stepping,                                                         // permit granted, not yet consumed
}
```

The controller maintains one `CoreStatus` per core and broadcasts
it over SSE. The UI renders the cores rail from this directly. The
edge cases the reviewer flagged map cleanly onto the state machine:

- **Pause MAM while idle** → `PauseRequested`. Next event lands in
  `ParkedAtEvent`.
- **Step MAM while idle** → `Stepping`. One future permit is held;
  the next gate consumes it and runs through.
- **Pause MAM while inside `handle`** → `handle` completes (it is
  sync after `gate_event` returned). The outcome gate observes the
  pause flag next and parks at `ParkedAtOutcome`.
- **Pause MAM while an action is dispatching** → action dispatch is
  not interruptible in v1 (per-action gate deferred). The next
  iteration parks.
- **"Step All" while MAM is `ParkedAtHttp` and qBit is
  `ParkedAtEvent`** → releases one permit per paused core,
  regardless of which gate each core is parked at.

### Controller (`ObservabilityController`)

Per-core: `paused: AtomicBool`, `step_permits: Semaphore`, current
`CoreStatus` (ArcSwap).

Storage:
- Seven per-core `StoredStepRecord` rings (count + byte budgets,
  whichever fills first).
- One cross-core `StoredHttpExchange` ring.
- One cross-core log ring (existing).

Indices:
- `action_id → (CoreId, step_id)`.
- `publish_id → (CoreId, step_id)`.
- `reveal_id → CleartextSlot` (for the secrets endpoint).

Eviction: ring drops oldest; corresponding index entries are removed
in the same write. Counters: `dropped_steps_total`,
`dropped_http_total`, `truncated_bodies_total`, per-core where
applicable, surfaced in the UI.

SSE: broadcasts new records and `CoreStatus` changes. Drops sends
on slow clients; never backpressures the runtime.

### Secrets (Decision 14 detail)

- `secrecy::SecretString` wraps MAM cookie, qBit password, etc. in
  config types. Default `Debug`/`Display`/`Serialize` impls keep
  them out of logs and ordinary serializations.
- On capture into a `StoredHttpExchange` or `StoredStepRecord`,
  secret-typed fields land server-side as `RedactedString { value:
  String, reveal_id: Uuid }`. The cleartext is held in the ring
  alongside the rest of the record.
- The SSE wire form serializes `RedactedString` as
  `{"redacted": true, "reveal_id": "..."}`. The UI shows a
  `[Reveal]` button.
- Clicking `[Reveal]` posts
  `POST /api/v1/observability/reveal/{reveal_id}` and the server
  responds with the cleartext for that one field of that one
  record. The UI holds the result in memory for the current page
  session only (cleared on navigation away or reload).
- Known classes redacted at capture without case-by-case opt-in:
  - HTTP `Authorization`, `Cookie`, `Set-Cookie` headers.
  - MAM session cookie wherever it appears in a body.
  - Any `SecretString` field in machine state snapshots.
- The reveal endpoint is rate-limited and logged (without the
  revealed value).

---

## Engineering contracts

These are the safety/discipline rules the implementation must
satisfy. Called out separately because the design only works if
they hold.

### EC-1 — `RuntimeTap`/`HttpTap` `observed_*` methods must not block

- No `await`. No mutex acquisition that can be held by another tap
  caller.
- No allocation that can fail observably. Bounded buffers; drop
  oldest when full.
- No panic across the trait boundary. If serialization fails or a
  body exceeds budget, replace the field with a truncation marker
  and increment a counter — never bubble.
- The runtime invokes `observed_step` after `apply`. A hung or
  slow tap implementation must not block the *next* iteration
  either — observation is dispatched to an internal channel that
  drops on overflow.

### EC-2 — Gate methods are the *only* places the tap may park

- `gate_event`, `gate_outcome`, `gate_request` may `await` a
  semaphore permit; that is their job.
- They must check the relevant pause flag with a single atomic
  load and return immediately when not set and no matching
  breakpoint is active.
- They must publish a `CoreStatus` transition before parking and
  after releasing.

### EC-3 — Ring eviction cleans indices

When a `StoredStepRecord` leaves a ring (count or byte budget),
every `action_id` and `publish_id` it owned is removed from the
controller's indices, plus from the React-side mirror via an
explicit `Evicted { ids }` SSE message. The UI shows "parent
evicted" rather than producing dangling jumps.

### EC-4 — Body budgets are enforced at capture

Request and response bodies are inspected against
`max_request_body_bytes` and `max_response_body_bytes`. Oversized
bodies become `BodyCapture::Truncated { kind, captured, original_len }`
where `captured` is the first `max_*_body_bytes` of content (as
JSON if parseable, otherwise as string).

### EC-5 — Always-on is bounded and lossy under pressure

Always-on capture must never backpressure the runtime. The runtime
→ controller channel is bounded; on overflow, the runtime side
drops with a per-core counter increment; the runtime never waits.

### EC-6 — Observer cannot change observable behavior

For each core, running with `NullRuntimeTap` + `NullHttpTap` and
running with a recording tap must produce the same machine state,
the same `Outcome`s, and the same publish order for the same event
sequence. Validated by acceptance test #1.

### EC-7 — IDs are runtime-side only

`Uuid::new_v4` is called inside `ServiceRuntime`, not inside
`Machine::handle` or `Machine::handle_command`. Property tests
remain pure-function tests.

---

## Redesign principles

### P1 — Per-core gate is the primitive; global is convenience

Three gate points, each per-core, each independently toggleable:

| Gate          | Lives in              | Fires before                         | Parked status        |
| ------------- | --------------------- | ------------------------------------ | -------------------- |
| Event gate    | `ServiceRuntime`      | `machine.handle(event)`              | `ParkedAtEvent`      |
| Outcome gate  | `ServiceRuntime`      | `self.apply(actions, publishes)`     | `ParkedAtOutcome`    |
| HTTP gate     | each HTTP client      | `client.execute(req)`                | `ParkedAtHttp`       |
| Action gate   | *(deferred)*          | *(between dispatches in one apply)*  | —                    |

Global Pause = set every core's pause flag. Global Step = add a
permit to every paused core's semaphore. Per-core operations are
the same primitive scoped to one `CoreId`.

### P2 — Observation is always on; gating is sometimes on

Five always-populated streams: per-core StoredStepRecord rings, the
cross-core HTTP ring, the cross-core log ring. The `gate_*` methods
are the only place the tap may park. EC-1 and EC-5 keep observation
non-blocking.

### P3 — Each per-core StoredStepRecord binds event + actions + publishes + state

One event in, all side effects out, state after, threaded IDs. The
operator collapses each record to a one-liner ("`StatusFetched` →
0 actions, 1 publish, no change") or expands. HTTP exchanges live
in the cross-core ring and join back to a step by `action_id`.

### P4 — Causal threading: bidirectional, by ID

Every action and publish has a Uuid (assigned by the runtime).
Every event carries `cause: EventCause`. Controller indices give
O(1) backward lookups.

| From            | To                       | How                                          |
| --------------- | ------------------------ | -------------------------------------------- |
| event           | its actions / publishes  | containment inside the StoredStepRecord      |
| event           | its cause                | `event_cause` field                          |
| action          | its parent event         | `action_id → step_id` index                  |
| action          | resulting events         | filter events with `cause = Action(this_id)` |
| publish         | its parent event         | `publish_id → step_id` index                 |
| publish         | resulting events         | filter events with `cause = Publish(this_id)`|
| HTTP exchange   | its action               | exchange carries `action_id`                 |
| HTTP exchange   | parent event (two hops)  | `action_id → step_id` index                  |

### P5 — Per-core machine state, full snapshot in every StoredStepRecord

`Machine::state_snapshot()` returns an owned `Self::StateSnapshot`
which serializes into the record. State Δ is computed in the React
UI against the previous record's `state_after`. The runtime does no
diff work.

### P6 — Breakpoints are per-variant; one flat list

Event variants, action variants, publish variants, and HTTP-URL
patterns. The controller routes each breakpoint to the owning core's
gate: event variants → `gate_event`; action/publish variants →
`gate_outcome` (this is the v1 reason `gate_outcome` exists at all);
HTTP URL patterns → `gate_request`.

### P7 — MAM rate-limit guardrail = "HTTP tap flips MAM's pause"

When the MAM client detects two requests issued within the minimum
interval, it calls `controller.pause(CoreId::Mam)` from inside its
`HttpTap::gate_request` and the second request parks before it
goes out. Other cores keep running.

Strict win over legacy: the violation parks *before* `execute()`,
not after. The bad request never leaves the host.

### P8 — Deletes

- `DebugDispatcher`, `DebuggableEventStream`,
  `DebugState.debug_mode: bool`, `DebugState.latest_state`,
  `TraceEntry.state_before/after`, `PausedOn::Action {index, of}`,
  dryrun, queue manipulation, action breakpoints in their current
  shape, `HttpObserver = Arc<dyn Fn(HttpExchange)>`, the entire
  current `/debug` React route, the "debug mode" name, the operator/
  maintainer toggle, the merged cross-core stream view.

---

## Frontend layout (v1 sketch)

One page, three regions plus a header.

```
┌─ Header ──────────────────────────────────────────────────────────┐
│ Observability  ●Live   [Pause All]  [Step All]                    │
└───────────────────────────────────────────────────────────────────┘

┌─ Cores rail (left, ~240 px) ─┐  ┌─ Selected core: StepRecord stream ─┐
│ VPN     ▶ running             │  │ ─ MAM ─                             │
│ qBit    ‖ park @ event Auth…  │  │                                     │
│ ▶ MAM   ‖ park @ http POST /… │  │ 18:42:17.013  StatusFetched  0.2ms ▶│
│ DB      ▶ running             │  │   actions: 0   publishes: 1         │
│ Disk    ▶ running             │  │   state Δ: connectable: false → true│
│ Docker  ▶ running             │  │   ↳ Publish(MamConnectable)         │
│ Domain  ▶ running             │  │      [→ 1 resulting event in Domain]│
│                               │  │                                     │
│ Selected core controls:       │  │ 18:42:16.002  TimerFired(KeepAlive)▼│
│ [Pause MAM]   [Step MAM]      │  │   cause: External(Timer{KeepAlive}) │
│ [Pause all]   [Step all]      │  │   actions:                          │
│                               │  │   • FetchStatus  → MAM /jsonLoad.php│
│ Breakpoints: [manage…]        │  │       200, 75ms  [view req/res]     │
│                               │  │   publishes: 0                      │
│ Drops: 0 step, 0 http, 0 trunc│  │   state Δ: last_status_at +1.0s     │
│                               │  │                                     │
│                               │  │ 18:42:15.001  AuthSucceeded   …     │
└───────────────────────────────┘  └─────────────────────────────────────┘
┌─ Bottom strip: tabs ──────────────────────────────────────────────┐
│ [HTTP] [Logs]                                                     │
│ 18:42:16.077  MAM   POST /update_seedbox   200  150ms             │
│ 18:42:16.002  MAM   GET  /jsonLoad.php     200   75ms             │
│ 18:42:14.500  qBit  POST /api/v2/auth      200   30ms  [Reveal]   │
└───────────────────────────────────────────────────────────────────┘
```

Click behaviors (the causal graph by hand):

- Click an **action row** → highlight its HTTP exchanges in the
  HTTP tab and any event in any core whose cause is this action.
- Click a **publish row** → highlight every downstream event in
  every core whose cause is this publish.
- Click an **event row** → if cause is `Action(uuid)` or
  `Publish(uuid)`, jump to the originating row (may be in another
  core). If `External`, expand to show the source.
- Click an **HTTP exchange** in the bottom tab → jump to its
  action's parent event row in the originating core's stream.
- Click a **`[Reveal]` button** on a redacted field → fetches
  cleartext from `POST /api/v1/observability/reveal/{reveal_id}`;
  value visible until the next navigation/reload.

Drops/truncation counters are visible on the cores rail and per-row.

Not on the page: queue editing, inject, reorder, delete, dryrun,
state-diff against legacy `SystemState`, "Enable Debug Mode"
toggle, operator/maintainer mode toggle, merged cross-core stream.

---

## Configuration

```toml
[observability]
step_records_per_core         = 500
step_record_bytes_per_core    = "4MiB"
http_exchanges                = 500
http_exchange_bytes_total     = "8MiB"
max_request_body_bytes        = "64KiB"
max_response_body_bytes       = "256KiB"
```

Defaults match these literals. All keys honor SI byte suffixes
(`KiB`, `MiB`). Rings enforce both `*_per_core` count and `*_bytes`
budget, whichever is reached first. Body caps trigger
`BodyCapture::Truncated` with `original_len` preserved.

`PAUSE_ON_START` env var (boot-time only):

```
PAUSE_ON_START=true         # all seven cores pre-paused at startup
PAUSE_ON_START=mam,qbit     # selected cores pre-paused
PAUSE_ON_START unset        # default: all cores running
```

---

## Acceptance tests (design-level)

Every story below must keep these passing once they're written in
§37pre. They encode the safety guarantees.

1. **Observer equivalence.** For each core, running with
   `NullRuntimeTap + NullHttpTap` vs running with the live taps
   produces the same machine state, the same `Outcome`s, and the
   same publish order over the same event sequence. (EC-6)
2. **Observer cannot block dispatch.** Simulate: full ring, closed
   SSE channel, slow serializer, tap panic (contained). Event
   handling and `apply` still proceed. The only place the runtime
   may park is an explicit gate. (EC-1, EC-5)
3. **Per-core pause isolation.** Pause MAM, feed events to MAM
   and qBit. MAM parks at the event gate; qBit continues to
   completion.
4. **HTTP gate prevents send.** The MAM rate-limit guardrail
   trips the gate; the test server receives only the first
   request until the operator releases. (Validates P7.)
5. **Ring eviction cleans indices.** Fill a core's step ring past
   capacity; the controller's `action_id`/`publish_id` indices
   for evicted steps are removed; SSE emits `Evicted { ids }`;
   the UI shows "parent evicted" rather than jumping to nothing.
   (EC-3)
6. **Secret behavior.** `tracing` logs, `Debug` impls, and default
   `Serialize` of any `SecretString`-carrying type produce
   `[REDACTED]`. The observability SSE wire form produces
   `{redacted: true, reveal_id: ...}`. The reveal endpoint returns
   cleartext only for the requested `reveal_id` and only for an
   in-ring record. (Decision 14.)

---

## Open questions

Narrow, all out-of-scope-for-v1:

1. **Per-action gate revival.** Trait surface supports adding
   `gate_action(...)` later. Open until anyone misses it.
2. **HTTP ring de-dup of repeated polling.** Default: do nothing
   for v1; revisit if it becomes annoying.
3. **Auth on `/observability`.** Secrets policy (Decision 14)
   keeps the page screenshot-safe by default, so auth is a true
   later story rather than a blocker.

---

## Proposed next steps

Operator-readiness §37 stays as the umbrella story; §37pre and
§37a–j live underneath. Sequencing is strict-ish — parallel only
where there is no shared API churn.

1. **§37pre — finalize observability contracts.** No code. Lock
   the engineering contracts (EC-1..7), the trait shapes, the
   stored-record formats, timestamp conventions, the `CoreStatus`
   enum, the `Configuration` schema, the secrets reveal endpoint,
   and the six acceptance tests above. Output: a "ready to
   implement" checklist signed off in this doc. Blocks every
   other story.
2. **§37a — `secrecy::SecretString` adoption.** Wrap MAM cookie,
   qBit password, any other in-code secrets. Custom serializer
   produces `RedactedString { value, reveal_id }` for the
   observability path only; defaults to `[REDACTED]` everywhere
   else. May land in parallel with §37b once §37pre is signed off.
3. **§37b — `Machine::state_snapshot`.** Add `type StateSnapshot:
   Serialize + Send + 'static` and `fn state_snapshot(&self) ->
   Self::StateSnapshot`. Each machine implements an initially
   1:1 snapshot of its current internal state; smaller projections
   come later if needed. Parallel-safe with §37a.
4. **§37c — `Timed<E>` causal extension.** Add `cause: EventCause`,
   `ExternalCause`, constructors. Update every event-construction
   site. Subscriber bridges that translate publish → event in
   another core set the cause to `Publish(publish_id)`. Must
   complete before §37f.
5. **§37d — `RuntimeTap` + envelopes + event/outcome gates.**
   Create `windlass-observability` crate. Add
   `tap: Arc<dyn RuntimeTap>` to `ServiceRuntime`; `NullRuntimeTap`
   default. Implement `ObservabilityController` with per-core
   pause flag, step semaphore, `CoreStatus`. Add envelope
   construction, `gate_event` and `gate_outcome`. Drop
   `DebugDispatcher` and `DebuggableEventStream`. Depends on §37c.
6. **§37e — `HttpTap` + HTTP gate.** Replace `HttpObserver` with
   `HttpTap` in `windlass-types`. Update `MamClient`,
   `QbitClient`, and any other HTTP client. Wire the MAM
   rate-limit guardrail through `gate_request`. Depends on §37d
   for the shared controller.
7. **§37f — Stored records + always-on rings + indices.** Implement
   `StoredStepRecord`, `StoredHttpExchange`, `BodyCapture`, ring
   storage with count + byte budgets, eviction with index
   cleanup, drop/truncation counters, SSE shape. Depends on §37b,
   §37c, §37d, §37e.
8. **§37g — Variant-keyed breakpoint registry.** Flat breakpoint
   list (event variants, action variants, publish variants, HTTP
   URL patterns); routes each to the owning core's gate. Depends
   on §37d + §37e.
9. **§37h — New `/observability` frontend.** Wholesale replacement
   of `app/src/routes/Debug.tsx`. New SSE consumer, components for
   cores rail, per-core StepRecord stream with collapsed/expanded
   rows, in-browser state diff, click-to-causal-jump, HTTP/logs
   tabs, Reveal UX, drops/truncation counters. Depends on §37f +
   §37g.
10. **§37i — `PAUSE_ON_START` env var.** Parse `true` or
    comma-separated list; construct each runtime with its
    per-core pause flag pre-set accordingly. Independent and tiny.
11. **§37j — Rename + canonical doc rewrite.** `windlass-debug` →
    `windlass-observability`, `/debug` → `/observability`,
    `docs/debug-mode.md` → `docs/observability.md` as the
    canonical spec. Mechanical rename + canonical spec rewrite.
    Last step.

Dependency summary: §37pre → (§37a ∥ §37b) → §37c → §37d → §37e →
§37f → §37g → §37h. §37i is independent. §37j is last.

Each story is small. The discuss-first cost is paid in this doc;
§37pre formalizes the contracts; implementation stories should
not need their own design rounds.
