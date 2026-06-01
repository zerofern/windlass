# §37pre — Observability Contracts Sign-Off Checklist

This is the work artifact for §37pre. It walks every contract the
implementation stories need to lock and surfaces the ambiguities the
redesign doc gestures at without resolving.

**When this is signed off, §37a and §37b can start in parallel.**
The implementation stories reference this checklist as the spec —
they do not re-open the decisions below.

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
defined. Propose:

```rust
pub enum StepKind {
    Event,                       // handle(timed_event)
    Command { response_sent: bool },  // handle_command(cmd)
}
```

`Command::response_sent` tells the UI whether the command's typed
response made it back to the caller (false = receiver dropped). This
is the only step kind that has a synchronous response, so it gets
that extra bit.

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
~1000 records total, an O(N) scan is microseconds. **Proposal: B6b
(scan rings).** Simpler; the saved memory matters more than the
saved lookup time.

### B7 — Internal channel sizes for runtime → controller

EC-1 and EC-5 say `observed_*` writes to a bounded internal channel
that drops on overflow. Channel sizes weren't named. Propose:

```rust
const RUNTIME_TO_CONTROLLER_BUFFER: usize = 4096;  // events, actions, publishes
const HTTP_TO_CONTROLLER_BUFFER: usize = 1024;     // HTTP exchanges (slower path)
```

Both surfaces drop-oldest with the corresponding counter advancing
on overflow. Tunable from `windlass.toml` (add to schema) or left as
constants. **Proposal: leave as compile-time constants for v1;
promote to config the first time we actually hit overflow in
production.**

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
wire envelope. Propose:

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SseMessage {
    /// Sent once on connect, before any other messages.
    Hello {
        protocol_version: u32,                            // start at 1
        snapshot: ObservabilitySnapshot,                  // ring contents at connect time
    },
    /// New step record committed by `observed_step`.
    Step(StoredStepRecord),
    /// New HTTP exchange committed by `observed_exchange`.
    HttpExchange(StoredHttpExchange),
    /// New log line.
    Log(StoredLogEntry),
    /// Per-core status transition.
    CoreStatus { core: CoreId, status: CoreStatus },
    /// IDs that have just left rings (and reveal slots that just expired).
    Evicted {
        step_ids: Vec<Uuid>,
        action_ids: Vec<Uuid>,
        publish_ids: Vec<Uuid>,
        reveal_ids: Vec<Uuid>,
    },
    /// Loss/truncation counter snapshot. Emitted on change, debounced.
    Loss {
        per_core: HashMap<CoreId, CoreCounters>,
        http: HttpCounters,
    },
}
```

The `Hello` message lets a freshly connected client receive the
current ring contents in one shot rather than reconstructing
history from `Evicted` deltas it never saw.

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

Most of these are small (~50 LOC each). D5's `httpmock` test server
likely already exists in some form for the existing integration
tests — confirm and reuse rather than rebuild.

---

## E. Sign-off

§37pre is **complete** when every line below has an explicit "Yes"
from the user, after which §37a (`secrecy` adoption) and §37b
(`Machine::state_snapshot`) start in parallel.

### B-items (genuine decisions)

- [ ] **B1** `CoreId` enum as proposed (`Vpn | Qbit | Mam | Db | Disk | Docker | Domain`, lowercased on wire).
- [ ] **B2** `StepKind` as proposed (`Event | Command { response_sent: bool }`).
- [ ] **B3** `BodyKind` as proposed (`Json | Text | Form | Binary`; binary truncates to length-only).
- [ ] **B4** Drop `erased_serde`; use `&serde_json::Value` for trait inputs.
- [ ] **B5** Hand-rolled `Serialize` impl on `ServerSecretSlot` emits `WireRedacted`.
- [ ] **B6** Scan rings for `reveal_id` lookup (no separate index).
- [ ] **B7** Internal channel sizes as compile-time constants (4096 + 1024); promote to config later if needed.
- [ ] **B8** `PAUSE_ON_START` parsing as specified (case-insensitive, `all` alias, lowercase token match, unknown → fatal).
- [ ] **B9** SSE message envelope as proposed (`Hello`/`Step`/`HttpExchange`/`Log`/`CoreStatus`/`Evicted`/`Loss`, tagged `kind`).

### C-items (migration scope)

- [ ] **C1..C10** all acknowledged. (One toggle — these are the
      mechanical changes the implementation stories perform.)

### D-items (test harness)

- [ ] **D1..D7** all acknowledged as in-scope build-work for the
      implementation stories that own each acceptance test.

When all three groups are signed off, this file gets a header line
`Status: locked YYYY-MM-DD`, the redesign doc gets a one-line
pointer to it as the canonical contract, and §37a + §37b kick off.
