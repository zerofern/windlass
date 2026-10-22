//! NAT-PMP UDP client.
//!
//! Thin async wrapper around the [`windlass_tunnel_core::natpmp`]
//! codec.  Builds the request bytes, sends via UDP to the
//! `ProtonVPN` gateway, awaits the response within a configurable
//! timeout, and returns the typed
//! [`windlass_tunnel_core::NatPmpLease`] or a typed error.
//!
//! Every request/response pair is captured via the supplied
//! [`HttpTap`] as an [`HttpExchange`] with `module = "natpmp"`,
//! `method = "udp"`, request/response bodies as hex-encoded byte
//! strings.  That's how tunnel ops land in the same observability
//! ring as MAM/qBit HTTP and become visible to the existing
//! `/observability` UI.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::net::UdpSocket;
use windlass_tunnel_core::{NatPmpDecodeError, NatPmpLease, NatPmpRequest};
use windlass_types::{CoreId, HttpExchange, HttpRequestView, HttpTap};

/// UDP client for the NAT-PMP port-map flow.
///
/// One instance per shell — the underlying socket is bound at
/// construction time and reused across requests so subsequent calls
/// don't pay the bind cost again.
pub struct NatPmpClient {
    socket: UdpSocket,
    gateway: SocketAddr,
    request_timeout: Duration,
    tap: Arc<dyn HttpTap>,
    core_id: CoreId,
}

#[derive(Debug, Error)]
pub enum NatPmpClientError {
    #[error("failed to bind local UDP socket: {0}")]
    Bind(#[source] std::io::Error),
    #[error("failed to send NAT-PMP request to {gateway}: {source}")]
    Send {
        gateway: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("timed out waiting for NAT-PMP response from {gateway} after {timeout:?}")]
    Timeout {
        gateway: SocketAddr,
        timeout: Duration,
    },
    #[error("failed to read NAT-PMP response: {0}")]
    Recv(#[source] std::io::Error),
    #[error("malformed NAT-PMP response: {0}")]
    Decode(#[from] NatPmpDecodeError),
}

impl NatPmpClient {
    /// Creates a client bound to a kernel-chosen local UDP port,
    /// configured to talk to the given gateway socket address.
    ///
    /// # Errors
    ///
    /// Returns [`NatPmpClientError::Bind`] if the local UDP bind
    /// fails — typically because something is already bound to the
    /// requested port or the namespace lacks capabilities.
    pub async fn new(
        gateway: SocketAddr,
        request_timeout: Duration,
        core_id: CoreId,
        tap: Arc<dyn HttpTap>,
    ) -> Result<Self, NatPmpClientError> {
        // Bind to an ephemeral port on all interfaces inside the
        // namespace.  The kernel will route via `wg0` because the
        // gateway is the tunnel peer's inside address.
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(NatPmpClientError::Bind)?;
        // Connect so the kernel drops datagrams from any source
        // other than the gateway — without this, anything that can
        // reach our ephemeral port could forge a lease.
        socket
            .connect(gateway)
            .await
            .map_err(NatPmpClientError::Bind)?;
        Ok(Self {
            socket,
            gateway,
            request_timeout,
            tap,
            core_id,
        })
    }

    /// Sends one NAT-PMP request and awaits the matching response.
    ///
    /// Datagrams that don't answer *this* request — malformed bytes,
    /// or a well-formed response/error for the other protocol or a
    /// different internal port (stale answers to an earlier request)
    /// — are ignored and the wait continues until the deadline, per
    /// RFC 6886 §3.2.  Adopting the first 16 bytes that arrived
    /// (the previous behavior) could assign a wrong lease.
    ///
    /// # Errors
    ///
    /// One of [`NatPmpClientError::Send`] / [`NatPmpClientError::Timeout`] /
    /// [`NatPmpClientError::Recv`] / [`NatPmpClientError::Decode`].
    pub async fn request(&self, req: NatPmpRequest) -> Result<NatPmpLease, NatPmpClientError> {
        let bytes = req.encode();
        let url = self.gateway.to_string();
        self.tap
            .gate_request(
                self.core_id,
                &HttpRequestView {
                    method: "udp",
                    url: &url,
                    body: None,
                },
            )
            .await;
        self.socket.send(&bytes).await.map_err(|source| {
            self.emit_exchange(&url, &bytes, &[], None);
            NatPmpClientError::Send {
                gateway: self.gateway,
                source,
            }
        })?;

        // Oversized so a too-long datagram fails the codec's length
        // check instead of being silently truncated to a valid 16.
        let mut buf = [0u8; 64];
        let deadline = tokio::time::Instant::now() + self.request_timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.socket.recv(&mut buf)).await {
                Err(_) => {
                    self.emit_exchange(&url, &bytes, &[], Some("timeout"));
                    return Err(NatPmpClientError::Timeout {
                        gateway: self.gateway,
                        timeout: self.request_timeout,
                    });
                }
                Ok(Err(e)) => {
                    self.emit_exchange(&url, &bytes, &[], Some("recv-error"));
                    return Err(NatPmpClientError::Recv(e));
                }
                Ok(Ok(n)) => {
                    let response_bytes = &buf[..n];
                    match NatPmpLease::decode(response_bytes) {
                        Ok(lease) if lease.matches_request(&req) => {
                            self.emit_exchange(&url, &bytes, response_bytes, None);
                            return Ok(lease);
                        }
                        // A gateway error for the protocol we asked
                        // about is a real answer; surface it.
                        Err(e @ NatPmpDecodeError::ErrorCode { protocol, .. })
                            if protocol == req.protocol =>
                        {
                            self.emit_exchange(&url, &bytes, response_bytes, None);
                            return Err(e.into());
                        }
                        // Anything else is noise on the socket:
                        // stale answer for another request, or a
                        // malformed datagram.  Log it to the tap and
                        // keep waiting for the real response.
                        Ok(_) | Err(_) => {
                            self.emit_exchange(&url, &bytes, response_bytes, Some("ignored"));
                        }
                    }
                }
            }
        }
    }

    fn emit_exchange(
        &self,
        url: &str,
        request_bytes: &[u8],
        response_bytes: &[u8],
        error_tag: Option<&str>,
    ) {
        let response_headers =
            error_tag.map_or_else(Vec::new, |tag| vec![("error".to_string(), tag.to_string())]);
        // For a parsed NAT-PMP response, the op + result code are in
        // bytes 1..4; the encoded response_status is the result code
        // so the operator sees success/failure at a glance.
        let status = if response_bytes.len() >= 4 {
            u16::from_be_bytes([response_bytes[2], response_bytes[3]])
        } else {
            0
        };
        self.tap.observed_exchange(
            self.core_id,
            &HttpExchange {
                module: "natpmp".to_string(),
                method: "udp".to_string(),
                url: url.to_string(),
                request_headers: Vec::new(),
                request_body: Some(hex_encode(request_bytes)),
                response_status: status,
                response_headers,
                response_body: hex_encode(response_bytes),
            },
        );
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_tunnel_core::natpmp::Protocol;

    /// Drives [`NatPmpClient`] against a local UDP fake that
    /// answers with a hand-crafted success response.  Exercises the
    /// full encode→send→recv→decode path without touching the
    /// real `ProtonVPN` gateway.
    #[tokio::test]
    async fn round_trip_against_local_udp_server() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway = server.local_addr().unwrap();

        // Spawn the fake gateway: read 12 bytes, echo a synthetic
        // 16-byte success response.
        tokio::spawn(async move {
            let mut req_buf = [0u8; 12];
            let (_n, peer) = server.recv_from(&mut req_buf).await.unwrap();
            let mut resp = [0u8; 16];
            // Version 0, Op = request_op | 0x80
            resp[0] = 0;
            resp[1] = 0x80 | req_buf[1];
            // Result code 0 (success), epoch 7
            resp[2..4].copy_from_slice(&0u16.to_be_bytes());
            resp[4..8].copy_from_slice(&7u32.to_be_bytes());
            // Internal port from request bytes 4..6
            resp[8..10].copy_from_slice(&req_buf[4..6]);
            // External port we grant: 51_820
            resp[10..12].copy_from_slice(&51_820u16.to_be_bytes());
            // Lifetime: 60 s
            resp[12..16].copy_from_slice(&60u32.to_be_bytes());
            server.send_to(&resp, peer).await.unwrap();
        });

        let client = NatPmpClient::new(
            gateway,
            Duration::from_secs(2),
            CoreId::Tunnel,
            windlass_types::NullHttpTap::arc(),
        )
        .await
        .expect("local bind succeeds");
        let lease = client
            .request(NatPmpRequest {
                protocol: Protocol::Udp,
                internal_port: 0,
                external_port_hint: 0,
                lifetime_seconds: 60,
            })
            .await
            .expect("round trip succeeds");
        assert_eq!(lease.external_port, 51_820);
        assert_eq!(lease.lifetime_seconds, 60);
    }

    /// A wrong-protocol datagram (e.g. the duplicate answer to an
    /// earlier UDP request arriving while we wait on the TCP one)
    /// must be ignored, and the wait must continue until the real
    /// response lands.
    #[tokio::test]
    async fn stale_response_for_other_protocol_is_ignored() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway = server.local_addr().unwrap();

        tokio::spawn(async move {
            let mut req_buf = [0u8; 12];
            let (_n, peer) = server.recv_from(&mut req_buf).await.unwrap();
            let mut resp = [0u8; 16];
            resp[0] = 0;
            resp[2..4].copy_from_slice(&0u16.to_be_bytes());
            resp[4..8].copy_from_slice(&7u32.to_be_bytes());
            resp[8..10].copy_from_slice(&req_buf[4..6]);
            resp[12..16].copy_from_slice(&60u32.to_be_bytes());
            // First: a success response for the OTHER protocol with
            // a tempting port the client must not adopt.
            resp[1] = 0x80 | if req_buf[1] == 2 { 1 } else { 2 };
            resp[10..12].copy_from_slice(&1111u16.to_be_bytes());
            server.send_to(&resp, peer).await.unwrap();
            // Then the real answer.
            resp[1] = 0x80 | req_buf[1];
            resp[10..12].copy_from_slice(&2222u16.to_be_bytes());
            server.send_to(&resp, peer).await.unwrap();
        });

        let client = NatPmpClient::new(
            gateway,
            Duration::from_secs(2),
            CoreId::Tunnel,
            windlass_types::NullHttpTap::arc(),
        )
        .await
        .expect("local bind succeeds");
        let lease = client
            .request(NatPmpRequest {
                protocol: Protocol::Tcp,
                internal_port: 0,
                external_port_hint: 0,
                lifetime_seconds: 60,
            })
            .await
            .expect("matching response should win");
        assert_eq!(lease.external_port, 2222);
        assert_eq!(lease.protocol, Protocol::Tcp);
    }

    #[tokio::test]
    async fn timeout_fires_when_gateway_silent() {
        // A bound-but-silent fake gateway: the request is consumed
        // and never answered, so the deadline must fire.  (An
        // unbound port no longer works for this test — the connected
        // socket surfaces ICMP port-unreachable as a Recv error.)
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway = server.local_addr().unwrap();
        let _hold = server; // keep the port bound, never respond
        let client = NatPmpClient::new(
            gateway,
            Duration::from_millis(50),
            CoreId::Tunnel,
            windlass_types::NullHttpTap::arc(),
        )
        .await
        .expect("local bind succeeds");
        let err = client
            .request(NatPmpRequest {
                protocol: Protocol::Udp,
                internal_port: 0,
                external_port_hint: 0,
                lifetime_seconds: 60,
            })
            .await
            .expect_err("expected timeout");
        assert!(matches!(err, NatPmpClientError::Timeout { .. }));
    }
}
