# Windlass

Windlass is a lightweight Rust operator for self-hosted torrent stacks.  It
owns its WireGuard VPN tunnel in-process (`docs/vpn-ownership.md`) and keeps
everything in sync automatically, so a VPN reconnect or IP change requires no
manual intervention.

## What it does

- **In-process WireGuard tunnel** — brings up `wg0`, installs an nftables
  kill switch, watches the handshake, and rotates endpoints on stalls
- **Port forwarding sync** — obtains the forwarded port via NAT-PMP and
  updates qBittorrent's listen port whenever it changes
- **MAM seedbox updates** — notifies MyAnonamouse of your current VPN exit IP
  and port so you stay connectable on the tracker
- **Tunnel health monitoring** — detects when the tunnel dies, dumps logs,
  restarts dependent containers when it recovers
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

The stack runs the same tunnel topology as production (requires the host
kernel's `wireguard` module):

| Container     | Role                                                     | Ports    |
| ------------- | -------------------------------------------------------- | -------- |
| `wg-server`   | WireGuard fixture peer + exit-IP reflector + NAT-PMP     | `:19090` (control) |
| `qbittorrent` | Real qBittorrent, sharing Windlass's network namespace   | `:18080` (via windlass) |
| `mock-mam`    | Fake MAM (testkit) with a request journal + control API  | `:18082` |
| `postgres`    | Real Postgres                                            | `:15432` |
| `windlass`    | Built from source; `NET_ADMIN`, owns `wg0` + kill switch | `:5010`  |

### Driving the fixture

```fish
# Change the NAT-PMP granted port (propagates on the next renewal)
curl -X POST http://localhost:19090/control/natpmp-port -d '51821'

# Override the exit IP the reflector reports (simulates an IP change)
curl -X POST http://localhost:19090/control/exit-ip -d '10.8.0.42'

# Restore fixture defaults
curl -X POST http://localhost:19090/control/reset
```

Watch the dashboard at `http://localhost:5010` to see Windlass react in real time.

### Integration tests

Contract tests live in `windlass/tests/integration_contracts.rs`. They are
`#[ignore]` by default and require the dev stack to be running.
`just integration` handles everything automatically:

```fish
just integration      # builds stack → runs tests → tears down
just integration-wg   # real-WireGuard tunnel lifecycle suite
```

See `docs/integration-tests.md` for the harness and how to add tests.

## Configuration

Windlass is configured entirely via environment variables.

| Variable            | Required | Default                       | Description                         |
| ------------------- | -------- | ----------------------------- | ----------------------------------- |
| `QBITTORRENT_URL`   | ✓        | —                             | qBittorrent WebUI base URL          |
| `QBITTORRENT_USER`  | ✓        | —                             | qBittorrent WebUI username          |
| `QBITTORRENT_PASS`  | ✓        | —                             | qBittorrent WebUI password          |
| `MAM_SESSION`       | ✓        | —                             | MyAnonamouse session cookie value   |
| `WG_CONFIG_PATH`    | ✓        | —                             | Path to the ProtonVPN-generated `wg.conf` Windlass uses to bring up the in-process WireGuard tunnel. See `docs/vpn-ownership.md`. |
| `MAM_USER_AGENT`    |          | `windlass`                    | User-Agent sent to MAM              |
| `DATA_PATH`         |          | `/mnt/Data`                   | Path to monitor for disk space      |
| `DISK_POLL_INTERVAL_SECS` | | `60` | Seconds between available-space observations |
| `DISK_HARD_FLOOR_BYTES` | | `53687091200` (50 GiB) | Free-space threshold below which disk-pressure handling starts |
| `DUMP_DIR`          |          | `/mnt/Data/windlass_dumps`    | Directory for crash log dumps       |
| `WINDLASS_BIND`     |          | `0.0.0.0:5010`                | Address inside the container for the embedded web server |
| `WG_INTERFACE_NAME` |          | `wg0`                         | Tunnel interface name |
| `NATPMP_GATEWAY`    |          | `10.2.0.1:5351`               | NAT-PMP gateway address for the in-process port-forwarding flow |
| `TUNNEL_FIREWALL_ALLOW_TCP` | | — | Comma-separated `ip:port` allow-list for non-tunnel TCP control-plane dependencies. The shipped compose uses this only for Postgres. |
| `TUNNEL_LOCAL_ROUTES` | | — | Comma-separated private network ranges whose replies bypass the tunnel. Empty means host-local access only; the shipped Compose enables Tailscale and `192.168.2.0/24`. |
| `EXIT_IP_URLS`      |          | `api.ipify.org,ipv4.icanhazip.com` | Comma-separated URLs the exit-IP query GETs through the tunnel |
| `EXIT_IP_QUERY_INTERVAL_SECS` | | `21600` (6 h) | Exit-IP query cadence |
| `SEEDBOX_UPDATE_MIN_INTERVAL_SECS` | | `3660` (61 min) | Machine-side spacing between MAM dynamic-seedbox updates |

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

## Container image

GitHub Actions publishes the production image to:

```text
ghcr.io/zerofern/windlass
```

Every build from `main` receives `latest` and an immutable tag containing
the full commit SHA: `sha-<40-character-commit>`. Pushing a tag such as
`v1.2.3` also publishes `1.2.3` and `1.2`. Production deployments should
use the immutable full-SHA tag.
