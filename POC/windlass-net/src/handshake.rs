//! Parser for `wg show <iface> latest-handshakes`.
//!
//! `wg(8)`'s `latest-handshakes` mode prints one line per peer with the
//! peer's base64 public key, a tab, and the unix timestamp of the most
//! recent handshake (`0` if never).  Example:
//!
//! ```text
//! PEERKEY1=    1718380000
//! PEERKEY2=    0
//! ```
//!
//! This module is the pure parser: bytes in, typed
//! [`HandshakeAge`] out.  The subprocess call lives in
//! [`crate::shell`].

use chrono::{DateTime, Utc};
use thiserror::Error;

/// Age of the latest handshake observed on the kernel interface,
/// computed against a caller-supplied `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeAge {
    /// A handshake has occurred and the time delta from `now` is
    /// known.  Negative deltas (clock skew) are clamped to zero.
    Observed { age_seconds: u64 },
    /// `wg show` reported `0` for this peer — never seen a handshake.
    NeverHandshook,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HandshakeParseError {
    #[error("no peer found matching public key `{0}`")]
    PeerNotFound(String),
    #[error("malformed `wg show latest-handshakes` line: `{0}`")]
    MalformedLine(String),
}

/// Parses `wg show <iface> latest-handshakes` output, locates the row
/// for `peer_public_key`, and computes the age against `now`.
///
/// # Errors
///
/// Returns [`HandshakeParseError::PeerNotFound`] if the public key
/// isn't in the output (peer not configured, or wg show couldn't read
/// the interface).  Returns [`HandshakeParseError::MalformedLine`] if
/// a line in the output isn't shaped `<key>\t<u64>`.
pub fn latest_handshake_age(
    output: &str,
    peer_public_key: &str,
    now: DateTime<Utc>,
) -> Result<HandshakeAge, HandshakeParseError> {
    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // `wg show` separates with a tab; some configurations show
        // multiple whitespace.  We accept any whitespace.
        let mut parts = line.split_whitespace();
        let key = parts
            .next()
            .ok_or_else(|| HandshakeParseError::MalformedLine(line.to_string()))?;
        let ts = parts
            .next()
            .ok_or_else(|| HandshakeParseError::MalformedLine(line.to_string()))?;
        if key != peer_public_key {
            continue;
        }
        let secs: i64 = ts
            .parse()
            .map_err(|_| HandshakeParseError::MalformedLine(line.to_string()))?;
        if secs == 0 {
            return Ok(HandshakeAge::NeverHandshook);
        }
        let observed = DateTime::<Utc>::from_timestamp(secs, 0)
            .ok_or_else(|| HandshakeParseError::MalformedLine(line.to_string()))?;
        let delta = now.signed_duration_since(observed).num_seconds();
        let age_seconds = u64::try_from(delta).unwrap_or(0);
        return Ok(HandshakeAge::Observed { age_seconds });
    }
    Err(HandshakeParseError::PeerNotFound(
        peer_public_key.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn ts(s: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(s, 0).unwrap()
    }

    #[test]
    fn observed_handshake_age_is_now_minus_recorded() {
        let output = "PEERKEY1=\t1000\n";
        let age = latest_handshake_age(output, "PEERKEY1=", ts(1060)).unwrap();
        assert_eq!(age, HandshakeAge::Observed { age_seconds: 60 });
    }

    #[test]
    fn zero_timestamp_is_never_handshook() {
        let output = "PEERKEY1=\t0\n";
        let age = latest_handshake_age(output, "PEERKEY1=", ts(1000)).unwrap();
        assert_eq!(age, HandshakeAge::NeverHandshook);
    }

    #[test]
    fn future_timestamp_clamps_to_zero_age() {
        // Clock skew — kernel timestamp ahead of our `now`.  We do
        // not panic and do not surface a negative duration.
        let output = "PEERKEY1=\t2000\n";
        let age = latest_handshake_age(output, "PEERKEY1=", ts(1000)).unwrap();
        assert_eq!(age, HandshakeAge::Observed { age_seconds: 0 });
    }

    #[test]
    fn missing_peer_is_typed_error() {
        let output = "OTHERKEY=\t1000\n";
        let err = latest_handshake_age(output, "MYKEY=", ts(1500)).unwrap_err();
        assert_eq!(err, HandshakeParseError::PeerNotFound("MYKEY=".into()));
    }

    #[test]
    fn malformed_line_is_typed_error() {
        let output = "no-timestamp-here\n";
        let err = latest_handshake_age(output, "MYKEY=", ts(1500)).unwrap_err();
        assert!(matches!(err, HandshakeParseError::MalformedLine(_)));
    }

    #[test]
    fn picks_matching_peer_among_many() {
        let output = "\
PEERONE=\t1000
PEERTWO=\t2000
PEERTHREE=\t0
";
        let age = latest_handshake_age(output, "PEERTWO=", ts(2500)).unwrap();
        assert_eq!(age, HandshakeAge::Observed { age_seconds: 500 });
        let age2 = latest_handshake_age(output, "PEERTHREE=", ts(3000)).unwrap();
        assert_eq!(age2, HandshakeAge::NeverHandshook);
    }

    #[test]
    fn accepts_multiple_whitespace_between_fields() {
        // Tolerate runs of spaces (some wg builds align columns).
        let output = "PEERKEY=    1000\n";
        let age = latest_handshake_age(output, "PEERKEY=", ts(1100)).unwrap();
        assert_eq!(age, HandshakeAge::Observed { age_seconds: 100 });
    }

    #[test]
    fn blank_lines_are_skipped() {
        let output = "\n\nPEERKEY=\t1000\n\n";
        let age = latest_handshake_age(output, "PEERKEY=", ts(1100)).unwrap();
        assert_eq!(age, HandshakeAge::Observed { age_seconds: 100 });
    }
}
