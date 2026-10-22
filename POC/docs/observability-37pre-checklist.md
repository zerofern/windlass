# §37pre — Observability Contracts Sign-Off Checklist

**Status: locked 2026-06-01.** §37a and §37b may start in parallel;
all other §37 stories proceed in the dependency order named in the
redesign doc. Implementation stories reference this checklist as the
spec; they do not re-open the decisions below.

This was the work artifact for §37pre. It walks every contract the
implementation stories need to lock and resolves the ambiguities the
redesign doc gestured at without pinning down.

## How to read this

- **§A** items are already locked in
  [`observability-redesign.md`](./observability-redesign.md) by
  citation; no new decision needed.
- **§B** items are genuine ambiguities. A proposal is given for each;
  the user signs off, pushes back, or names a different option.
- **§C** items are breaking-change migrations the implementation
  stories will perform. They need scope acknowledgement.
- **§D** items are test-harness pieces that have to be built to
  satisfy the six acceptance tests.
- **§E** is the sign-off list itself.

---

## A. Contracts already locked (citations into redesign doc)

| # | Contract                                       | Lives in (redesign doc)                                  |
|---|------------------------------------------------|----------------------------------------------------------|
| A1  | `RuntimeTap` trait methods                   | Architecture / `RuntimeTap`                              |
| A2  | `HttpTap` trait methods                      | Architecture / `HttpTap`                                 |
| A3  | `EventGateView`, `OutcomeGateView`, `HttpRequestView` field lists | Architecture (gate views)                |
| A4  | `ActionEnvelope`, `PublishEnvelope`          | Architecture / Envelopes                                 |
| A5  | `StepRecordView` (borrowed) + `StoredStepRecord` (owned) | Architecture / Borrowed vs owned                |
| A6  | `StoredAction`, `StoredPublish`, `StoredHttpExchange` | Architecture / Borrowed vs owned                    |
| A7  | `BodyCapture::{ Inline, Text, Bytes, Truncated, None }` | Architecture / Borrowed vs owned                  |
| A8  | `MaybeSecret`, `ServerSecretSlot`, `WireRedacted` | Architecture / Borrowed vs owned + Secrets             |
| A9  | `EventCause` + `ExternalCause` (runtime side) | Architecture / `Timed<E>` causal extension              |
| A10 | `StoredEventCause` + `StoredExternalCause` (wire side) | Architecture / `Timed<E>` causal extension       |
| A11 | `Machine::StateSnapshot: Serialize + Send + 'static` + `fn state_snapshot(&self)` | Architecture / Machine extension |
| A12 | `CoreStatus` enum with all five variants     | Architecture / `CoreStatus`                              |
| A13 | Engineering contracts EC-1..EC-8             | Engineering contracts                                    |
| A14 | Configuration schema (`[observability]`)     | Configuration                                            |
| A15 | `PAUSE_ON_START` semantics (`true` / list / unset) | Configuration                                       |
| A16 | Six acceptance tests                         | Acceptance tests                                         |
| A17 | Reveal endpoint URL, scope, `410 Gone` semantics | Architecture / Secrets                               |
| A18 | UI principles + supporting surfaces          | Frontend layout / UI principles                          |
| A19 | Runtime loop order (apply before observed_step; reserve_step_ids before apply) | Architecture / Runtime loop diff |

---

## B. Decisions to make now

### B1 — `CoreId` enum

The redesign doc references `CoreId` everywhere but never writes it
out. Propose:

```rust
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoreId { Vpn, Qbit, Mam, Db, Disk, Docker, Domain }
```

Closed enum. Order matches the cores rail in the frontend sketch.
Lowercased on the wire to keep SSE payloads stable.

### B2 — `StepKind` variants

Referenced in `StepRecordView` and `StoredStepRecord` but never
defined. **Locked shape:**

```rust
pub enum StepKind {
    Event,                                          // handle(timed_event)
    Command { response: CommandResponseStatus },    // handle_command(cmd)
}

pub enum CommandResponseStatus {
    Sent,             // oneshot::send succeeded
    ReceiverDropped,  // caller dropped the oneshot::Receiver before we replied
}
```

A `bool` would tell the UI something went differently without
telling the operator whether it mattered. The two-variant enum is
honest: every command has a typed response, and the operator sees
exactly which case fired.

### B3 — `BodyKind` variants

Referenced in `BodyCapture::Truncated { kind, ... }` but never
defined. Propose:

```rust
pub enum BodyKind { Json, Text, Form, Binary }
```

Determined at capture from `Content-Type` (request side) and
response headers (response side). Binary bodies record only their
length, never the contents — they truncate to `BodyCapture::Bytes`
unconditionally.

### B4 — `erased_serde` dependency

Trait shapes use `&dyn erased_serde::Serialize` for type-erased
serialization. Confirm we adopt the
[`erased-serde`](https://docs.rs/erased-serde) crate. Tiny dep, the
canonical way to do object-safe Serialize in Rust. Alternative is
serializing-at-callsite into `serde_json::Value` and passing that —
which the runtime already does for the event payload, so the trait
inputs could just take `&serde_json::Value` instead of
`&dyn erased_serde::Serialize`. **Proposal: drop `erased_serde` and
use `&serde_json::Value` throughout.** Simpler, no extra dep,
matches the existing serialization-at-callsite pattern in the
runtime loop diff.

### B5 — Secret serializer mechanism

How does `ServerSecretSlot` end up as `WireRedacted` on the SSE
wire, while the reveal endpoint returns cleartext?

```rust
impl Serialize for ServerSecretSlot {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut sm = s.serialize_struct("WireRedacted", 2)?;
        sm.serialize_field("redacted", &true)?;
        sm.serialize_field("reveal_id", &self.reveal_id)?;
        sm.end()
    }
}
```

The reveal endpoint accesses `slot.cleartext` by direct field access
(it never goes through `Serialize`). This means any code path that
takes a `ServerSecretSlot` and serializes it gets the redacted form
"for free" — there is no opt-in. **Proposal: hand-rolled `Serialize`
impl as above.**

### B6 — `reveal_id` lookup mechanism

When the operator clicks `[Reveal]`, the server resolves the
`reveal_id` to a cleartext. Two options:

- **(B6a) Separate index**: controller holds
  `HashMap<Uuid, (RecordHandle, FieldPath)>` updated on every
  capture and eviction. O(1) lookup, doubles per-secret memory.
- **(B6b) Scan rings**: server walks current step / HTTP rings
  looking for the matching `reveal_id`. O(N) but cheaper memory.

The reveal click is a rare operator action. With rings bounded at
~1000 records total, an O(N) scan is microseconds. **Locked: B6b
(scan rings).** Simpler; the saved memory matters more than the
saved lookup time.

**Scan covers both the step-record rings and the HTTP exchange
ring.** Missing `reveal_id` → `410 Gone`, never cleartext. There is
**no separate `reveal_id → CleartextSlot` index** on the controller
— the redesign doc's earlier mention of one is removed. Eviction
invalidates reveal IDs naturally because their parent slot is gone.

### B7 — Internal channel sizes for runtime → controller

EC-1 and EC-5 say `observed_*` writes to a bounded internal channel
that drops on overflow. **Locked:**

```rust
const STEP_RECORD_CHANNEL_SIZE: usize = 4096;     // events, actions, publishes
const HTTP_EXCHANGE_CHANNEL_SIZE: usize = 1024;   // HTTP exchanges
```

Compile-time constants, **documented in the same source file as the
observability config defaults** so a maintainer sees them together.
These are *internal channel capacities*, not ring sizes — ring
sizes and body caps remain user-tunable in `windlass.toml`; channel
sizes do not, because changing them rarely solves a real problem.

Drop-oldest on overflow with per-core counters surfaced in the
`Loss` SSE message (see B9). The rule for promoting to config is:
**only if the `Loss` counters show real pressure in production.**
The visible counter exists from day one so we know when that
moment has come; we don't pre-emptively add a knob.

### B8 — `PAUSE_ON_START` parsing rules

Beyond the doc's "`true`" / comma-list / unset, the doc doesn't pin
down parsing details. Propose:

- Empty string ≡ unset (all cores running).
- Case-insensitive: `True`, `TRUE`, `true` all accepted.
- `PAUSE_ON_START=all` ≡ `PAUSE_ON_START=true`.
- Comma-separated list: trim each token, lowercase, match against
  `CoreId` (vpn / qbit / mam / db / disk / docker / domain).
- Unknown token → fatal at startup with a clear error listing the
  valid names.

### B9 — SSE message envelope

The doc references several SSE message types ("new records,"
"`CoreStatus` changes," "`Evicted { ids }`") without defining the
wire envelope. **Locked:**

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SseMessage {
    Hello(HelloSnapshot),
    Step(StoredStepRecord),
    HttpExchange(StoredHttpExchange),
    Log(StoredLogLine),
    CoreStatus { core: CoreId, status: CoreStatus },
    Evicted(EvictedIds),
    Loss(LossCounters),
}

/// The single snapshot message sent immediately after connect,
/// before any incremental messages. The frontend boot path is:
///   empty local store → receive Hello → hydrate → apply deltas.
pub struct HelloSnapshot {
    pub protocol_version: u32,            // starts at 1
    pub cores: Vec<(CoreId, CoreStatus)>,
    pub steps: Vec<StoredStepRecord>,
    pub http: Vec<StoredHttpExchange>,
    pub logs: Vec<StoredLogLine>,
    pub loss: LossCounters,
    pub active_breakpoints: Vec<Breakpoint>,
}

pub struct EvictedIds {
    pub step_ids: Vec<Uuid>,
    pub action_ids: Vec<Uuid>,
    pub publish_ids: Vec<Uuid>,
    pub reveal_ids: Vec<Uuid>,
}

pub struct LossCounters {
    pub per_core: HashMap<CoreId, CoreCounters>,
    pub http: HttpCounters,
}
```

`Hello` carries everything a fresh client needs to hydrate:
ring contents *plus* current core statuses, *plus* loss counters,
*plus* active breakpoints. Without all of those the page would
boot inconsistent and have to chase `CoreStatus` / `Loss` deltas
it never saw. Reconnect logic stays boring.

---

## C. Migration scope (breaking changes to confirm)

| #  | Change                                                                                          | Lands in    |
|----|-------------------------------------------------------------------------------------------------|-------------|
| C1 | `Machine::Outcome::publish` → `Outcome::publishes` (one-field rename)                           | §37d        |
| C2 | `TopicFanout::send(&P)` → `TopicFanout::send(&PublishEnvelope<P>)`; subscribers receive envelopes so cross-core bridges can set `cause = Publish(publish_id)` on forwarded events | §37c + §37d |
| C3 | `ServiceRuntime<M, S>` gains `tap: Arc<dyn RuntimeTap>`                                         | §37d        |
| C4 | Every `Machine` impl: add `type StateSnapshot` + `fn state_snapshot(&self)` (initially 1:1 with internal state) | §37b |
| C5 | `Timed<E>` gains `cause: EventCause`; every event-construction site uses one of the three constructors | §37c   |
| C6 | Drop `windlass_debug::{ DebugDispatcher, DebuggableEventStream }`                               | §37d        |
| C7 | Drop the legacy `/debug` React route wholesale; replace with `/observability` (§37j renames endpoints too) | §37h + §37j |
| C8 | Every HTTP client (`MamClient`, `QbitClient`, etc.): replace `HttpObserver` parameter with `Arc<dyn HttpTap>`; build the `HttpRequestView` from typed inputs *before* `reqwest::Request::build()` | §37e |
| C9 | `Outcome::publishes` envelope conversion happens in the runtime, not in `apply`'s callers — `apply`'s signature changes to take envelope slices | §37d |
| C10 | Rename crate `windlass-debug` → `windlass-observability` (mechanical, atomic) | §37j |
| C11 | `reserve_step_ids` signature is ID-only: `(core, step_id, action_ids: &[Uuid], publish_ids: &[Uuid])`. No payload serialization, reinforcing EC-8. | §37d |

### Cross-core publish-ID preservation rule (qualifies C2)

The publishing core's runtime mints a `publish_id` once, into a
`PublishEnvelope<P>`. `TopicFanout` forwards the envelope unchanged.
Subscriber bridges that translate publish → event in another core
**preserve the envelope's `publish_id`** when constructing the
downstream `Timed::from_publish(now, publish_id, event)`. No
bridge mints a new publish_id. This is what makes the causal graph
real rather than decorative; bridge-side regression here would
silently break "jump to resulting events."

### HTTP request capture rule (qualifies C8)

**Hard rule:** `HttpRequestView` is constructed from the same
typed data used to build the `reqwest::Request`, **before**
`reqwest::Request::build()`. The observability layer never
introspects a built `reqwest::Request` body — streaming /
multipart / compressed bodies make post-build inspection
unreliable. This is enforced by code review, not by trait shape
(no good way to express it in Rust types).

### Apply ordering rule (qualifies C9)

`apply(&[ActionEnvelope<A>], &[PublishEnvelope<P>])` preserves
order exactly:

- Action envelopes are dispatched in the same order as
  `Outcome.actions`.
- Publish envelopes are fanned out in the same order as
  `Outcome.publishes`.

Order preservation is part of the observer-equivalence guarantee
and is asserted by acceptance test #1.

---

## D. Test harness pieces

| # | Harness                                            | Used by acceptance test |
|---|----------------------------------------------------|-------------------------|
| D1 | `RecordingRuntimeTap` + `RecordingHttpTap` that captures every call into a `Vec` for assertions | #1 (Observer equivalence) |
| D2 | `StallingRuntimeTap` whose `observed_step` blocks indefinitely (to validate runtime keeps making progress via the bounded channel) | #2 (Observer cannot block dispatch) |
| D3 | `PanickingRuntimeTap` whose `observed_step` panics (to validate the trait-boundary catch and counter increment) | #2 |
| D4 | A simple `tokio::sync::Mutex`-backed multi-core driver that can `pause(core)`, `step(core)`, and inject events into specific cores | #3 (Per-core pause isolation) |
| D5 | An `httpmock` test server that observes whether the second MAM rate-limited request actually arrives, plus a way to release the gate after assertion | #4 (HTTP gate prevents send) |
| D6 | A small `RingFiller` utility that pushes synthetic step records into a `StepRing` until eviction begins, plus an SSE consumer that records every `Evicted` message | #5 (Ring eviction cleans indices) |
| D7 | A `Serialize`-asserting wrapper that runs every `windlass-types` configuration / capture struct through `serde_json::to_value` and pattern-matches the result against expected redaction shapes | #6 (Secret behavior) |
| D8 | A fanout-bridge harness that proves the publish-ID preservation chain: core A emits `PublishEnvelope { id: X, .. }` → `TopicFanout` forwards → bridge subscriber → core B receives `Timed::from_publish(now, X, event)`. Without this the causal-graph UI may have IDs everywhere and still fail the cross-core jump. | #1 (extends Observer equivalence) |

Most of these are small (~50 LOC each). D5's `httpmock` test server
likely already exists in some form for the existing integration
tests — confirm and reuse rather than rebuild.

---

## E. Sign-off — locked 2026-06-01

### B-items (genuine decisions) — all locked

- [x] **B1** `CoreId` closed enum, lowercase wire tokens, fail-loud
      on unknown wire value. `Display`, `FromStr`, `Serialize`,
      `Deserialize`, `serde(rename_all = "lowercase")`.
- [x] **B2** `StepKind = Event | Command { response: CommandResponseStatus }`;
      `CommandResponseStatus = Sent | ReceiverDropped`.
- [x] **B3** `BodyKind = Json | Text | Form | Binary`; binary
      truncates to length-only (`BodyCapture::Bytes`).
- [x] **B4** `erased_serde` dropped. Trait inputs use
      `&serde_json::Value` throughout. `reserve_step_ids` takes
      `&[Uuid]` only (no payload at all).
- [x] **B5** Hand-rolled `Serialize` impl on `ServerSecretSlot`
      emits `WireRedacted`. Reveal endpoint reads `slot.cleartext`
      via direct field access, never via `Serialize`.
- [x] **B6** Scan step rings + HTTP ring for `reveal_id`.
      No separate `reveal_id → CleartextSlot` index on the
      controller. Missing → `410 Gone`.
- [x] **B7** Compile-time constants `STEP_RECORD_CHANNEL_SIZE = 4096`
      and `HTTP_EXCHANGE_CHANNEL_SIZE = 1024`, documented adjacent
      to the observability config defaults. Overflow visible via
      `Loss` SSE counters; promote to config only on real
      production pressure.
- [x] **B8** `PAUSE_ON_START` parsing: case-insensitive,
      `true`/`all` mean all cores, lowercase token match against
      `CoreId`, unknown token → fatal at startup.
- [x] **B9** SSE envelope locked: `Hello(HelloSnapshot)`, `Step`,
      `HttpExchange`, `Log`, `CoreStatus`, `Evicted(EvictedIds)`,
      `Loss(LossCounters)`. `HelloSnapshot` carries ring contents
      + core statuses + loss counters + active breakpoints.

### C-items (migration scope) — all acknowledged

- [x] **C1..C11** acknowledged. C11 is the ID-only
      `reserve_step_ids` signature (consequence of B4). The
      cross-core publish-ID preservation rule (qualifying C2),
      the HTTP request capture pre-build rule (qualifying C8), and
      the apply-order preservation rule (qualifying C9) are all
      part of the lock — see narrative below the C table.

### D-items (test harness) — all acknowledged

- [x] **D1..D8** acknowledged. D8 is the fanout-bridge harness for
      publish-ID preservation, without which the causal graph could
      look right while silently breaking cross-core jumps.

### Hand-off

§37pre is done. The redesign doc cross-references this file as the
canonical contract. §37a (`secrecy` adoption) and §37b
(`Machine::state_snapshot`) may begin in parallel. Every other §37
story proceeds in the dependency order named in the redesign doc.
