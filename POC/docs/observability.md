# Windlass Observability

## Purpose

The `/observability` page is a debugger-like surface over every running
core (Vpn, Qbit, Mam, Db, Disk, Docker, Domain).  It is **always on**
in development and production — there is no enable/disable toggle for
the underlying capture pipeline.

**In development:** step through edge cases without the system racing
ahead; inspect the exact HTTP exchange a core is about to issue;
verify the action / publish sequence each event produces.

**In production:** operate with confidence over rate-sensitive
external services (especially MAM); pause a single core to inspect
state without freezing the rest of the system; jump to a specific
event / action / publish / HTTP URL with breakpoints.

## Core principles

- **Transparent.** Observation never modifies events, actions, state,
  or HTTP requests.  Capture sites are read-only; the `gate_*`
  methods are the only places the tap may park.  (EC-2, EC-6.)
- **Per-core.** Pause / resume / step is per-core.  Pausing MAM does
  not stall Qbit.  (Acceptance test #3.)
- **Bounded.** Always-on capture must never backpressure the runtime.
  Internal write channels are bounded (`STEP_RECORD_CHANNEL_SIZE =
  4096`, `HTTP_EXCHANGE_CHANNEL_SIZE = 1024`); overflow surfaces as
  per-core drop counters in the UI.  (EC-1 + EC-5.)
- **Secret-safe.** Secret-bearing fields land server-side as
  `ServerSecretSlot { cleartext, reveal_id }` and serialize as
  `WireRedacted { redacted: true, reveal_id }`.  Cleartext is exposed
  only through the dedicated reveal endpoint, on explicit operator
  click.  (Decision 14 + B5 + B6.)

## Three ways to enter "paused" state

### 1. `PAUSE_ON_START` environment variable

```
PAUSE_ON_START=true           # all seven cores pre-paused at startup
PAUSE_ON_START=mam,qbit       # only the listed cores pre-paused
PAUSE_ON_START unset          # default: all cores running
```

Read once at startup.  Cores that should start paused are paused
before any runtime spawns, so no event is processed until the
operator releases them.  Use for cold-start inspection, new
environments, or any scenario where you want to verify the initial
action sequence before any external service is contacted.

### 2. Web-UI pause buttons

The cores rail on `/observability` shows the current `CoreStatus` for
each core and exposes per-core Pause / Resume / Step buttons plus
"Step All" / "Pause All" / "Resume All".  A click hits the
controller's REST surface (see API reference) and the affected core
parks at the next gate.

### 3. MAM rate-limit guardrail (automatic)

If the MAM client detects that two requests would violate the
minimum interval, the `HttpTap::signal_anomaly` call flips the
per-core pause flag for `mam`.  The next `gate_request` parks
*before* `client.execute(req)` runs — the offending request is
never sent.  (P7 + acceptance test #4.)

## What the operator sees

The `/observability` page renders:

- **Cores rail.**  One entry per core, showing its current
  `CoreStatus` (`Running`, `PauseRequested`, `ParkedAtEvent`,
  `ParkedAtOutcome`, `ParkedAtHttp`, `Stepping`).  Each parked
  variant carries useful context: the event variant + payload
  preview, the action/publish variants the source event produced,
  or the method + URL + request-body preview about to be sent.
- **Per-core step records.**  One row per `Machine::handle` /
  `handle_command` invocation, expandable to show the event payload,
  every action + publish produced, the state snapshot after, and
  causal links to the originating step (clickable `action_id` /
  `publish_id`).
- **Cross-core HTTP exchange ring.**  One row per captured exchange
  (qBit, MAM); request preview + response body + status + duration.
  Each row joins back to its originating step via `action_id`.
- **Active breakpoints.**  All event / action / publish / HTTP-URL
  breakpoints currently armed.
- **Loss counters.**  Per-core `dropped_steps`, cross-core
  `dropped_exchanges`, `truncated_request_bodies`,
  `truncated_response_bodies`.  Non-zero values surface as a banner.
- **Logs.**  Captured `tracing` events that occurred in the
  observability window, attached to their originating step where
  possible.

## Breakpoints

Breakpoints work independently of full pause.  Arm a breakpoint by
naming a specific variant or URL substring:

- **Event variant.**  E.g. `QbitAuthFailed`.  The next event of that
  variant arrives at the event gate; the core parks before `handle`
  runs.
- **Action variant.**  E.g. `UpdateMam`.  The core parks at the
  outcome gate after `handle` produces the action, before apply
  dispatches it.
- **Publish variant.**  Same path as action breakpoints — outcome
  gate parks when the variant appears.
- **HTTP URL pattern.**  Substring-matched against the outgoing URL.
  The HTTP gate parks before `client.execute` runs.

Breakpoints survive pause/resume — they remain armed until explicitly
removed.

## Reveal secrets

Each redacted field on the wire carries a `reveal_id`.  Clicking
**Reveal** posts to
`POST /api/v1/observability/reveal/{reveal_id}`:

- On hit, the response body is the cleartext for that one field of
  that one record.  The UI keeps the result in memory for the current
  page session only.
- On miss (record evicted from the ring, or unknown id), the response
  is `410 Gone`.  Reveal IDs are unguessable UUIDv4s; ring eviction
  invalidates them naturally.  (EC-3.)

## Configuration

All knobs are environment variables.  Byte budgets accept IEC binary
suffixes (`KiB`, `MiB`).  Each is optional; defaults match the §37pre
B7 lock.

| Variable | Default | Purpose |
| --- | --- | --- |
| `PAUSE_ON_START` | unset | Pre-pause selected cores (or `true` for all). |
| `WINDLASS_OBS_STEP_RECORDS_PER_CORE` | `500` | Per-core step ring count budget. |
| `WINDLASS_OBS_STEP_RECORD_BYTES_PER_CORE` | `4MiB` | Per-core step ring byte budget. |
| `WINDLASS_OBS_HTTP_EXCHANGES` | `500` | Cross-core HTTP ring count budget. |
| `WINDLASS_OBS_HTTP_EXCHANGE_BYTES_TOTAL` | `8MiB` | Cross-core HTTP ring byte budget. |
| `WINDLASS_OBS_MAX_REQUEST_BODY_BYTES` | `64KiB` | Request-body capture cap (truncate above). |
| `WINDLASS_OBS_MAX_RESPONSE_BODY_BYTES` | `256KiB` | Response-body capture cap. |

Rings enforce both the count budget *and* the byte budget; whichever
is reached first triggers eviction (drop-oldest with an `Evicted` SSE
message).

## API reference

| Method   | Path                                                            | Description                                          |
| -------- | --------------------------------------------------------------- | ---------------------------------------------------- |
| `GET`    | `/api/v1/observability/stream`                                  | SSE stream of every captured record + status change. |
| `POST`   | `/api/v1/observability/pause/{core}`                            | Pause a single core.                                 |
| `POST`   | `/api/v1/observability/pause_all`                               | Pause every core.                                    |
| `POST`   | `/api/v1/observability/resume/{core}`                           | Resume a single core.                                |
| `POST`   | `/api/v1/observability/resume_all`                              | Resume every core.                                   |
| `POST`   | `/api/v1/observability/step/{core}`                             | Release one permit for a single core.                |
| `POST`   | `/api/v1/observability/step_all`                                | Release one permit for every paused core.            |
| `GET`    | `/api/v1/observability/breakpoints`                             | List active breakpoints.                             |
| `POST`   | `/api/v1/observability/breakpoints/{kind}/{value}`              | Add a breakpoint (kind: event / action / publish / http). |
| `DELETE` | `/api/v1/observability/breakpoints/{kind}/{value}`              | Remove a breakpoint.                                 |
| `POST`   | `/api/v1/observability/reveal/{reveal_id}`                      | Reveal cleartext for one secret slot (`410 Gone` on miss). |

`{core}` is one of `vpn`, `qbit`, `mam`, `db`, `disk`, `docker`,
`domain`.  Unknown tokens return `404 Not Found`.

## SSE envelope

The `/stream` endpoint emits one `observability` event per message,
with JSON-encoded payloads of:

- `Hello(HelloSnapshot)` — initial snapshot for newly-attached
  subscribers (ring contents + core statuses + loss counters +
  active breakpoints).
- `Step(StoredStepRecord)` — one fresh step record.
- `HttpExchange(StoredHttpExchange)` — one fresh HTTP exchange.
- `Log(StoredLogLine)` — one fresh log line.
- `CoreStatus { core, status }` — a core's status changed.
- `Evicted(EvictedIds)` — records left a ring; UI drops their
  causal links and any revealed-secret state.
- `Loss(LossCounters)` — drop / truncate counters advanced.

## See also

- `docs/observability-redesign.md` — the locked design document.
- `docs/observability-37pre-checklist.md` — the §37pre engineering
  contracts + acceptance-test catalog.
