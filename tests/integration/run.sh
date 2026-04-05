#!/usr/bin/env bash
# Integration test for Windlass.
#
# Spins up the full mock stack, drives it through the key scenarios, then
# tears everything down.  Exit 0 = all pass, non-zero = failure.
#
# Requirements: docker, docker compose (v2), python3 (stdlib only)
#
# Usage:
#   ./tests/integration/run.sh
#
# Run from the repo root.

set -euo pipefail
cd "$(dirname "$0")/../.."          # ensure we run from repo root

COMPOSE="docker compose -f docker-compose.test.yml"
QBIT_ADMIN="http://localhost:18080/__admin"
GOTIFY_ADMIN="http://localhost:18081/__admin"
MAM_ADMIN="http://localhost:18082/__admin"

# ── Helpers ───────────────────────────────────────────────────────────────────

log()  { echo "[$(date '+%H:%M:%S')] $*"; }
pass() { echo "  ✓ $*"; }
fail() {
    echo "  ✗ FAIL: $*" >&2
    log "Dumping Windlass logs..."
    $COMPOSE logs windlass || true
    log "Tearing down..."
    $COMPOSE down -v --remove-orphans 2>/dev/null || true
    exit 1
}

# Wait up to $3 seconds for $1 to respond at $2.
wait_for() {
    local name=$1 url=$2 secs=${3:-30}
    log "Waiting for $name..."
    for i in $(seq 1 "$secs"); do
        if curl -sf "$url" 2>/dev/null | grep -q healthy; then
            log "$name is ready (${i}s)"
            return 0
        fi
        sleep 1
    done
    fail "$name did not become ready within ${secs}s"
}

# Count received requests at a WireMock admin URL.
# $1 = admin base URL, $2 = URL fragment to match, $3 = optional body fragment
count_requests() {
    local admin_url=$1 url_fragment=$2 body_fragment=${3:-}
    local tmp
    tmp=$(mktemp)
    curl -sf "${admin_url}/requests" >"$tmp" 2>/dev/null \
        || echo '{"requests":[]}' >"$tmp"

    # Pass the JSON file as argv[1] so stdin is free for the heredoc script.
    URL_FRAG="$url_fragment" BODY_FRAG="$body_fragment" python3 - "$tmp" <<'PYEOF'
import json, os, sys

with open(sys.argv[1]) as fh:
    data = json.load(fh)

url_frag  = os.environ['URL_FRAG']
body_frag = os.environ.get('BODY_FRAG', '')

count = sum(
    1 for r in data.get('requests', [])
    if url_frag in r.get('request', {}).get('url', '')
    and (not body_frag or body_frag in r.get('request', {}).get('body', ''))
)
print(count)
PYEOF
    rm -f "$tmp"
}

# ── Setup ─────────────────────────────────────────────────────────────────────

log "=== Windlass Integration Test ==="
log "Tearing down any previous run..."
$COMPOSE down -v --remove-orphans 2>/dev/null || true

log "Building and starting test stack..."
$COMPOSE up --build -d

wait_for "mock-qbittorrent" "${QBIT_ADMIN}/health"   30
wait_for "mock-gotify"       "${GOTIFY_ADMIN}/health" 30
wait_for "mock-mam"          "${MAM_ADMIN}/health"    30

log "Waiting for Windlass boot sequence to complete (20s)..."
sleep 20

# ── Scenario 1: Boot sequence ─────────────────────────────────────────────────
log "--- Scenario 1: Boot sequence ---"

N=$(count_requests "$QBIT_ADMIN" "/api/v2/auth/login")
[ "$N" -ge 1 ] || fail "qBit auth was not called (got $N)"
pass "qBit authenticated ($N call(s))"

N=$(count_requests "$QBIT_ADMIN" "/api/v2/app/setPreferences" "51820")
[ "$N" -ge 1 ] || fail "Port sync to 51820 was not called (got $N)"
pass "Port synced to 51820"

N=$(count_requests "$GOTIFY_ADMIN" "/message")
[ "$N" -ge 1 ] || fail "Gotify received no alerts (got $N)"
pass "Gotify received $N alert(s)"

N=$(count_requests "$MAM_ADMIN" "/json/dynamicSeedbox.php")
[ "$N" -ge 1 ] || fail "MAM seedbox not called (got $N)"
pass "MAM seedbox updated ($N call(s))"

# ── Scenario 2: VPN reconnect ─────────────────────────────────────────────────
log "--- Scenario 2: Simulating VPN reconnect (new port 51821) ---"

# Overwrite the VPN files to simulate Gluetun getting a new forwarded port.
$COMPOSE exec -T mock-gluetun sh -c \
    "printf '10.8.0.2' > /tmp/gluetun/ip && printf '51821' > /tmp/gluetun/forwarded_port"

log "Waiting for Windlass to detect file change and re-sync (15s)..."
sleep 15

N=$(count_requests "$QBIT_ADMIN" "/api/v2/app/setPreferences" "51821")
[ "$N" -ge 1 ] || fail "Port re-sync to 51821 was not called (got $N)"
pass "Port re-synced to 51821 after reconnect"

N_ALERTS=$(count_requests "$GOTIFY_ADMIN" "/message")
[ "$N_ALERTS" -ge 2 ] || fail "Expected ≥2 Gotify alerts after reconnect, got $N_ALERTS"
pass "Gotify received $N_ALERTS total alerts (boot + reconnect)"

# ── Teardown ──────────────────────────────────────────────────────────────────
log "All scenarios passed. Tearing down..."
$COMPOSE down -v --remove-orphans

echo ""
echo "╔══════════════════════════════════════╗"
echo "║  ✓  Integration test suite PASSED    ║"
echo "╚══════════════════════════════════════╝"
