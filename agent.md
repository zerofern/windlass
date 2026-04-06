# Windlass — Agent Context

Windlass is a Rust operator that manages a Gluetun VPN stack: it syncs the
forwarded port to qBittorrent, updates the MAM seedbox record, monitors
container health, and sends Gotify alerts.

## Architecture: Sans I/O / Functional Core Imperative Shell

The entire project follows the **Functional Core, Imperative Shell** pattern.

```
shell/ (async, I/O, side effects)
  ↓ Event
core/ (pure, no I/O, no async)
  ↓ Vec<Action>
shell/ (executes actions)
```

- **`src/core/mod.rs`** — pure `process_event(state, event) -> (state, actions)`.
  No I/O, no async, no side effects. All business logic lives here.
- **`src/shell/mod.rs`** — async event loop. Executes `Vec<Action>`, sends
  `Event`s back via an mpsc channel. No decisions, only translations.
- **`src/core/events.rs`** / **`src/core/actions.rs`** — the boundary types.
- **`src/core/types.rs`** — domain types (`VpnState`, `QbitState`, `MamState`,
  `SystemState`).

### The rule

If code makes a decision ("should I retry?", "is this a new value?"), it belongs
in `core/`. If code talks to the OS, network, or Docker, it belongs in `shell/`.

## Design decisions — always ask

The owner wants to be included in design decisions. When you spot an
improvement that goes beyond the literal requirements — even a small one —
**ask before deciding to skip it**.

The pattern to avoid:

> "This would be cleaner but it's beyond the scope of what's described,
> so I'll leave it for now."

The pattern to use instead:

> "While implementing X, I noticed Y would be cleaner (e.g. moving
> `cached_cookie` into `QbitClient` as `Arc<Mutex<Option<AuthCookie>>>`).
> It's a small extra change — want me to do it now or save it for later?"

This applies even when the current requirements are explicit. The owner's
answer may well be "yes, do it." Never silently skip an improvement you
have spotted — surface it as a question.

## File size

Keep source files small. LLMs and humans both work better with focused,
short files. The rough guide:

- **Hard limit: 300 lines.** If a file approaches this, it must be split.
- **Target: under 200 lines.** Aim for this on new files.
- Each file should have a single clear responsibility. If you find yourself
  writing a second `impl` block on a different concept, that concept belongs
  in its own file.
- Tests live in `#[cfg(test)] mod tests` inside the same file for unit tests,
  but large integration test suites should be in `tests/`.
- When splitting, prefer splitting by concept (e.g. one file per HTTP client)
  over splitting by layer.

## Newtypes

Windlass uses newtypes extensively to make invalid states unrepresentable and
to prevent primitive obsession (passing a raw `u16` where an `Ipv4Addr` is
expected, etc.). All domain values crossing the core/shell boundary must be
wrapped in their newtype.

### Requirements when adding a new type

1. **Wrap all domain values — no raw primitives anywhere.** A `u16` is a port
   number, an index, a count, or a year — the type system cannot tell. Every
   domain concept must be its own newtype, even if it only ever lives inside
   the core or only inside the shell. This applies to values that cross the
   core/shell boundary and to those that do not.

2. **Use exsiting types before making oure own** We use existeng rust types
   before we make our own type. secrecy and std::path are good exampels of this.

3. **Validated types use `nutype`.** Use the `nutype` crate when the primitive
   has invariants (e.g. port must be 1–65535). `nutype` makes the inner field
   private and enforces validation at construction time via `try_new()`.
   Do not add `pub` to a nutype field or bypass it with `unsafe`.

4. **Unvalidated wrappers use plain tuple structs.** When wrapping for
   type-safety without validation (e.g. `ContainerId(String)`), a plain
   `#[derive(...)] pub struct Foo(pub T)` is fine.

5. **Secrets use `secrecy`.** Any type that wraps a password, key, or session
   token must wrap `SecretString` (from the `secrecy` crate) so it is
   redacted in logs and not accidentally cloned into plain memory.
   Construction: `SecretString::new(value.into())` (`Box<str>`, not `String`).

6. **Derive `PartialEq + Eq` on everything in `types.rs`.** The core tests
   and prop tests rely on equality. `nutype` types need these listed explicitly
   in `derive(...)`.

### Existing types

All primitive domain types live in `src/types.rs`. Before adding a new type,
read that file to see what already exists.

## Clippy

The project runs `cargo clippy -- -W clippy::pedantic -W clippy::nursery` with
**no suppressed rules** in the justfile. The goal is zero warnings.

### Rules for `#[allow(...)]`

- Every `#[allow(clippy::...)]` in the source code **must** be accompanied by
  an inline comment explaining why the suppression is justified.
- A lint can only be added to the justfile's allow-list with explicit user
  approval. Do not add `-A clippy::*` flags to silence warnings you cannot fix.
- When clippy flags a warning, fix the code — don't suppress the lint.

### The one current exception

`ShellContext::new` in `shell/mod.rs` carries `#[allow(clippy::too_many_arguments)]`
because it is a constructor for a context struct that genuinely needs all those
fields. The `#[allow]` is accompanied by a comment explaining why.

## Web layer communication

The axum web server (`windlass-web`) is **just another event producer** — it has no
special status over the Docker watcher or file watcher. It never calls `process_event`
directly and never reads the core internals.

Communication goes through `AppState`, which is an `Arc`-wrapped struct shared across
all axum handlers:

```rust
pub struct AppState {
    pub event_tx: mpsc::Sender<Event>,           // inject events INTO the loop
    pub state: Arc<RwLock<SystemState>>,          // read current state OUT
    pub observations: broadcast::Sender<Observation>, // stream of everything happening
    pub debug_gate: Arc<DebugGate>,
}
```

**Inbound (browser → core):**
```
POST /api/v1/operator/reset
  → axum handler clones event_tx
  → event_tx.send(Event::ManualReset).await
  → main event loop receives it, calls process_event normally
```

**Outbound (core → browser):**
```
process_event() runs in the main loop
  → broadcasts EventReceived, StateSnapshot, ActionDispatched
  → SSE handler streams these to all connected browser clients
```

**The rule:** if a web handler needs to cause something to happen, it sends an `Event`.
It never directly calls shell functions. The effect flows back to the browser via the
SSE broadcast channel, not via the HTTP response body (which returns `202 Accepted`
immediately).

## Build & test

A `justfile` is at the repo root — always use `just` rather than bare `cargo`
commands. Run `just` with no arguments to see all recipes.

### Before every commit

Run `just check && just coverage`. Both must pass with zero warnings, zero test
failures, and no coverage regression.

### Test tiers

| Tier | What                      | Gate                              |
| ---- | ------------------------- | --------------------------------- |
| 1    | Pure unit tests (no I/O)  | always                            |
| 2    | Mock HTTP via wiremock    | always                            |
| 3    | Real filesystem (tempdir) | always                            |
| 4    | Real Docker containers    | `#[ignore]` — needs Docker socket |

### Coverage goal

Target 100% on all code testable without a live Docker socket. Tiers 1–3 must
cover everything in `core/` and the pure parts of `shell/`. When adding new
logic, add tests alongside it.

## Key invariants — do not break these

1. **`core/mod.rs` must stay pure.** No `use std::fs`, no `tokio`, no `reqwest`,
   no `bollard`. If you need to check this: `grep -n "tokio\|reqwest\|bollard\|std::fs" src/core/mod.rs` should return nothing.

2. **Guards prevent stale-event cascades.** Three explicit guards exist:
   - `PortFileReadResult(Ok)` — no-op if ip+port match current `VpnState::Connected`
   - `QbitConnectionRefused` — ignored if qbit is not `Authenticating`
   - `Wakeup(QbitAuthRetry)` — ignored if qbit is not `Authenticating`
     Do not remove these without adding equivalent protection.

3. **File watcher sends one event per content change.** `spawn_file_watcher` uses
   `notify-debouncer-mini` (100 ms window) + a capacity-1 `try_send` + content
   deduplication (`last_sent`). This prevents the inotify feedback loop where
   `read_port_files` itself triggers new inotify events. Do not change this to
   `blocking_send` or increase channel capacity without re-testing call counts.

4. **Core owns all retry/backoff.** The shell never sleeps-and-retries. If a
   shell operation fails, it sends an `Event::*Err` and the core decides
   whether to `ScheduleWakeup` for a retry.

## Important gotchas

### bollard 0.18

- No builder pattern: use `ListContainersOptions::<String> { all: true, ..Default::default() }`
- `StartContainerOptions` needs explicit type: `None::<bollard::container::StartContainerOptions<String>>`
- `discover_dependents_for` resolves the anchor's container ID via
  `inspect_container` because `docker-compose` stores `container:<name>` while
  plain `docker run` stores `container:<id>`.

### nutype

- `VpnPort` is a nutype newtype. Its inner field is private — use
  `VpnPort::try_new(n).unwrap()` to construct, not `VpnPort(n)`.

### secrecy 0.10

- `SecretString::new()` takes `Box<str>`, not `String`. Call `.into()`.

### WireMock 3.5.4 (integration tests)

- Health endpoint returns `{"status": "healthy"}`, not `"UP"`.
- No `curl` in the WireMock image — use `wget`.

## Configuration (`src/shell/config.rs`)

All config comes from environment variables with defaults. Key fields for
testing: `MAM_SEEDBOX_URL`, `MAM_LOAD_URL` (override MAM endpoints),
`GLUETUN_PROXY_URL` (optional — absent means no VPN proxy, used in tests).

## File layout

```
src/core/       ← pure state machine (events, actions, types, tests)
src/shell/      ← async I/O (Docker, HTTP clients, config, event loop)
src/types.rs    ← shared primitive newtypes
src/main.rs     ← entry point
windlass/tests/ ← Rust integration tests (ignored by default, require dev stack)
windlass-testkit/ ← chaos controller and gluetun mock binary
```

## Integration tests

Before marking any feature done that touches the shell, external API clients,
the event loop, or Docker integration: run `just integration` and confirm all
tests pass. The integration tests bring up the full mock stack, run scenarios,
and tear down automatically.

Integration tests live in `windlass/tests/integration.rs`. They are `#[ignore]`
by default so `cargo test` doesn't require Docker. `just integration` runs them
with `--ignored`.

When adding new external API behaviour, add a corresponding scenario to
`windlass-testkit/src/scenarios.rs` and a test to `windlass/tests/integration.rs`.
