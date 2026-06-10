# Windlass

Windlass is a lightweight Rust operator for self-hosted torrent stacks running
behind a Gluetun VPN. It watches the VPN tunnel and keeps everything in sync
automatically, so a VPN reconnect or IP change requires no manual intervention.

## What it does

- **Port forwarding sync** — reads Gluetun's forwarded port file and updates
  qBittorrent's listen port whenever it changes
- **MAM seedbox updates** — notifies MyAnonamouse of your current VPN IP and
  port so you stay connectable on the tracker
- **Container health monitoring** — detects when Gluetun dies or becomes
  unhealthy, dumps logs, restarts dependent containers when the tunnel recovers
- **New torrent alerts** — records an alert when new torrents appear in
  qBittorrent
- **Low disk space alerts** — records an alert when available space drops below
  the configured threshold
- **Crash log dumps** — collects container logs to disk on unhealthy events for
  post-mortem inspection
- **Web UI** — embedded dashboard served at `:5010` showing live operator state
  and a scrolling event/action log

## Development

### Prerequisites

- Rust (stable)
- Docker + Docker Compose v2
- Node.js 22+ via [fnm](https://github.com/Schniz/fnm) (for the frontend)

### Common tasks

```fish
just build-web        # build the React frontend into app/dist/
just build            # compile the Rust workspace (embeds the frontend)
just test             # run unit tests
just check            # fmt-check + clippy + test

just dev-web          # Vite dev server on :5173 with /api proxied to :5010
just stack-up         # start the full mock stack in Docker (see below)
just stack-down       # tear down the mock stack
just stack-logs       # tail Windlass logs from the mock stack
just integration      # run Rust integration tests against the mock stack
```

### Mock dev stack

`docker-compose.dev.yml` runs Windlass against fully mocked external
dependencies — no real VPN, no real qBittorrent, no real MAM account needed.
Use it to develop the UI, test new features, and run the integration suite.

```fish
just build-web        # frontend must be built before docker build
just stack-up         # builds and starts all containers
# open http://localhost:5010
```

The stack includes:

| Container          | Role                                  | Ports            |
| ------------------ | ------------------------------------- | ---------------- |
| `mock-gluetun`     | Writes VPN IP/port files; control API | `:9001`          |
| `mock-qbittorrent` | WireMock stub for qBit API            | `:18080` (admin) |
| `mock-mam`         | WireMock stub for MAM                 | `:18082` (admin) |
| `chaos-controller` | Named scenario API                    | `:9000`          |
| `windlass`         | Built from source                     | `:5010`          |

### Chaos scenarios

The chaos controller lets you inject fault conditions at runtime to observe
how Windlass responds:

```fish
# Trigger a named scenario
curl -X POST http://localhost:9000/scenario/qbit-auth-fail
curl -X POST http://localhost:9000/scenario/mam-rate-limit

# Restore all mocks to the happy-path default
curl -X POST http://localhost:9000/reset

# Simulate VPN reconnect with a new port
curl -X POST http://localhost:9001/set \
  -H "Content-Type: application/json" \
  -d '{"ip":"10.8.0.2","port":51821}'

# Write an empty port file (simulates Gluetun not yet having a port)
curl -X POST http://localhost:9001/clear-port
```

Watch the dashboard at `http://localhost:5010` to see Windlass react in real time.

### Integration tests

Integration tests are in `windlass/tests/integration.rs`. They are `#[ignore]`
by default and require the dev stack to be running. `just integration` handles
everything automatically:

```fish
just integration      # builds stack → runs tests → tears down
```

When adding new external API behaviour, add a corresponding scenario to
`windlass-testkit/src/scenarios.rs` and a test to `windlass/tests/integration.rs`.

## Configuration

Windlass is configured entirely via environment variables.

| Variable            | Required | Default                       | Description                         |
| ------------------- | -------- | ----------------------------- | ----------------------------------- |
| `QBITTORRENT_URL`   | ✓        | —                             | qBittorrent WebUI base URL          |
| `QBITTORRENT_USER`  | ✓        | —                             | qBittorrent WebUI username          |
| `QBITTORRENT_PASS`  | ✓        | —                             | qBittorrent WebUI password          |
| `MAM_SESSION`       | ✓        | —                             | MyAnonamouse session cookie value   |
| `WG_CONFIG_PATH`    | tunnel mode | —                          | Path to the ProtonVPN-generated `wg.conf` Windlass uses to bring up the in-process WireGuard tunnel. When unset, Windlass keeps the legacy Gluetun-compatible VPN shell active. See `docs/vpn-ownership.md`. |
| `MAM_USER_AGENT`    |          | `windlass`                    | User-Agent sent to MAM              |
| `DATA_PATH`         |          | `/mnt/Data`                   | Path to monitor for disk space      |
| `DUMP_DIR`          |          | `/mnt/Data/windlass_dumps`    | Directory for crash log dumps       |
| `WINDLASS_BIND`     |          | `0.0.0.0:5010`                | Address for the embedded web server |
| `WINDLASS_EXECUTE_SERVICE_ACTIONS` | | `true` | Execute the sans-I/O service-core action path; disabling is diagnostic only |
| `WG_INTERFACE_NAME` |          | `wg0`                         | Tunnel interface name |
| `NATPMP_GATEWAY`    |          | `10.2.0.1:5351`               | NAT-PMP gateway address for the in-process port-forwarding flow |
| `TUNNEL_FIREWALL_ALLOW_TCP` | | — | Comma-separated `ip:port` allow-list for non-tunnel TCP control-plane dependencies. The shipped compose uses this only for Postgres. |

`WINDLASS_EXECUTE_SHADOW_ACTIONS` is still accepted as a deprecated alias for
the service action switch. Legacy service orchestration has been retired from
`windlass-core`, so this is no longer a rollback to a complete legacy path.

## Running with Docker Compose

Windlass owns the WireGuard tunnel in-process: no Gluetun container,
no proxy URL, no file watchers.  qBittorrent shares Windlass's
network namespace so both processes egress under the same Proton
exit IP (required for MAM connectability).  Windlass also joins a
private control network for Postgres and allows only that fixed DB
endpoint through the nftables kill switch. Requires `NET_ADMIN`.

```fish
docker compose -f docker-compose.tunnel.yml up -d
```

See `docker-compose.tunnel.yml` for the override skeleton and the
required `WG_CONFIG_PATH` env var.  Background on the design:
`docs/vpn-ownership.md`.
