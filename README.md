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
| `GLUETUN_PROXY_URL` |          | —                             | Gluetun HTTP proxy for MAM traffic  |
| `MAM_USER_AGENT`    |          | `windlass`                    | User-Agent sent to MAM              |
| `DATA_PATH`         |          | `/mnt/Data`                   | Path to monitor for disk space      |
| `DUMP_DIR`          |          | `/mnt/Data/windlass_dumps`    | Directory for crash log dumps       |
| `VPN_IP_FILE`       |          | `/tmp/gluetun/ip`             | Gluetun IP file path                |
| `VPN_PORT_FILE`     |          | `/tmp/gluetun/forwarded_port` | Gluetun forwarded port file path    |
| `WINDLASS_BIND`     |          | `0.0.0.0:5010`                | Address for the embedded web server |
| `WINDLASS_EXECUTE_SHADOW_ACTIONS` | | `true` | Execute the new service-core action path; set `false` for legacy-only orchestration rollback |

## Running with Docker Compose

Windlass must share Gluetun's network namespace so it can reach qBittorrent
and read the VPN files.

```yaml
windlass:
  image: ghcr.io/stirlingmouse/windlass:main
  container_name: windlass
  network_mode: "service:gluetun"
  volumes:
    - /opt/gluetun/tmp:/tmp/gluetun:ro
  environment:
    - QBITTORRENT_URL=http://localhost:8080
    - QBITTORRENT_USER=admin
    - QBITTORRENT_PASS=changeme
    - MAM_SESSION=your_session_cookie
    - GLUETUN_PROXY_URL=http://localhost:8888
  restart: unless-stopped
  depends_on:
    gluetun:
      condition: service_healthy
```
