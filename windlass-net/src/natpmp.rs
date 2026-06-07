//! NAT-PMP UDP client.
//!
//! Thin async wrapper around the [`windlass_tunnel_core::natpmp`]
//! codec.  Builds the request bytes, sends via UDP to the
//! `ProtonVPN` gateway, awaits the response within a configurable
//! timeout, and returns the typed
//! [`windlass_tunnel_core::NatPmpLease`] or a typed error.

use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;
use tokio::net::UdpSocket;
use windlass_tunnel_core::{NatPmpDecodeError, NatPmpLease, NatPmpRequest};

/// UDP client for the NAT-PMP port-map flow.
///
/// One instance per shell — the underlying socket is bound at
/// construction time and reused across requests so subsequent calls
/// don't pay the bind cost again.
#[derive(Debug)]
pub struct NatPmpClient {
    socket: UdpSocket,
    gateway: SocketAddr,
    request_timeout: Duration,
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
    ) -> Result<Self, NatPmpClientError> {
        // Bind to an ephemeral port on all interfaces inside the
        // namespace.  The kernel will route via `wg0` because the
        // gateway is the tunnel peer's inside address.
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(NatPmpClientError::Bind)?;
        Ok(Self {
            socket,
            gateway,
            request_timeout,
        })
    }

    /// Sends one NAT-PMP request and awaits the matching response.
    ///
    /// # Errors
    ///
    /// One of [`NatPmpClientError::Send`] / [`NatPmpClientError::Timeout`] /
    /// [`NatPmpClientError::Recv`] / [`NatPmpClientError::Decode`].
    pub async fn request(&self, req: NatPmpRequest) -> Result<NatPmpLease, NatPmpClientError> {
        let bytes = req.encode();
        self.socket
            .send_to(&bytes, self.gateway)
            .await
            .map_err(|source| NatPmpClientError::Send {
                gateway: self.gateway,
                source,
            })?;

        let mut buf = [0u8; 16];
        match tokio::time::timeout(self.request_timeout, self.socket.recv(&mut buf)).await {
            Err(_) => Err(NatPmpClientError::Timeout {
                gateway: self.gateway,
                timeout: self.request_timeout,
            }),
            Ok(Err(e)) => Err(NatPmpClientError::Recv(e)),
            Ok(Ok(n)) => Ok(NatPmpLease::decode(&buf[..n])?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
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

        let client = NatPmpClient::new(gateway, Duration::from_secs(2))
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

    #[tokio::test]
    async fn timeout_fires_when_gateway_silent() {
        // Pick an address that won't respond.  We don't bind a fake
        // server so the recv side will time out.
        let gateway = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
        let client = NatPmpClient::new(gateway, Duration::from_millis(50))
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
