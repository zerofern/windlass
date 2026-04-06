# Windlass Web UI

The embedded control plane for the Windlass operator. Served directly from the binary
via axum and rust-embed. Designed for local/LAN/Tailscale access — no authentication.

This document covers the web UI architecture and build plan. The UI is designed to
grow into a full audiobook management control plane (queue management, AI curation,
series tracking, ratings) — architectural decisions reflect that larger scope even
where initial implementation is minimal.

---

## Foundational decisions (hard to change later)

These must be in place before Tier 1 is built.

### 1. Cargo workspace

The project becomes a workspace now. Splitting later is painful.

```
windlass/                   workspace root
  windlass-types/           shared types: Event, Action, SystemState, Observation,
                            alert models, torrent models, API request/response types
  windlass-core/            pure state machine (current src/core/)
  windlass-local/           local system ops: DockerClient, vpn_files, monitors
                            (current src/shell/docker.rs, vpn_files.rs, monitors.rs)
  windlass-clients/         outbound HTTP clients: QbitClient, MamClient, GotifyClient
                            modules within; future: ABS, Audnexus, LLM
                            (current src/shell/qbit.rs, mam.rs, gotify.rs)
  windlass-web/             axum server, SSE, API route handlers, rust-embed of frontend
  windlass/                 binary — config, ShellContext, event loop, wires everything

  web/                      React + Vite + shadcn/ui SPA (not a Cargo crate)
    src/
    dist/                   built output, embedded into windlass-web at compile time
    package.json
```

Dependency graph (each crate only sees what it needs):

```
windlass-types   ← no deps (serde only)
windlass-core    ← windlass-types
windlass-local   ← windlass-types, bollard, notify-debouncer-mini
windlass-clients ← windlass-types, reqwest
windlass-web     ← windlass-types  (not core — just needs types for JSON)
windlass         ← all of the above
```

### 2. API versioning

All API routes under `/api/v1/`. Route handlers are modular axum sub-routers composed
in `windlass-web`. Adding `/api/v2/` later or adding new feature areas
(e.g. `/api/v1/library/`, `/api/v1/ai/`) is additive and non-breaking.

### 3. Alert IDs and Gotify deep links

A nice-to-have, not a hard architectural constraint — can be added when the alerts
history page is built (and when a database is introduced).

When it is implemented: every alert is persisted before being sent to Gotify.
The notification body includes `https://<WINDLASS_HOST>/alerts/{id}`. Tapping it on
your phone opens the alert detail page showing the event that triggered it and the
system state at that moment — useful hours after the fact when the system has moved on.

Until then, Gotify notifications can omit the URL or link to `/`.

## Protocol

**REST** for reads and commands. **SSE** for all live streams.

SSE is chosen over WebSocket because it works through any HTTP reverse proxy (nginx,
Tailscale funnel) without `Upgrade` header configuration, and maps naturally to a
"server pushes observations" model.

Web server: **axum**.

---

## Frontend

**React + Vite + shadcn/ui + React Router + TypeScript**, compiled to a static SPA,
embedded in the `windlass-web` crate at compile time via `rust-embed` and served
under `/`. API calls go to `/api/v1/`.

**Hard requirements:**
- Fully functional on **mobile** (touch-first interactions, responsive layout)
- Fully functional on **desktop** (keyboard-friendly, dense information layout where appropriate)

shadcn/ui + Tailwind CSS handles responsive layout; `@dnd-kit/core` handles touch-first
drag-and-drop for the queue page.

**Stack rationale:**
- **Vite** — fast builds, HMR during development, outputs a static bundle suitable for embedding
- **shadcn/ui** — components copied into the codebase (not a dependency), built on Radix UI primitives (accessible, mobile-first) + Tailwind CSS; fully customizable, no version lock-in
- **React Router v7** — client-side routing; the route table maps directly to the planned pages
- **TypeScript** — type safety; API response types are written once in TypeScript to match the Rust structs
- **`@dnd-kit/core`** — drag-and-drop queue management; built touch-first, the standard choice for React

For SSE the browser's native `EventSource` API is used directly — no library needed.

In development, Vite proxies `/api/` to the running Rust backend.

**Planned routes:**

| Route | Feature |
|-------|---------|
| `/` | Dashboard — operator state, reset button |
| `/log` | Live event/action stream |
| `/debug` | Stepper, breakpoints |
| `/queue` | Download queue, drag-and-drop prioritisation |
| `/library` | Ratings, series, ABS sync status |
| `/alerts` | Alert history |
| `/alerts/:id` | Alert detail page (Gotify deep link target) |
| `/ai` | AI recommendations, approve/reject |
| `/calendar` | Upcoming series releases |
| `/settings` | Configuration, custom format rules |

---

## Axum router structure

```rust
// windlass-web/src/lib.rs

pub fn router(state: AppState) -> Router {
    Router::new()
        .nest("/api/v1/operator", operator::router())
        .nest("/api/v1/alerts",   alerts::router())
        .nest("/api/v1/queue",    queue::router())
        .nest("/api/v1/library",  library::router())
        .nest("/api/v1/stream",   stream::router())
        .nest("/api/v1/debug",    debug::router())
        .fallback(frontend::handler)     // serves Leptos SPA for all other paths
        .with_state(state)
}
```

Each sub-module owns its route handlers. New feature areas are added by adding a
`.nest()` call — no changes to existing handlers.

`AppState` is an `Arc`-wrapped struct shared across all handlers:

```rust
pub struct AppState {
    pub event_tx: mpsc::Sender<Event>,
    pub state: Arc<RwLock<SystemState>>,
    pub observations: broadcast::Sender<Observation>,
    pub debug_gate: Arc<DebugGate>,
}
```

---

## Tiers

### Tier 1 — State & Control

Endpoints:

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/operator/state` | Current `SystemState` as JSON |
| `POST` | `/api/v1/operator/reset` | Injects `Event::ManualReset` |
| `GET`  | `/api/v1/alerts` | Paginated alert history from DB |
| `GET`  | `/api/v1/alerts/{id}` | Alert detail — message, event context, state snapshot |
| `GET`  | `/api/v1/health` | Liveness probe |

**Rust changes:**
- Extract project to workspace.
- Add `axum` and `rust-embed` dependencies.
- Spawn axum server task in the binary's `main`.
- `AppState` with `event_tx` and `Arc<RwLock<SystemState>>`.

**Frontend:** Dashboard (state panel, reset button), Alerts list, Alert detail page.

---

### Tier 2 — Live Event/Action Stream

Endpoint:

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/stream` | SSE stream of `Observation` messages |

```rust
pub enum Observation {
    StateSnapshot(SystemState),
    EventReceived(Event),
    ActionDispatched(Action),
    // Extended in Tier 3:
    HttpExchange { module: String, request: String, response: String, status: u16 },
}
```

On connect, the server immediately sends a `StateSnapshot` so the client is not blank.
Multiple clients can subscribe simultaneously. Slow clients are dropped on buffer
overflow and reconnect to receive a fresh snapshot.

**Rust changes:**
- `tokio::sync::broadcast::Sender<Observation>` (capacity 256) in `AppState`.
- Main event loop taps it at three points:
  - Before `process_event` → `EventReceived`
  - After `process_event` → `StateSnapshot` + `ActionDispatched` for each action
- SSE handler subscribes a receiver per client.

**Frontend:** Live log view — scrolling, filterable by event/action type, expandable JSON.

---

### Tier 3 — Debug Mode

Debug mode is **dynamically togglable** from the UI. An env var `DEBUG_MODE_ON_START=true`
enables it at startup.

**Endpoints:**

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/debug` | Current debug state (enabled, queue, pending actions, breakpoints) |
| `POST` | `/api/v1/debug/enable` | Enable debug mode |
| `POST` | `/api/v1/debug/disable` | Disable debug mode; flush queue normally |
| `GET`  | `/api/v1/debug/events` | All `Event` variant names (for breakpoint UI) |
| `GET`  | `/api/v1/debug/actions` | All `Action` variant names (for breakpoint UI) |
| `POST` | `/api/v1/debug/breakpoints/event/{variant}` | Add event breakpoint |
| `DELETE` | `/api/v1/debug/breakpoints/event/{variant}` | Remove event breakpoint |
| `POST` | `/api/v1/debug/breakpoints/action/{variant}` | Add action breakpoint |
| `DELETE` | `/api/v1/debug/breakpoints/action/{variant}` | Remove action breakpoint |
| `POST` | `/api/v1/debug/step/event` | Process next queued event through the Core |
| `POST` | `/api/v1/debug/step/action` | Dispatch next pending action through the Shell |

The loop pauses on an event if: debug mode is enabled, **or** the event's variant name
is in the breakpoints set. Same logic for actions. This means breakpoints work
independently of full debug mode — set a breakpoint on `QbitAuthFailed` and the
system runs normally until that specific event arrives.

**Shell HTTP logging:** `QbitClient`, `MamClient`, `GotifyClient` each hold an
`Option<broadcast::Sender<Observation>>` — `None` in normal mode (zero overhead).
When debug mode is active, HTTP request/response details are forwarded to the SSE
stream as `HttpExchange` observations.

**Frontend:** Debugger — queued event display, "Step Event" button, action queue with
per-action "Step" controls, request/response detail, breakpoint editor (searchable
list of all event/action variant names with toggle switches).

---

## Pre-requisite refactor: Core cleanup

Before the HTTP client refactor, clean up the core state machine:

### New types

- **`TorrentName(pub String)`** — replaces raw `String` in `known_torrents` and
  `NewTorrentsObserved`. Plain tuple struct, no validation needed.
- **`HttpStatusCode(pub u16)`** — replaces raw `u16` in `QbitPortSyncFailed` and
  `QbitApiError`. Makes intent clear at call sites.
- **`MamStatus`** enum — replaces `bool` in `MamConnectabilityObserved`:
  ```rust
  pub enum MamStatus {
      Connectable,     // MAM reached, qBit listed as connectable
      NotConnectable,  // MAM reached, qBit not connectable (port forward issue)
      Unreachable,     // Network failure or parse error reaching MAM
  }
  ```
  Event becomes `MamStatusObserved(MamStatus)`. Allows differentiated recovery
  logic in future (e.g. `NotConnectable` may indicate a port forward issue rather
  than a VPN problem).

### Fix `QbitApiError` gap

`QbitApiError` exists in core and tests but the shell never emits it — dead code.
Shell `authenticate` currently collapses all non-success responses to `QbitAuthFailed`.
Fix: emit `QbitAuthFailed` only when qBit returns body `"Fails."` (wrong credentials),
and `QbitApiError(HttpStatusCode)` for all other HTTP errors (transient failures).
Core's exponential backoff for `QbitApiError` becomes live code.

### `process_event` as a method

`process_event` moves from a free function to a method on `SystemState`:

```rust
impl SystemState {
    pub fn process_event(self, event: Event) -> (Self, Vec<Action>) { ... }
}
```

All call sites updated (shell event loop, all core tests).

### Handler methods

Each event branch becomes a private method on `SystemState`, pulling the large
`match` in `mod.rs` into focused handler functions. Lives in `src/core/handlers.rs`;
split by area (vpn, qbit, mam, monitoring) if it exceeds 300 lines.

---

## Pre-requisite refactor: HTTP client structs

Before Tier 1, refactor `qbit.rs`, `mam.rs`, `gotify.rs` to match the `DockerClient`
pattern. Each becomes a struct holding its `reqwest::Client`, URL, credentials, and
(for MAM) the rotating session cookie:

- **`QbitClient`** — `reqwest::Client` (direct), `base_url`, `user`, `pass`
- **`MamClient`** — `reqwest::Client` (VPN-routed), `seedbox_url`, `load_url`,
  `Arc<Mutex<String>>` for rotating session (currently floating in `run()`).
  Lives in `mam/` subdirectory (not flat `mam.rs`) to accommodate future growth.
- **`GotifyClient`** — `reqwest::Client` (direct), `url`, `token`

`ShellContext` loses `config`, `direct`, `vpn`, and `mam_session` — replaced by the
three client structs. This also provides the natural hook for attaching the optional
debug observer channel to each client.

---

## Build order

1. Core cleanup (new types, `MamStatus`, fix `QbitApiError` gap, `process_event` as method, handler methods)
2. HTTP client struct refactor (`windlass-clients` pattern)
3. Cargo workspace extraction
4. **Tier 1** — axum server, `/api/v1/operator/state`, `/api/v1/operator/reset`, basic dashboard
5. **Tier 2** — broadcast channel, SSE stream, live log
6. **Tier 3** — debug gate, breakpoints, stepper, HTTP logging
7. Beyond — queue management, library/ABS, AI pipeline, series calendar

---

## File layout (target)

```
web/                        React + Vite + shadcn/ui SPA
  src/
  dist/                     built output, embedded in binary at compile time
  package.json

crates/
  windlass-types/src/lib.rs    Event, Action, SystemState, Observation (serde derives)
  windlass-core/src/           process_event state machine (current src/core/)
  windlass-local/src/          docker.rs, vpn_files.rs, monitors.rs
  windlass-clients/src/        qbit.rs, mam/, gotify.rs (modules within)
                               mam/ is a subdirectory to accommodate future growth
                               (search, stats, ratio, HnR tracking, series lookups)
  windlass-web/src/
    lib.rs                     axum router, AppState
    operator.rs                /api/v1/operator routes
    stream.rs                  SSE handler
    debug.rs                   debug gate + step routes
    frontend.rs                rust-embed SPA handler
  windlass/src/
    main.rs                    entry point
    config.rs                  Config::from_env()
    shell.rs                   ShellContext, event loop
```
