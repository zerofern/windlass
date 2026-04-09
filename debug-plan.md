# Debug Redesign — Full Observability & Control

## Problem

The current debug page requires you to be watching the SSE stream to see anything useful.
If you open it after the boot sequence, you see nothing. There is no event queue visibility,
no action lifecycle tracking, no causal chain, no log output, no history.

## Goal

Open the debug page at any time → see the full picture:

- Ordered event queue (what's waiting, with payloads)
- What event is currently being processed
- What actions it produced and which are still running
- A scrollable trace of all past events with state diffs and actions
- All log output from `tracing` macros
- Full control: step, skip, inject, edit, delete, reorder events
- Dry-run: preview what any event would do without dispatching

## Architectural Principles

- **`DebugStore` is the authoritative state store** — not observations, not SSE history.
  `GET /api/v1/debug` always returns the complete current picture.
- **Only active when debug mode is on.** When off, DebugStore is minimal (no history,
  no trace, no logs). Enabling debug mode starts capturing immediately.
- **General SSE stream (`/api/v1/stream`) is unchanged** — carries only `StateSnapshot`
  and `DebugModeChanged`. Nothing debug-specific.
- **Debug SSE stream (`/api/v1/debug/stream`)** — silent when debug mode is off.
  When on, pushes a seq-tagged `DebugState` snapshot on every change, plus log lines.
- **`Observation` enum shrinks to 2 variants**: `StateSnapshot`, `DebugModeChanged`.
  `EventArrived`, `EventReceived`, `ActionDispatched`, `HttpExchange` are removed — their
  data now lives in `DebugStore`.
- **No Mutexes anywhere.** `DebugHistory` is owned exclusively by the main event loop
  (`&mut`). HTTP handlers send `DebugCommand` via mpsc; the loop drains them between
  events. Reads are served from an `ArcSwap<Arc<DebugState>>` the loop publishes after
  each mutation.

---

## Data Model

### DebugStore ownership split

`DebugController` is extended into `DebugStore`. It retains all existing hot-path fields
(lock-free) and gains a `DebugHistory` owned exclusively by the main loop.

**Hot-path fields** (unchanged from current DebugController — lock-free, cloned freely):

```rust
debug_mode:         Arc<AtomicBool>
step_semaphore:     Arc<Semaphore>
paused_on:          Arc<ArcSwap<Option<PausedOn>>>
event_breakpoints:  Arc<ArcSwap<HashSet<&'static str>>>
action_breakpoints: Arc<ArcSwap<HashSet<&'static str>>>
```

**Shared handles** (on `DebugStore`, accessed from HTTP handlers and SSE):

```rust
snapshot:   ArcSwap<Arc<DebugState>>       // published by main loop after each mutation
cmd_tx:     mpsc::Sender<DebugCommand>     // HTTP handlers send queue mutations here
log_tx:     mpsc::Sender<LogEntry>         // DebugLogLayer sends log lines here
notify_tx:  broadcast::Sender<u64>         // broadcasts seq on every snapshot update
```

**History** (owned exclusively by main loop — zero contention, zero locking):

```rust
pub struct DebugHistory {
    seq:             u64,
    event_queue:     VecDeque<StoredEvent>,
    current_event:   Option<ActiveEvent>,
    running_actions: Vec<RunningAction>,
    trace:           VecDeque<TraceEntry>,     // capped at 200
    logs:            VecDeque<LogEntry>,        // capped at 500
    latest_state:    SystemState,              // SystemState::initial() at boot; always valid
}
```

Main loop drains `cmd_rx` and `log_rx` between events:

```rust
loop {
    tokio::select! {
        event = debug_stream.recv() => { /* process */ }
        cmd   = cmd_rx.recv()       => { history.apply_cmd(cmd); publish(&store, &history); }
        log   = log_rx.recv()       => { history.append_log(log); publish(&store, &history); }
    }
}
```

`publish()` serialises `DebugState` from history, stores it in `store.snapshot`, increments
`seq`, and broadcasts the new seq on `notify_tx`.

### Sequence counter for SSE/GET race

Every `DebugState` snapshot carries `seq: u64`. Frontend connect sequence:

```
1. Subscribe to /api/v1/debug/stream  (SSE events begin arriving, each with seq)
2. GET /api/v1/debug                  (receive snapshot with seq=N)
3. Discard any buffered SSE events where event.seq <= N
4. Apply subsequent SSE events in order
```

### QueueSink — race-free mode transition

The intake task holds an `Arc<ArcSwap<QueueSink>>`:

```rust
enum QueueSink {
    Mpsc(mpsc::Sender<Event>),           // debug mode off
    VecDeque(mpsc::Sender<StoredEvent>), // debug mode on
}
```

`enable_debug()` atomically swaps the sink to VecDeque. `disable_debug()` swaps back.
The intake task calls `arc_swap.load()` per event — no lock, no race.

### HttpObserver type change

After `HttpExchange` is removed from `Observation`:

```rust
// windlass-core/src/lib.rs
pub type HttpObserver = Arc<dyn Fn(HttpExchange) + Send + Sync>;
```

`HttpExchange` moves to `windlass-types` so both `windlass-core` and `windlass-debug`
can reference it without a circular dependency. Clients call `on_http(exchange)` directly.

### Core types

```rust
pub struct StoredEvent {
    pub id:               Uuid,
    pub at:               DateTime<Utc>,    // event.at
    pub arrived_at:       DateTime<Utc>,
    pub variant:          &'static str,
    pub payload:          Value,
    pub caused_by_action: Option<Uuid>,
}

pub struct ActiveEvent {
    pub stored:       StoredEvent,
    pub state_before: SystemState,
    pub started_at:   DateTime<Utc>,
    pub actions:      Vec<ActionEntry>,
}

pub struct ActionEntry {
    pub id:              Uuid,
    pub variant:         &'static str,
    pub payload:         Value,
    pub parent_event_id: Uuid,
    pub started_at:      DateTime<Utc>,
    pub completed_at:    Option<DateTime<Utc>>,
    pub caused_event_id: Option<Uuid>,
    pub http_exchanges:  Vec<HttpExchange>,
}

pub struct RunningAction {
    pub id:              Uuid,
    pub variant:         &'static str,
    pub payload:         Value,
    pub parent_event_id: Uuid,
    pub started_at:      DateTime<Utc>,
}

pub struct TraceEntry {
    pub event:        StoredEvent,
    pub state_before: SystemState,
    pub state_after:  SystemState,
    pub actions:      Vec<ActionEntry>,
    pub completed_at: DateTime<Utc>,
}

pub struct LogEntry {
    pub at:      DateTime<Utc>,
    pub level:   String,
    pub target:  String,
    pub message: String,
}

pub enum DebugCommand {
    RemoveQueuedEvent(Uuid),
    EditQueuedEvent(Uuid, Value, oneshot::Sender<Result<(), String>>),
    InjectEvent {
        variant:  String,
        payload:  Value,
        position: Option<usize>,
        at:       DateTime<Utc>,    // defaults to Utc::now() server-side if absent
        reply:    oneshot::Sender<Result<Uuid, String>>,
    },
    ReorderQueue(Vec<Uuid>, oneshot::Sender<Result<(), String>>),
}
```

### DebugHistory methods (main loop only — no locking needed)

```rust
fn event_arrived(&mut self, event: &Event, caused_by: Option<Uuid>) -> Uuid
fn event_started(&mut self, event_id: Uuid, state_before: SystemState)
fn action_started(&mut self, action: &Action, parent_event_id: Uuid) -> Uuid
fn action_http_exchange(&mut self, action_id: Uuid, exchange: HttpExchange)
fn action_completed(&mut self, action_id: Uuid, caused_event_id: Option<Uuid>)
fn event_completed(&mut self, event_id: Uuid, state_after: SystemState)
fn append_log(&mut self, entry: LogEntry)
fn apply_cmd(&mut self, cmd: DebugCommand)
fn latest_state(&self) -> &SystemState    // always valid — initial() before first event
```

---

## Causation Tracking (CausalTx)

To link "AuthenticateQbit caused QbitAuthSuccess", HTTP-result action handlers use a
`CausalTx` instead of `self.tx`.

**Not used for:** `ScheduleWakeup`, `StopDependentContainers`, `StartDependentContainers`,
`RestartGluetun`, `SendGotifyAlert` (fire-and-forget or timer-based; WakeupId is the
implicit link for wakeups).

**Used for:** `ReadPortFiles`, `FetchAndDumpAllLogs`, `AuthenticateQbit`, `SyncQbitPort`,
`UpdateMam`, `CheckMamConnectability`, `CheckDiskSpace`, `CheckNewTorrents`.

### Implementation

A dedicated causation channel in `shell/mod.rs`:

```rust
let (causal_tx_inner, causal_rx) = mpsc::channel::<(Event, Uuid)>(128);
```

`CausalTx` (`windlass-debug/src/causal_tx.rs`) wraps `mpsc::Sender<(Event, Uuid)>` and
carries an action ID. Calling `causal_tx.send(event)` forwards `(event, action_id)`.

`DebuggableEventStream` reads from both `external_rx` and `causal_rx` via `select!`:

- From `external_rx`: `history.event_arrived(event, None)`
- From `causal_rx`: `history.event_arrived(event, Some(action_id))`

`DebugDispatcher::dispatch` creates a `CausalTx` per action and passes it to `execute`:

```rust
pub async fn dispatch(
    &self,
    event_id: Uuid,
    actions: Vec<Action>,
    mut execute: impl FnMut(Action, CausalTx),
)
```

Each HTTP-result handler captures `causal_tx` instead of `self.tx`:

```rust
pub(super) fn authenticate_qbit(&self, causal_tx: CausalTx) {
    let qbit = self.qbit.clone();
    tokio::spawn(async move {
        let event = qbit.authenticate().await;
        let _ = causal_tx.send(event).await;
    });
}
```

When debug mode is off: `CausalTx::send` just forwards to plain `tx`. Zero overhead.

---

## HTTP Exchanges per Action

Uses `tokio::task_local!`:

```rust
tokio::task_local! {
    pub(crate) static CURRENT_ACTION_ID: Option<Uuid>;
}
```

`CausalTx` is constructed per action and sets the task-local ID when the spawned task
runs: `CURRENT_ACTION_ID.scope(Some(action_id), future)`.

The `on_http` implementation in `windlass-debug` reads the task-local:

```rust
Arc::new(move |exchange: HttpExchange| {
    if !debug_mode.load(Ordering::Relaxed) { return; }
    let id = CURRENT_ACTION_ID.try_with(|id| *id).ok().flatten();
    // forward exchange + id to main loop via log_tx (or a dedicated exchange_tx)
})
```

The main loop routes it to `history.action_http_exchange(action_id, exchange)`.

---

## Log Streaming (DebugLogLayer)

New file: `windlass-debug/src/log_layer.rs`

```rust
pub struct DebugLogLayer {
    log_tx:     mpsc::Sender<LogEntry>,
    debug_mode: Arc<AtomicBool>,
}

impl<S: Subscriber> Layer<S> for DebugLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if !self.debug_mode.load(Ordering::Relaxed) { return; }
        let entry = LogEntry { /* extract level, target, message */ };
        let _ = self.log_tx.try_send(entry);   // bounded send; drop if full (non-blocking)
    }
}
```

Registered at startup in `windlass/src/main.rs`:

```rust
tracing_subscriber::registry()
    .with(fmt_layer)
    .with(DebugLogLayer::new(store.log_tx.clone(), store.debug_mode()))
    .init();
```

---

## API Changes

### Existing endpoints (behaviour unchanged, response body of GET expanded)

```
GET    /api/v1/debug
POST   /api/v1/debug/enable
POST   /api/v1/debug/disable
POST   /api/v1/debug/step
POST   /api/v1/debug/skip
GET    /api/v1/debug/events
GET    /api/v1/debug/actions
POST   /api/v1/debug/breakpoints/event/{variant}
DELETE /api/v1/debug/breakpoints/event/{variant}
POST   /api/v1/debug/breakpoints/action/{variant}
DELETE /api/v1/debug/breakpoints/action/{variant}
```

`GET /api/v1/debug` returns the full `DebugState`: seq, event_queue, current_event,
running_actions, trace (last 200), logs (last 500), breakpoints, paused_on.

### New endpoints

```
GET    /api/v1/debug/stream          SSE: push DebugState snapshot on change + log lines
DELETE /api/v1/debug/queue/{id}      Remove a queued event
PUT    /api/v1/debug/queue/{id}      Edit queued event payload (JSON body)
POST   /api/v1/debug/queue           Inject event { variant, payload, position?, at? }
PUT    /api/v1/debug/queue/order     Reorder queue { ids: [Uuid] }
POST   /api/v1/debug/dryrun          Preview: { variant, payload } → { state_diff, actions }
```

### Observation enum (windlass-core) — after cleanup

```rust
pub enum Observation {
    StateSnapshot(SystemState),
    DebugModeChanged(bool),
}
```

---

## Frontend Redesign (Debug.tsx)

**Connect sequence:**

1. Subscribe to `/api/v1/debug/stream` SSE
2. Call `GET /api/v1/debug` for initial snapshot (seq=N)
3. Discard any buffered SSE events with seq ≤ N
4. Render snapshot; apply SSE diffs in order

**Layout:**

```
┌─ Header: Debug Mode toggle · Step · Skip · status badges ───────────────────┐

┌─ Event Timeline (left, scrollable) ──┐  ┌─ Detail panel (right) ───────────┐
│                                       │  │ (click any event to select)       │
│ ● PROCESSING: QbitAuthSuccess         │  │                                   │
│   ├─ ✓ ScheduleWakeup(Heartbeat)     │  │ State diff                        │
│   ├─ ⟳ UpdateMam (1.2s running)      │  │ - mam: Authenticating             │
│   └─ ✓ SendGotifyAlert              │  │ + mam: Connected                  │
│                                       │  │                                   │
│ ▼ QUEUE (2 waiting)                   │  │ Actions                           │
│   [Wakeup(DiskCheck)]  [preview][edit][×]│ AuthenticateQbit                 │
│   [DockerGluetunDied]  [preview][edit][×]│  HTTP: POST /api/v2/auth → 200  │
│   [+ Inject event]  [⇅ Reorder]       │  │  caused → QbitAuthSuccess        │
│                                       │  └──────────────────────────────────┘
│ ── TRACE ──────────────────────────── │
│ Init               → 3 actions        │  ┌─ Log output ──────────────────── ┐
│ QbitAuthSuccess    → 1 action         │  │ INFO  windlass::shell             │
│ QbitPortSyncSuccess→ 1 action         │  │   Windlass started                │
│ MamUpdateSuccess   → 1 action         │  │ INFO  windlass_debug::stream      │
│ ...                                   │  │   Debug mode enabled              │
└───────────────────────────────────────┘  └───────────────────────────────────┘
```

- **Preview button** per queued event: calls `POST /api/v1/debug/dryrun`, shows overlay
  with state diff + actions — no dispatching.
- **Edit button**: inline JSON editor for queued event payload.
- **Inject**: variant picker dropdown + JSON template editor with `at` field pre-filled
  to current time (editable).
- **Reorder**: drag-to-reorder or up/down arrows.
- **Log panel**: auto-scroll, colour-coded by level (INFO=grey, WARN=yellow, ERROR=red).

---

## Implementation Phases & Todos

### Phase 1 — DebugStore foundation (no user-visible change yet)

- Define `DebugHistory`, `StoredEvent`, `ActiveEvent`, `ActionEntry`, `RunningAction`,
  `TraceEntry`, `LogEntry`, `DebugCommand` in `windlass-debug/src/history.rs` (keep
  under 300 lines; split into `windlass-debug/src/types.rs` if needed)
- Extend `DebugStore` (currently `DebugController`) to hold `snapshot: ArcSwap`,
  `cmd_tx`, `log_tx`, `notify_tx`
- Add `DebugHistory` ownership to `shell/mod.rs` main loop; wire `cmd_rx`, `log_rx`
  into `select!`
- Integrate `event_arrived`/`event_started` into intake task
- Integrate `action_started`/`action_completed`/`event_completed` into `DebugDispatcher`
  (dispatcher needs `event_id` + state snapshots passed in from shell)
- Implement `publish()`: serialize history → `Arc<DebugState>` → store in ArcSwap →
  broadcast seq on `notify_tx`
- Update `GET /api/v1/debug` to return full `DebugState` from snapshot

### Phase 2 — SSE push + Observation cleanup

- Implement `/api/v1/debug/stream` SSE handler: subscribe to `notify_tx`, push snapshot
  on each seq notification; push log lines as they arrive
- Remove `EventArrived`, `EventReceived`, `ActionDispatched`, `HttpExchange` from
  `Observation` enum in `windlass-core`
- Remove the corresponding `obs_tx.send(...)` calls from shell, dispatcher, intake task
- Update frontend to use new SSE stream with seq-based dedup

### Phase 3 — Queue redesign for manipulation

- Add `QueueSink` enum + `ArcSwap<QueueSink>` to `DebuggableEventStream`
- On `enable_debug()`: swap sink to VecDeque path; on `disable_debug()`: swap back
- Implement `DebugHistory::apply_cmd` for all four `DebugCommand` variants:
  - `RemoveQueuedEvent`: pop by ID from VecDeque
  - `EditQueuedEvent`: replace payload, validate by deserializing to `Event`
  - `InjectEvent`: insert at position (or push_back), default `at` to `Utc::now()`
  - `ReorderQueue`: reorder VecDeque by provided ID list; error on unknown IDs
- Add REST endpoints in `windlass-web/src/routes/debug.rs`
- Add queue manipulation UI to Debug.tsx

### Phase 4 — CausalTx

- Implement `CausalTx` in `windlass-debug/src/causal_tx.rs`
- Add causation channel `mpsc::Sender<(Event, Uuid)>` in `shell/mod.rs`
- `DebuggableEventStream` reads from both `external_rx` and `causal_rx` via `select!`
- `DebugDispatcher::dispatch` creates `CausalTx` per action, passes to `execute` callback
- `ShellContext::execute(action, CausalTx)` — route `causal_tx` to each handler
- Update HTTP-result action handlers to use `causal_tx` instead of `self.tx`
- Wire `caused_event_id` into `ActionEntry` and `caused_by_action` into `StoredEvent`

### Phase 5 — Log streaming

- Implement `DebugLogLayer` in `windlass-debug/src/log_layer.rs`
- Layer calls `log_tx.try_send(entry)` (non-blocking; bounded channel, drops if full)
- Register layer in `windlass/src/main.rs` alongside `fmt` subscriber
- Add tracing-subscriber dependency to `windlass-debug`
- Log lines already included in `DebugState` via Phase 1; SSE streams them via Phase 2

### Phase 6 — HTTP exchanges per action

- Add `tokio::task_local! { CURRENT_ACTION_ID: Option<Uuid> }` in `windlass-debug`
- `CausalTx` sets the task-local in scope around the spawned task
- `HttpObserver` type changes: `Arc<dyn Fn(HttpExchange) + Send + Sync>`
- Move `HttpExchange` to `windlass-types`
- `on_http` impl in `windlass-debug` reads task-local; sends exchange via a dedicated
  `exchange_tx: mpsc::Sender<(Uuid, HttpExchange)>` that the main loop drains and routes
  to `history.action_http_exchange()`
- Display per-action HTTP exchanges in Detail panel

### Phase 7 — Dryrun

- `DebugHistory::latest_state()` returns `&SystemState` (always valid — initial() at boot)
- `POST /api/v1/debug/dryrun`: deserialize event from JSON, call `state.process_event()`
  on a clone of `latest_state`, return `{ state_before, state_after, state_changed, actions }`
- Add Preview button per queued event in Debug.tsx

### Phase 8 — Frontend full redesign

- Replace Debug.tsx with new layout (timeline + detail + queue + log panels)
- Timeline: scrollable trace list; click to select; shows processing indicator and queue
- State diff component: renders changed fields between state_before and state_after
- Queue component: per-item edit/delete/preview + inject modal + reorder
- Log component: auto-scroll, level colours
- Running actions indicator (spinner per in-flight action in trace)

---

## Notes

- **ScheduleWakeup causation**: tracked via `WakeupId` domain field, not `CausalTx`.
  Both trace entry and queue show WakeupId prominently — the link is visually clear.
- **Debug mode off overhead**: `DebugLogLayer.on_event` is a single atomic load + early
  return. `CausalTx::send` when off forwards directly to `tx` with no allocation.
  `QueueSink` is an ArcSwap load per intake event. Total overhead ≈ 0.
- **Trace memory**: 200 entries × (2 × ~2 KB SystemState + action payloads) ≈ a few MB.
  Only allocated when debug mode is on.
- **`at` field on injected events**: defaulted to `Utc::now()` server-side; exposed as an
  editable field in the JSON template so users can backdate synthetic events.
- **Edit validation**: `EditQueuedEvent` deserializes the new payload back into `Event`
  before storing. Returns error string if deserialization fails.
- **File size**: `windlass-debug` will grow significantly. Split into focused files:
  `store.rs` (DebugStore outer struct), `history.rs` (DebugHistory + methods),
  `types.rs` (StoredEvent, ActionEntry, etc.), `causal_tx.rs`, `log_layer.rs`,
  `dispatcher.rs` (existing), `stream.rs` (existing). Each must stay under 300 lines.
