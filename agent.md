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

## Build & test

```bash
cargo build
cargo test                          # all unit + mock HTTP + Tier 3 fs tests
cargo test -- --include-ignored     # also runs Tier 4 Docker tests (needs socket)
./tests/integration/run.sh          # full Docker Compose integration test
```

### Test tiers

| Tier | What | Gate |
|------|------|------|
| 1 | Pure unit tests (no I/O) | always |
| 2 | Mock HTTP via wiremock | always |
| 3 | Real filesystem (tempdir) | always |
| 4 | Real Docker containers | `#[ignore]` — needs Docker socket |

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
src/
  core/
    mod.rs          ← process_event state machine
    events.rs       ← Event enum
    actions.rs      ← Action enum
    types.rs        ← VpnState, QbitState, MamState, SystemState, domain types
    tests.rs        ← deterministic unit tests
    prop_tests.rs   ← proptest property tests
  shell/
    mod.rs          ← async event loop + action dispatcher
    config.rs       ← Config struct (env vars)
    docker.rs       ← bollard + inotify file watcher
    qbit.rs         ← qBittorrent HTTP client
    mam.rs          ← MAM seedbox HTTP client
    gotify.rs       ← Gotify push notification client
    monitors.rs     ← disk space check, torrent list
  main.rs           ← entry point
  types.rs          ← shared primitive types (VpnIp, VpnPort, AuthCookie, …)
tests/
  integration/
    run.sh          ← two-scenario bash test runner
    stubs/          ← WireMock stub mappings for qBit, Gotify, MAM
Dockerfile          ← multi-stage build (rust builder + debian-slim runtime)
docker-compose.test.yml ← integration test stack
```
