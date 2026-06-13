# Integration tests

The §34 (operator-readiness) integration suite verifies the **contracts
between Windlass and its external dependencies** — qBittorrent, MAM,
the WireGuard tunnel path, Docker, Postgres.  This is the only test
layer that catches
wire-format drift in services we don't own, side effects across the
trust boundary, and real-I/O wiring between cores.

For the purpose statement and architectural lock that this doc
implements, see `docs/operator-readiness.md` §34.

## Quick start

```bash
just integration
```

That recipe:

1. Brings up `docker-compose.dev.yml` (real qBit sharing Windlass's
   namespace, fake MAM, the WireGuard fixture peer, real Postgres,
   Windlass built from source with `NET_ADMIN`).
2. Brings up `docker-compose.qbit-integration.yml` (standalone real
   qBit for the client-layer suite).
3. Runs the test crates listed below in series.
4. Tears the stack(s) down at the end (even on failure, via shell
   `trap`).

To iterate on a single test against an already-running stack:

```bash
just stack-up
cargo test --test integration_contracts $TEST_NAME -- --ignored --test-threads=1 --nocapture
just stack-down
```

## What runs in `just integration`

| Test crate | Lives in | Purpose |
|---|---|---|
| `mam_drift` | `windlass-testkit/tests/` | In-process: spins up the fake-MAM router on a random port and pins every MAM response shape through `windlass-clients::MamClient`. Catches contract drift between the fake and what the client decodes. |
| `integration_contracts` | `windlass/tests/` | Live stack: each test calls `reset_stack()`, then drives a wire Windlass depends on (NAT-PMP → qBit port sync, qBit auth, MAM seedbox, DB snapshot, torrent persistence, MAM update dedup, qBit drift smoke). |
| `integration_support` | `windlass/tests/` | Live stack: smoke tests for the helpers themselves (bollard restart, magnet fixture, fake-MAM control plane, full reset). |
| `windlass-local` ignored tests | `windlass-local/src/docker_tests.rs` | Real Docker daemon: container lifecycle for the docker-core. |
| `qbit_integration` | `windlass-clients/tests/` | Standalone real qBit (separate compose): client-layer contract for the qBit API. |

## The dev stack (`docker-compose.dev.yml`)

| Service | Image | Exposes | Role |
|---|---|---|---|
| `wg-server` | `windlass-testkit/wg-server/` | `:19090` (control) | WireGuard fixture peer: fresh keys per run, exit-IP reflector + NAT-PMP responder on its tunnel address, `/control/...` plane (natpmp-port, exit-ip, reset). |
| `qbittorrent` | `windlass-clients/tests/qbit-image` | `:18080` (via windlass) | Real qBittorrent sharing Windlass's network namespace — its egress goes through `wg0` and the kill switch. Tests authenticate as `admin/adminadmin`. |
| `mock-mam` | testkit `TESTKIT_MODE=mam` | `:18082` | Fake MAM with the 8 endpoints Windlass calls + `/control/...` plane. |
| `postgres` | `postgres:16-alpine` | `:15432` | Real Postgres with the windlass schema. |
| `windlass-test` (`windlass`) | built from source | `:5010` | The system under test: `NET_ADMIN`, owns `wg0` + the nftables kill switch. Control-plane egress (Postgres, fake MAM) is allow-listed; everything else goes through the tunnel. Cadences are shortened via env (`EXIT_IP_QUERY_INTERVAL_SECS=5`, `SEEDBOX_UPDATE_MIN_INTERVAL_SECS=20`, NAT-PMP lease 15 s) so timing contracts are testable. |

The chaos controller and the WireMock-based mocks were retired in §34
PR 2; control surfaces moved to each fake's own HTTP plane.

## The WireGuard suite (`just integration-wg`)

The owned-tunnel path (`docs/vpn-ownership.md`) gets its own suite
because its external dependency is not an HTTP service but the
kernel: WireGuard netlink, `wg`/`ip`/`nft` userland, UDP NAT-PMP.
`docker-compose.wg-integration.yml` stands up:

| Service | Image | Role |
|---|---|---|
| `wg-server` | `windlass-testkit/wg-server/` (alpine + wireguard-tools + python3) | Fixture "VPN provider": fresh keys per run, server-side `wg0` at `10.2.0.1`, writes the client `wg.conf` into a shared volume, serves an exit-IP reflector (`:8080`) and a NAT-PMP responder (`:5351/udp`) on its tunnel address. |
| `wg-test-runner` | `windlass-testkit/wg-runner/` (rust + iproute2/wireguard-tools/nftables) | Runs `windlass-net/tests/wg_integration.rs` with `NET_ADMIN` in its own namespace: real `wg0`, real nftables kill switch. |

The suite drives a live `TunnelMachine` + `TunnelShell` pair through
`windlass_machine::spawn` and asserts, in one lifecycle: the kernel
handshake completes (`Up`), the dual UDP+TCP NAT-PMP grant surfaces
as `PortReady` with the fixture's port, the exit-IP query observes
the client's tunnel address, direct underlay egress is dropped by the
kill switch while the tunnel path stays open, and several
watchdog/leak-probe cycles pass without `Down`/`LeakDetected`.

Requires the host kernel's `wireguard` module (mainline since 5.6).
The runner mounts the repo and keeps its cargo registry + target dir
in named volumes, so re-runs are incremental.

## Writing a new test

Tests live in `windlass/tests/integration_contracts.rs` (one big file
keeps the imports + helper trampoline in one place).  Pattern:

```rust
mod support;

use std::time::Duration;
use support::{reset, wait_for, MAM_BASE, mam};

#[tokio::test]
#[ignore = "requires the §34 dev stack: just stack-up"]
async fn descriptive_name_for_what_this_verifies() {
    reset::reset_stack().await.expect("reset_stack");

    // Drive the stack (set fake-MAM state, drive the WG fixture
    // control plane, add a torrent, etc.).
    let fake = mam::FakeMam::new(MAM_BASE);
    fake.set_seedbox(serde_json::json!({ "msg": "Completed" }))
        .await
        .expect("set seedbox");

    // Wait for the wire effect.
    wait_for(
        "fake-mam sees /jsonLoad.php at boot",
        Duration::from_secs(30),
        || async {
            fake.journal()
                .await
                .is_ok_and(|e| e.iter().any(|x| x.path == "/jsonLoad.php"))
        },
    )
    .await;
}
```

Rules:

- **Every test starts with `reset_stack()`.**  No exceptions.  Tests
  that don't reset will flake against each other.
- **`#[ignore = "..."]` is mandatory.**  `cargo test` runs only
  unignored; the integration recipe passes `--ignored`.
- **Assert on a wire**, not on internal Windlass state.  Wires are
  fake-MAM journal, qBit web API, the WG fixture control plane,
  the Postgres tables, Windlass's `/api/v1/...` endpoints.
- **`wait_for(label, timeout, fn)`** is the standard polling helper;
  it panics with the label on timeout.
- **No `cargo test` parallelism for live-stack tests.**  Use
  `--test-threads=1`; the recipe already does.

## The harness

### `reset_stack()` (`support/reset.rs`)

Brings the stack to a known baseline:

1. Reset fake-MAM (clear journal + restore defaults).
2. Reset the WG fixture (default NAT-PMP port `43210`, no exit-IP override).
3. Delete every torrent in qBit; restore default preferences.
4. Truncate the windlass DB tables.
5. Restart the `windlass-test` container via bollard.
6. Wait for `/api/v1/health` to answer 200.

**The fake-MAM journal is preserved across step 5.**  Contract tests
like `boot_updates_mam_seedbox` need to see what Windlass called
during boot.  If a test wants a clean post-boot journal it can call
`FakeMam::reset()` itself after `reset_stack()` returns.

### Fake-MAM control surface (`support/mam.rs`)

`FakeMam` wraps the `/control/...` plane exposed by
`TESTKIT_MODE=mam`:

```rust
let fake = mam::FakeMam::new(MAM_BASE);
fake.set_seedbox(json!({ "msg": "Last change too recent", "status": 429 })).await?;
fake.set_json_load(json!({ "connectable": "no" })).await?;
fake.set_json_ip(json!({ "ip": "1.2.3.4" })).await?;
fake.set_check_cookie(403).await?;
let entries = fake.journal().await?;        // every request, in order
fake.reset().await?;                          // clear journal + restore defaults
```

The patch shape for each setter matches the testkit's `MamState`
defaults in `windlass-testkit/src/mam.rs`.  Setting a field overrides;
omitting it leaves the current value alone.  Defaults are pinned to
`docs/mam-api.md`.

### bollard helper (`support/docker.rs`)

```rust
docker::restart("windlass-test").await?;
docker::stop("windlass-qbittorrent-1", Duration::from_secs(5)).await?;
docker::start("windlass-qbittorrent-1").await?;
let info = docker::inspect("gluetun").await?;
assert!(info.is_ready());
docker::restart_and_wait_healthy("windlass-test", Duration::from_secs(45)).await?;
```

Used by `reset_stack()` to restart Windlass.  Tests that need to
exercise §35-style "qBit went away mid-flight" scenarios use it
directly.

### qBit fixture (`support/qbit.rs`)

```rust
let handle = qbit::add_magnet_torrent("contract-fixture").await?;
let hashes = qbit::list_hashes().await?;
let count = qbit::torrent_count().await?;
qbit::delete_all().await?;
```

`add_magnet_torrent(label)` builds a random 40-hex info hash and
feeds it to qBit as a magnet.  The torrent has no peers, so it sits
in `stalledDL`/`metaDL` indefinitely — exactly the state needed to
exercise the `/api/v2/torrents/info` shape without waiting for real
download activity.

**Limitation:** qBit's web API doesn't let tests write `seed_time`,
`downloaded_bytes`, or internal state fields.  Tests that need
synthetic torrent state (e.g. "30 days of seed time") stay at the
property-test layer.

## Adding a MAM endpoint

If a new MAM endpoint becomes part of the contract:

1. Document it in `docs/mam-api.md` (shape + known `msg` values).
2. Add a handler in `windlass-testkit/src/mam.rs` with a sensible
   default response.
3. Add the matching client method in `windlass-clients/src/mam/`.
4. Add a drift test in `windlass-testkit/tests/mam_drift.rs` that
   pins the response shape end-to-end through the client type.
5. If tests need to drive the response, add a setter on `FakeMam` in
   `support/mam.rs`.

## Taxonomy: contract vs drift vs behavior

| Kind | Where | Question it answers |
|---|---|---|
| **Contract** | `integration_contracts` | "Does Windlass's wire to dependency X match what we believe X promises?" |
| **Drift** | `mam_drift` + qBit drift sub-test | "Does the dependency still return what `windlass-clients`'s types decode?" — runs against the real dependency (or the fake whose defaults pin the contract). |
| **Behavior** | per-machine proptest suites | "Given event E in state S, does the machine produce the right actions/publishes?" — exhaustive `(state × event)` coverage, runs in-process, no I/O. |

If you're about to add a test, the decision is:

- Does this test require real I/O (a real socket, a real file, a real
  Docker container)?  If no → proptest.
- Does this test pin a wire format (request body, response shape,
  file format)?  If yes → drift or contract.
- Does this test exercise wiring across cores under real timing
  (e.g. "Gluetun file change → Windlass observes → qBit setPref
  hits")?  If yes → contract.

## Limitations

The harness deliberately does NOT cover:

- **Real ProtonVPN.**  The WireGuard fixture is a real wg peer with
  a synthetic NAT-PMP gateway + exit-IP reflector; the protocol
  surfaces are real, the provider behind them is not.  Provider
  quirks (lease durations, ASN changes) are covered by unit tests
  against captured shapes.
- **External notifications.**  The current notification surface is
  in-app only (`SendAlert` → `alerts` DB row → `/api/v1/alerts`).
  When Telegram / Pushover / webhook / SMTP surfaces are added, each
  one gets its own contract test against a fake delivery endpoint
  in the testkit.
- **§35 real netns invalidation.**  Fake Gluetun can't reproduce
  the network-namespace invalidation a real Gluetun restart causes.
  Integration tests verify Windlass's signal handling; the netns
  side effects stay at the docker-core proptest layer.
- **Synthetic qBit torrent states.**  See "qBit fixture" above.

## Failure-discovery log

Bugs the harness uncovered while being built (kept here as
evidence the layer earns its keep):

- **qBit `TorrentStateRecord::Unknown(_)`** wrote the raw qBit
  state string to Postgres, violating the `torrents_state_valid`
  CHECK constraint.  Every freshly-added torrent silently failed
  the upsert.  Fixed: `Unknown(_)` lowers to `'other'` (the catch-
  all the constraint already allows).  Commit `beaad22`.
- **MAM 400 ms inter-request guard** rejected concurrent boot
  calls with `LocalRateLimit` instead of waiting.  On unlucky
  boots, `dynamicSeedbox.php` was the loser and the operator
  silently missed registration until the 5-minute keep-alive
  retried.  Fixed: `wait_for_rate_limit()` holds a tokio mutex
  across a sleep so callers serialize.  Commit `026a3f3`.
- **Silent DB failures.**  The DB shell logged nothing when the
  actor returned `DbEvent::Failed`.  Now logs at WARN, surfaced
  the first bug above immediately when reading container logs.
  Commit `beaad22`.

## See also

- `docs/operator-readiness.md` §34 — purpose statement and the
  architectural decisions this doc implements.
- `docs/integration-test-audit.md` — phase 1 of §34; historical
  per-story coverage analysis + the punch-list disposition.
- `docs/mam-api.md` — the MAM contract the fake encodes.
- `docs/invariants.md` — what stays at the property-test layer
  and why.
