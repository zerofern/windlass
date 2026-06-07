//! NAT-PMP codec (RFC 6886) for `ProtonVPN` port forwarding.
//!
//! This module is a pure protocol layer: bytes in, typed structs out.
//! UDP socket I/O lives in the shell crate.
//!
//! ## Protocol summary
//!
//! NAT-PMP port-map request (12 bytes):
//!
//! ```text
//! 0     1     2           4              8             12
//! +-----+-----+-----------+--------------+-------------+
//! | Ver | Op  | Reserved  | Internal Port| External    |
//! |  0  |     |    = 0    |              | Port (hint) |
//! +-----+-----+-----------+--------------+-------------+
//! |                Requested Lifetime (s)              |
//! +----------------------------------------------------+
//! ```
//!
//! Op is `1` for UDP, `2` for TCP.  `ProtonVPN` expects both; the
//! caller decides which to request.
//!
//! NAT-PMP port-map response (16 bytes):
//!
//! ```text
//! 0     1     2     4              8             12             16
//! +-----+-----+-----+--------------+-------------+--------------+
//! | Ver |Op+128| Code| Epoch (4)   | Internal    | External     |
//! |  0  |      |     |             | Port        | Port         |
//! +-----+-----+-----+--------------+-------------+--------------+
//! |              Lifetime granted (s)                          |
//! +-------------------------------------------------------------+
//! ```
//!
//! Op for a successful port-map response is the request's Op | 0x80.
//! Code 0 is success; non-zero is a typed error.

use thiserror::Error;

/// NAT-PMP protocol version.  RFC 6886 specifies 0.
const VERSION: u8 = 0;

/// Op codes for NAT-PMP port mapping.
const OP_MAP_UDP: u8 = 1;
const OP_MAP_TCP: u8 = 2;

/// A NAT-PMP port-map request, ready to be encoded for the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatPmpRequest {
    pub protocol: Protocol,
    /// Internal port we are asking to map.  0 is a wildcard meaning
    /// "any port the gateway picks for us"; `ProtonVPN`'s port-forward
    /// flow uses 0 here.
    pub internal_port: u16,
    /// External port hint.  0 means "let the gateway choose".
    pub external_port_hint: u16,
    /// Requested lifetime in seconds.  RFC 6886 §3.3 caps this at
    /// `604_800` (one week); `ProtonVPN`'s gateway grants 60-second leases
    /// regardless of what we request.
    pub lifetime_seconds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Udp,
    Tcp,
}

impl Protocol {
    const fn op(self) -> u8 {
        match self {
            Self::Udp => OP_MAP_UDP,
            Self::Tcp => OP_MAP_TCP,
        }
    }

    const fn from_op(op: u8) -> Option<Self> {
        match op & 0x7f {
            OP_MAP_UDP => Some(Self::Udp),
            OP_MAP_TCP => Some(Self::Tcp),
            _ => None,
        }
    }
}

impl NatPmpRequest {
    /// Encodes the request into the 12-byte wire format.
    #[must_use]
    pub const fn encode(&self) -> [u8; 12] {
        let internal = self.internal_port.to_be_bytes();
        let external = self.external_port_hint.to_be_bytes();
        let lifetime = self.lifetime_seconds.to_be_bytes();
        [
            VERSION,
            self.protocol.op(),
            0, // reserved
            0, // reserved
            internal[0],
            internal[1],
            external[0],
            external[1],
            lifetime[0],
            lifetime[1],
            lifetime[2],
            lifetime[3],
        ]
    }
}

/// A successful NAT-PMP port-map response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatPmpLease {
    pub protocol: Protocol,
    /// Gateway's epoch counter, in seconds since gateway reboot.  RFC
    /// 6886 §3.6: a decrease across responses means the gateway
    /// rebooted and existing leases may have been lost — clients
    /// must re-request immediately.
    pub epoch_seconds: u32,
    /// Internal port the response refers to.  Matches the request,
    /// or is the port the gateway picked when the request was 0.
    pub internal_port: u16,
    /// External (public-facing) port granted.
    pub external_port: u16,
    /// Lifetime granted, in seconds.  Renew well before this expires
    /// (`ProtonVPN` grants 60s; typical renewal cadence is 45s).
    pub lifetime_seconds: u32,
}

/// Errors produced by [`NatPmpLease::decode`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NatPmpDecodeError {
    #[error("response is {0} bytes; NAT-PMP port-map responses are 16 bytes")]
    WrongLength(usize),
    #[error("response version is {0}; expected 0 (RFC 6886)")]
    WrongVersion(u8),
    #[error("response op {0:#04x} is not a known port-map response (0x81 / 0x82)")]
    UnknownOp(u8),
    #[error("gateway returned error code {0:?}")]
    ErrorCode(NatPmpResponseCode),
}

/// Result codes a NAT-PMP gateway may return.  RFC 6886 §3.5 + the
/// `ProtonVPN` documented set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatPmpResponseCode {
    /// Code 0 — operation succeeded.  Carried as a variant so
    /// [`NatPmpLease::decode`] can return a typed Ok rather than
    /// silently treating non-zero codes as success.  See
    /// [`NatPmpLease::decode`] for usage.
    Success,
    /// Code 1 — Unsupported Version.
    UnsupportedVersion,
    /// Code 2 — Not Authorized / Refused.
    NotAuthorized,
    /// Code 3 — Network Failure.
    NetworkFailure,
    /// Code 4 — Out of Resources.
    OutOfResources,
    /// Code 5 — Unsupported Opcode.
    UnsupportedOpcode,
    /// Any other code.  Carried verbatim so the operator sees what
    /// the gateway said.
    Other(u16),
}

impl NatPmpResponseCode {
    /// Maps the on-wire code to a typed variant.
    #[must_use]
    pub const fn from_u16(code: u16) -> Self {
        match code {
            0 => Self::Success,
            1 => Self::UnsupportedVersion,
            2 => Self::NotAuthorized,
            3 => Self::NetworkFailure,
            4 => Self::OutOfResources,
            5 => Self::UnsupportedOpcode,
            other => Self::Other(other),
        }
    }
}

impl NatPmpLease {
    /// Decodes a 16-byte NAT-PMP port-map response.
    ///
    /// # Errors
    ///
    /// Returns [`NatPmpDecodeError`] on:
    /// - length mismatch,
    /// - unsupported version,
    /// - unknown op,
    /// - any non-success result code (carried verbatim via
    ///   [`NatPmpResponseCode`] so the caller can react with typed
    ///   logic rather than substring matching).
    pub fn decode(bytes: &[u8]) -> Result<Self, NatPmpDecodeError> {
        if bytes.len() != 16 {
            return Err(NatPmpDecodeError::WrongLength(bytes.len()));
        }
        if bytes[0] != VERSION {
            return Err(NatPmpDecodeError::WrongVersion(bytes[0]));
        }
        let op = bytes[1];
        if op != 0x80 | OP_MAP_UDP && op != 0x80 | OP_MAP_TCP {
            return Err(NatPmpDecodeError::UnknownOp(op));
        }
        let protocol = Protocol::from_op(op).ok_or(NatPmpDecodeError::UnknownOp(op))?;
        let code = u16::from_be_bytes([bytes[2], bytes[3]]);
        let typed_code = NatPmpResponseCode::from_u16(code);
        if !matches!(typed_code, NatPmpResponseCode::Success) {
            return Err(NatPmpDecodeError::ErrorCode(typed_code));
        }
        let epoch_seconds = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let internal_port = u16::from_be_bytes([bytes[8], bytes[9]]);
        let external_port = u16::from_be_bytes([bytes[10], bytes[11]]);
        let lifetime_seconds = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        Ok(Self {
            protocol,
            epoch_seconds,
            internal_port,
            external_port,
            lifetime_seconds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_encodes_udp_with_lifetime() {
        let req = NatPmpRequest {
            protocol: Protocol::Udp,
            internal_port: 0,
            external_port_hint: 0,
            lifetime_seconds: 60,
        };
        let bytes = req.encode();
        assert_eq!(bytes[0], 0); // version
        assert_eq!(bytes[1], OP_MAP_UDP); // op
        assert_eq!(bytes[2..4], [0, 0]); // reserved
        assert_eq!(bytes[4..6], [0, 0]); // internal port
        assert_eq!(bytes[6..8], [0, 0]); // external port hint
        assert_eq!(
            u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            60
        );
    }

    #[test]
    fn request_encodes_tcp_with_nonzero_ports() {
        let req = NatPmpRequest {
            protocol: Protocol::Tcp,
            internal_port: 51820,
            external_port_hint: 12345,
            lifetime_seconds: 7200,
        };
        let bytes = req.encode();
        assert_eq!(bytes[1], OP_MAP_TCP);
        assert_eq!(u16::from_be_bytes([bytes[4], bytes[5]]), 51820);
        assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 12345);
        assert_eq!(
            u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            7200
        );
    }

    #[test]
    fn response_decode_success_udp() {
        // Version 0, Op 0x81 (UDP response), Code 0, Epoch 0x0001_0000,
        // Internal 0x1234, External 0x5678, Lifetime 60.
        let mut bytes = [0u8; 16];
        bytes[0] = 0;
        bytes[1] = 0x80 | OP_MAP_UDP;
        bytes[2..4].copy_from_slice(&0u16.to_be_bytes());
        bytes[4..8].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        bytes[8..10].copy_from_slice(&0x1234u16.to_be_bytes());
        bytes[10..12].copy_from_slice(&0x5678u16.to_be_bytes());
        bytes[12..16].copy_from_slice(&60u32.to_be_bytes());

        let lease = NatPmpLease::decode(&bytes).expect("success response should decode");
        assert_eq!(lease.protocol, Protocol::Udp);
        assert_eq!(lease.epoch_seconds, 0x0001_0000);
        assert_eq!(lease.internal_port, 0x1234);
        assert_eq!(lease.external_port, 0x5678);
        assert_eq!(lease.lifetime_seconds, 60);
    }

    #[test]
    fn response_decode_success_tcp() {
        let mut bytes = [0u8; 16];
        bytes[0] = 0;
        bytes[1] = 0x80 | OP_MAP_TCP;
        bytes[4..8].copy_from_slice(&7u32.to_be_bytes());
        bytes[8..10].copy_from_slice(&80u16.to_be_bytes());
        bytes[10..12].copy_from_slice(&8080u16.to_be_bytes());
        bytes[12..16].copy_from_slice(&3600u32.to_be_bytes());

        let lease = NatPmpLease::decode(&bytes).expect("success response should decode");
        assert_eq!(lease.protocol, Protocol::Tcp);
        assert_eq!(lease.external_port, 8080);
    }

    #[test]
    fn response_decode_rejects_wrong_length() {
        let result = NatPmpLease::decode(&[0u8; 12]);
        assert_eq!(result.unwrap_err(), NatPmpDecodeError::WrongLength(12));
    }

    #[test]
    fn response_decode_rejects_wrong_version() {
        let mut bytes = [0u8; 16];
        bytes[0] = 42;
        bytes[1] = 0x80 | OP_MAP_UDP;
        let result = NatPmpLease::decode(&bytes);
        assert_eq!(result.unwrap_err(), NatPmpDecodeError::WrongVersion(42));
    }

    #[test]
    fn response_decode_rejects_unknown_op() {
        let mut bytes = [0u8; 16];
        bytes[1] = 0xaa; // unknown response op
        let result = NatPmpLease::decode(&bytes);
        assert_eq!(result.unwrap_err(), NatPmpDecodeError::UnknownOp(0xaa));
    }

    #[test]
    fn response_decode_surfaces_typed_error_codes() {
        let cases = [
            (1u16, NatPmpResponseCode::UnsupportedVersion),
            (2, NatPmpResponseCode::NotAuthorized),
            (3, NatPmpResponseCode::NetworkFailure),
            (4, NatPmpResponseCode::OutOfResources),
            (5, NatPmpResponseCode::UnsupportedOpcode),
            (99, NatPmpResponseCode::Other(99)),
        ];
        for (code, expected) in cases {
            let mut bytes = [0u8; 16];
            bytes[0] = 0;
            bytes[1] = 0x80 | OP_MAP_UDP;
            bytes[2..4].copy_from_slice(&code.to_be_bytes());
            let err = NatPmpLease::decode(&bytes).unwrap_err();
            assert_eq!(
                err,
                NatPmpDecodeError::ErrorCode(expected),
                "code {code} should map to {expected:?}"
            );
        }
    }

    #[test]
    fn response_code_from_u16_known_values() {
        assert_eq!(NatPmpResponseCode::from_u16(0), NatPmpResponseCode::Success);
        assert_eq!(
            NatPmpResponseCode::from_u16(2),
            NatPmpResponseCode::NotAuthorized
        );
        assert_eq!(
            NatPmpResponseCode::from_u16(12345),
            NatPmpResponseCode::Other(12345)
        );
    }

    #[test]
    fn round_trip_through_encode_and_decode() {
        // Encode a request, then synthesize a matching response and
        // decode it.  The two formats differ but exercising both
        // confirms the encode/decode bytes work coherently in tests.
        let req = NatPmpRequest {
            protocol: Protocol::Udp,
            internal_port: 0,
            external_port_hint: 0,
            lifetime_seconds: 60,
        };
        let encoded = req.encode();
        assert_eq!(encoded.len(), 12);

        let mut response = [0u8; 16];
        response[0] = 0;
        response[1] = 0x80 | encoded[1];
        response[4..8].copy_from_slice(&100u32.to_be_bytes());
        response[8..10].copy_from_slice(&encoded[4..6]); // echo internal
        response[10..12].copy_from_slice(&42000u16.to_be_bytes());
        response[12..16].copy_from_slice(&60u32.to_be_bytes());
        let lease = NatPmpLease::decode(&response).unwrap();
        assert_eq!(lease.protocol, req.protocol);
        assert_eq!(lease.external_port, 42000);
    }
}
