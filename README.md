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
- **New torrent alerts** — sends a Gotify notification when new torrents appear
  in qBittorrent
- **Low disk space alerts** — sends a Gotify notification when available space
  drops below the configured threshold
- **Crash log dumps** — collects container logs to disk on unhealthy events for
  post-mortem inspection

## Configuration

Windlass is configured entirely via environment variables.

| Variable            | Required | Default                        | Description                            |
| ------------------- | -------- | ------------------------------ | -------------------------------------- |
| `QBITTORRENT_URL`   | ✓        | —                              | qBittorrent WebUI base URL             |
| `QBITTORRENT_USER`  | ✓        | —                              | qBittorrent WebUI username             |
| `QBITTORRENT_PASS`  | ✓        | —                              | qBittorrent WebUI password             |
| `MAM_SESSION`       | ✓        | —                              | MyAnonamouse session cookie value      |
| `GOTIFY_URL`        | ✓        | —                              | Gotify server base URL                 |
| `GOTIFY_TOKEN`      | ✓        | —                              | Gotify application token               |
| `GLUETUN_PROXY_URL` |          | —                              | Gluetun HTTP proxy for MAM traffic     |
| `DATA_PATH`         |          | `/mnt/Data`                    | Path to monitor for disk space         |
| `DUMP_DIR`          |          | `/mnt/Data/windlass_dumps`     | Directory for crash log dumps          |
| `VPN_IP_FILE`       |          | `/tmp/gluetun/ip`              | Gluetun IP file path                   |
| `VPN_PORT_FILE`     |          | `/tmp/gluetun/forwarded_port`  | Gluetun forwarded port file path       |

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
    - GOTIFY_URL=https://gotify.example.com
    - GOTIFY_TOKEN=your_token
    - GLUETUN_PROXY_URL=http://localhost:8888
  restart: unless-stopped
  depends_on:
    gluetun:
      condition: service_healthy
```
