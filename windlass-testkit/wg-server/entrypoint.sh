#!/bin/sh
# Test-fixture WireGuard "provider" for the wg integration suite.
#
# Generates a fresh keypair for both ends on every run, brings up the
# server side of the tunnel (10.2.0.1, mirroring ProtonVPN's inside
# addressing), writes the client's wg.conf into the shared volume —
# the runner and the compose healthcheck gate on that file — and then
# serves the in-tunnel services (exit-IP reflector + NAT-PMP
# responder) from services.py.
set -eu
umask 077

SERVER_PRIV=$(wg genkey)
SERVER_PUB=$(printf '%s' "$SERVER_PRIV" | wg pubkey)
CLIENT_PRIV=$(wg genkey)
CLIENT_PUB=$(printf '%s' "$CLIENT_PRIV" | wg pubkey)

ip link add wg0 type wireguard
printf '%s' "$SERVER_PRIV" >/tmp/server.key
wg set wg0 listen-port 51820 private-key /tmp/server.key \
    peer "$CLIENT_PUB" allowed-ips 10.2.0.2/32
ip addr add 10.2.0.1/24 dev wg0
ip link set wg0 up

# Atomic write: the healthcheck must never observe a half-written
# config.  PersistentKeepalive keeps the handshake fresh so the
# client's watchdog sees a recent handshake on every poll.
cat >/shared/wg.conf.tmp <<EOF
[Interface]
PrivateKey = $CLIENT_PRIV
Address = 10.2.0.2/32

[Peer]
PublicKey = $SERVER_PUB
AllowedIPs = 0.0.0.0/0
Endpoint = ${WG_INT_ENDPOINT:-172.31.0.10:51820}
PersistentKeepalive = 5
EOF
chmod 644 /shared/wg.conf.tmp
mv /shared/wg.conf.tmp /shared/wg.conf

exec python3 /opt/services.py
