#!/usr/bin/env python3
"""In-tunnel services for the wg integration fixture.

- HTTP exit-IP reflector on :8080 — returns the caller's source IP as
  plain text.  Reached through the tunnel, that's the client's inside
  address (10.2.0.2), which is what the suite asserts.
- NAT-PMP responder on UDP :5351 — minimal RFC 6886 port-map
  responses for both the UDP and TCP ops, always granting the same
  fixed external port so the client's dual-mapping flow (UDP + TCP
  must agree) can complete.
"""

import http.server
import socket
import struct
import threading
import time

EXTERNAL_PORT = 43210
LIFETIME_SECONDS = 60
START = time.monotonic()


class Reflector(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = (self.client_address[0] + "\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

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
        resp = struct.pack(
            "!BBHIHHI",
            0,                  # version
            data[1] | 0x80,     # response op mirrors the request
            0,                  # result code: success
            epoch,              # seconds since "gateway boot"
            internal_port,      # echo the requested internal port
            EXTERNAL_PORT,
            LIFETIME_SECONDS,
        )
        sock.sendto(resp, addr)


threading.Thread(target=natpmp_responder, daemon=True).start()
http.server.ThreadingHTTPServer(("0.0.0.0", 8080), Reflector).serve_forever()
