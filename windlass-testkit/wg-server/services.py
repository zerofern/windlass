#!/usr/bin/env python3
"""In-tunnel services for the WireGuard fixture.

- HTTP exit-IP reflector on :8080 — returns the caller's source IP as
  plain text (or the operator-set override).  Reached through the
  tunnel, the source is the client's inside address (10.2.0.2).
- NAT-PMP responder on UDP :5351 — minimal RFC 6886 port-map
  responses for both the UDP and TCP ops, always granting the same
  external port so the client's dual-mapping flow (UDP + TCP must
  agree) can complete.
- Control plane on the same HTTP server, for the integration suite:
    POST /control/natpmp-port   body: the new external port (digits)
    POST /control/exit-ip       body: an IPv4 literal, or empty to
                                clear the override
    POST /control/reset         restore defaults
  The granted port changes propagate to the client on its next
  NAT-PMP renewal (lease lifetime is NATPMP_LIFETIME_SECONDS,
  default 60; the dev stack shortens it).
"""

import http.server
import os
import socket
import struct
import threading
import time

DEFAULT_EXTERNAL_PORT = 43210
LIFETIME_SECONDS = int(os.environ.get("NATPMP_LIFETIME_SECONDS", "60"))
START = time.monotonic()

state = {
    "external_port": DEFAULT_EXTERNAL_PORT,
    "exit_ip_override": None,
}
state_lock = threading.Lock()


class Handler(http.server.BaseHTTPRequestHandler):
    def _respond(self, code, body):
        data = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        with state_lock:
            override = state["exit_ip_override"]
        ip = override if override else self.client_address[0]
        self._respond(200, ip + "\n")

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length).decode().strip()
        if self.path == "/control/natpmp-port":
            if not body.isdigit() or not 0 < int(body) < 65536:
                self._respond(400, "expected a port number\n")
                return
            with state_lock:
                state["external_port"] = int(body)
            self._respond(200, "ok\n")
        elif self.path == "/control/exit-ip":
            with state_lock:
                state["exit_ip_override"] = body if body else None
            self._respond(200, "ok\n")
        elif self.path == "/control/reset":
            with state_lock:
                state["external_port"] = DEFAULT_EXTERNAL_PORT
                state["exit_ip_override"] = None
            self._respond(200, "ok\n")
        else:
            self._respond(404, "unknown control path\n")

    def log_message(self, fmt, *args):
        pass


def natpmp_responder():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", 5351))
    while True:
        data, addr = sock.recvfrom(64)
        # Port-map request: version 0, op 1 (UDP) or 2 (TCP), 12 bytes.
        if len(data) != 12 or data[0] != 0 or data[1] not in (1, 2):
            continue
        internal_port = struct.unpack_from("!H", data, 4)[0]
        epoch = int(time.monotonic() - START)
        with state_lock:
            external_port = state["external_port"]
        resp = struct.pack(
            "!BBHIHHI",
            0,                  # version
            data[1] | 0x80,     # response op mirrors the request
            0,                  # result code: success
            epoch,              # seconds since "gateway boot"
            internal_port,      # echo the requested internal port
            external_port,
            LIFETIME_SECONDS,
        )
        sock.sendto(resp, addr)


threading.Thread(target=natpmp_responder, daemon=True).start()
http.server.ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
