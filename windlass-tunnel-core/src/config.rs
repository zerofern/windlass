//! Parser and validated representation of a `ProtonVPN` `wg.conf` file.
//!
//! Format reference: `wg-quick(8)` configuration file.  `ProtonVPN`'s
//! generated configs have a single `[Interface]` section and one or
//! more `[Peer]` sections.  Lines are `key = value` pairs; everything
//! after `#` is a comment; section headers are bracketed names.
//!
//! ## What this module owns
//!
//! - The byte-level parse from a string of file content.
//! - Structural validation: required keys present, key formats
//!   parseable (base64 32-byte private/public keys, IP addresses,
//!   ports, CIDR notation).
//! - Endpoint extraction with an explicit
//!   [`EndpointResolutionPolicy`] choice for hostname-vs-literal — the
//!   policy is supplied by the caller (set in deployment) so the
//!   parser can reject a hostname endpoint when only IP literals are
//!   permitted (see `docs/vpn-ownership.md` external requirements).
//!
//! ## What this module does NOT own
//!
//! - Any I/O.  The caller passes file content as `&str`.
//! - DNS resolution.  When the endpoint is a hostname and the
//!   resolution policy allows it, the parser preserves the hostname
//!   verbatim; the shell does the resolution at a controlled time
//!   with the firewall in the correct state.
//! - Cryptographic validation.  We check key *length* and *base64
//!   shape*, not whether the key is a valid Curve25519 point — that
//!   is the kernel's job at `wg set` time.

use std::net::{AddrParseError, IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::num::ParseIntError;

use secrecy::SecretString;
use thiserror::Error;

/// A validated `ProtonVPN` `WireGuard` configuration.
///
/// Constructed by [`WgConfig::parse`].  Field-level invariants:
///
/// - `interface.private_key` is 32 bytes when base64-decoded.
/// - `interface.address` contains at least one IPv4 or IPv6 address.
/// - `peers` is non-empty.  Each peer has a 32-byte public key and a
///   parseable endpoint.
#[derive(Debug)]
pub struct WgConfig {
    pub interface: InterfaceConfig,
    pub peers: Vec<PeerConfig>,
}

/// Parsed `[Interface]` section.
#[derive(Debug)]
pub struct InterfaceConfig {
    /// Curve25519 private key, base64-encoded in the file.  Held as
    /// [`SecretString`] so it cannot reach a generic `Serialize` impl
    /// or appear in `Debug` output.
    pub private_key: SecretString,
    /// Addresses to assign to the `WireGuard` interface.  Each is an IP
    /// with an optional prefix length; we preserve both so the shell
    /// can configure the interface exactly as written.
    pub addresses: Vec<InterfaceAddress>,
    /// DNS servers from the `DNS =` directive, if any.  The shell
    /// writes these to the namespace resolver after the tunnel is up.
    pub dns_servers: Vec<IpAddr>,
    /// Optional `MTU =` override.
    pub mtu: Option<u16>,
    /// Optional `ListenPort =` override (Proton's configs typically
    /// omit it — the kernel picks one).
    pub listen_port: Option<u16>,
}

/// One IP address from an `Address =` directive, with the prefix
/// length the operator wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceAddress {
    pub ip: IpAddr,
    pub prefix_len: u8,
}

/// Parsed `[Peer]` section.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    /// Curve25519 public key, base64-encoded in the file.  Not a
    /// secret; carried as a plain string so it can appear in logs and
    /// the observability surface.
    pub public_key: String,
    /// Optional pre-shared key, base64-encoded.  Secret if present.
    pub preshared_key: Option<SecretString>,
    /// CIDRs this peer is allowed to send/receive through the tunnel.
    /// `0.0.0.0/0` and `::/0` are the default-route entries.
    pub allowed_ips: Vec<AllowedIp>,
    /// `Endpoint = host:port`.  See [`Endpoint`] for the IP-vs-hostname
    /// distinction.
    pub endpoint: Endpoint,
    /// Optional `PersistentKeepalive =` in seconds.
    pub persistent_keepalive: Option<u16>,
}

/// A CIDR from an `AllowedIPs =` directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowedIp {
    pub ip: IpAddr,
    pub prefix_len: u8,
}

/// A peer endpoint.  Distinguishes IP literals from hostnames so the
/// caller can apply [`EndpointResolutionPolicy`].
///
/// `port` is always present — `WireGuard` endpoints without a port are
/// not valid `wg-quick` configurations.
#[derive(Debug, Clone)]
pub enum Endpoint {
    /// Literal IP — no DNS work needed, the shell can write the
    /// endpoint into the kernel directly.
    Ip(SocketAddr),
    /// Hostname — the shell must resolve it under whatever pre-tunnel
    /// allowlist the deployment provides.
    Hostname { host: String, port: u16 },
}

/// How to handle a hostname endpoint at parse time.
///
/// Set by the deployment to lock the policy in one place.  Each
/// variant maps to the three acceptable policies in
/// `docs/vpn-ownership.md` external requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointResolutionPolicy {
    /// Reject hostnames at parse time.  Operator must use an IP
    /// literal in `wg.conf`.  Strongest leak prevention; least
    /// flexible.
    RequireIpLiteral,
    /// Allow hostnames; the shell will resolve them through a
    /// pre-tunnel DNS allowlist before the tunnel is up.
    AllowHostnamePreTunnelDns,
}

/// Errors produced by [`WgConfig::parse`].
///
/// Carries enough context for the boot-time "refuses to start with a
/// specific message" acceptance criterion in
/// `docs/vpn-ownership.md`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WgConfigError {
    #[error("config is empty")]
    Empty,
    #[error("unknown section header `[{0}]` (expected `Interface` or `Peer`)")]
    UnknownSection(String),
    #[error("line {line}: expected `key = value` but got `{raw}`")]
    Malformed { line: usize, raw: String },
    #[error("line {line}: key `{key}` outside any section")]
    KeyOutsideSection { line: usize, key: String },
    #[error("line {line}: unknown key `{key}` in [{section}]")]
    UnknownKey {
        line: usize,
        section: String,
        key: String,
    },
    #[error("line {line}: invalid value for `{key}`: {reason}")]
    InvalidValue {
        line: usize,
        key: String,
        reason: String,
    },
    #[error("[Interface] is missing required key `{0}`")]
    MissingInterfaceKey(&'static str),
    #[error("[Peer] (section {peer_index}) is missing required key `{key}`")]
    MissingPeerKey {
        peer_index: usize,
        key: &'static str,
    },
    #[error("config has no [Peer] sections")]
    NoPeers,
    #[error("endpoint `{0}` is a hostname but the deployment requires an IP literal")]
    HostnameEndpointForbidden(String),
    #[error("private key is not valid base64 of a 32-byte key: {0}")]
    InvalidPrivateKey(String),
    #[error("public key for peer {peer_index} is not valid base64 of a 32-byte key: {reason}")]
    InvalidPublicKey { peer_index: usize, reason: String },
}

impl WgConfig {
    /// Parses `wg-quick`-style content with the given hostname
    /// resolution policy.
    ///
    /// Returns a [`WgConfigError`] with a one-line human-readable
    /// `Display` so a boot-time failure surfaces the exact problem
    /// to the operator's log.
    ///
    /// # Errors
    ///
    /// Returns one of the [`WgConfigError`] variants if the input is
    /// empty, malformed, missing required keys, or violates the
    /// supplied [`EndpointResolutionPolicy`].
    ///
    /// # Panics
    ///
    /// Does not panic in practice: the only internal `expect` runs after
    /// a guaranteed `peers.push(...)`, so the `last_mut()` cannot return
    /// `None`.  Documented to satisfy clippy's `missing_panics_doc`.
    pub fn parse(content: &str, policy: EndpointResolutionPolicy) -> Result<Self, WgConfigError> {
        let mut current_section: Option<Section> = None;
        let mut interface_builder = InterfaceBuilder::default();
        let mut peers: Vec<PeerBuilder> = Vec::new();
        let mut any_section_seen = false;

        for (idx, raw_line) in content.lines().enumerate() {
            let line_number = idx + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }

            if let Some(name) = parse_section_header(line) {
                let section = match name {
                    "Interface" => Section::Interface,
                    "Peer" => {
                        peers.push(PeerBuilder::default());
                        Section::Peer
                    }
                    other => return Err(WgConfigError::UnknownSection(other.to_string())),
                };
                current_section = Some(section);
                any_section_seen = true;
                continue;
            }

            let (key, value) = split_key_value(line).ok_or_else(|| WgConfigError::Malformed {
                line: line_number,
                raw: raw_line.to_string(),
            })?;

            match current_section {
                None => {
                    return Err(WgConfigError::KeyOutsideSection {
                        line: line_number,
                        key: key.to_string(),
                    });
                }
                Some(Section::Interface) => {
                    interface_builder.set(key, value, line_number)?;
                }
                Some(Section::Peer) => {
                    let peer_index = peers.len() - 1;
                    let peer = peers
                        .last_mut()
                        .expect("Peer section pushed before key parse");
                    peer.set(key, value, line_number, peer_index, policy)?;
                }
            }
        }

        if !any_section_seen {
            return Err(WgConfigError::Empty);
        }
        let interface = interface_builder.build()?;
        if peers.is_empty() {
            return Err(WgConfigError::NoPeers);
        }
        let peers = peers
            .into_iter()
            .enumerate()
            .map(|(i, b)| b.build(i))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { interface, peers })
    }
}

// ── Internal builders ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Section {
    Interface,
    Peer,
}

#[derive(Default)]
struct InterfaceBuilder {
    private_key: Option<String>,
    addresses: Vec<InterfaceAddress>,
    dns_servers: Vec<IpAddr>,
    mtu: Option<u16>,
    listen_port: Option<u16>,
}

impl InterfaceBuilder {
    fn set(&mut self, key: &str, value: &str, line: usize) -> Result<(), WgConfigError> {
        match key {
            "PrivateKey" => {
                self.private_key = Some(value.to_string());
            }
            "Address" => {
                for piece in value.split(',') {
                    let piece = piece.trim();
                    if piece.is_empty() {
                        continue;
                    }
                    let addr = parse_cidr(piece).map_err(|reason| WgConfigError::InvalidValue {
                        line,
                        key: "Address".to_string(),
                        reason,
                    })?;
                    self.addresses.push(InterfaceAddress {
                        ip: addr.ip,
                        prefix_len: addr.prefix_len,
                    });
                }
            }
            "DNS" => {
                for piece in value.split(',') {
                    let piece = piece.trim();
                    if piece.is_empty() {
                        continue;
                    }
                    let ip = piece.parse::<IpAddr>().map_err(|e: AddrParseError| {
                        WgConfigError::InvalidValue {
                            line,
                            key: "DNS".to_string(),
                            reason: e.to_string(),
                        }
                    })?;
                    self.dns_servers.push(ip);
                }
            }
            "MTU" => {
                self.mtu = Some(value.parse::<u16>().map_err(|e: ParseIntError| {
                    WgConfigError::InvalidValue {
                        line,
                        key: "MTU".to_string(),
                        reason: e.to_string(),
                    }
                })?);
            }
            "ListenPort" => {
                self.listen_port = Some(value.parse::<u16>().map_err(|e: ParseIntError| {
                    WgConfigError::InvalidValue {
                        line,
                        key: "ListenPort".to_string(),
                        reason: e.to_string(),
                    }
                })?);
            }
            // wg-quick allows extra keys like PreUp/PostUp/Table; we ignore
            // them rather than failing, because Proton has been observed
            // to add `Table = off` and similar.  Unknown keys we do not
            // understand are silently dropped at this layer — the shell
            // will only act on what `WgConfig` exposes.
            "PreUp" | "PostUp" | "PreDown" | "PostDown" | "Table" | "FwMark" | "SaveConfig" => {}
            other => {
                return Err(WgConfigError::UnknownKey {
                    line,
                    section: "Interface".to_string(),
                    key: other.to_string(),
                });
            }
        }
        Ok(())
    }

    fn build(self) -> Result<InterfaceConfig, WgConfigError> {
        let private_key_raw = self
            .private_key
            .ok_or(WgConfigError::MissingInterfaceKey("PrivateKey"))?;
        validate_base64_key(&private_key_raw).map_err(WgConfigError::InvalidPrivateKey)?;
        if self.addresses.is_empty() {
            return Err(WgConfigError::MissingInterfaceKey("Address"));
        }
        Ok(InterfaceConfig {
            private_key: SecretString::from(private_key_raw),
            addresses: self.addresses,
            dns_servers: self.dns_servers,
            mtu: self.mtu,
            listen_port: self.listen_port,
        })
    }
}

#[derive(Default)]
struct PeerBuilder {
    public_key: Option<String>,
    preshared_key: Option<String>,
    allowed_ips: Vec<AllowedIp>,
    endpoint: Option<Endpoint>,
    persistent_keepalive: Option<u16>,
}

impl PeerBuilder {
    fn set(
        &mut self,
        key: &str,
        value: &str,
        line: usize,
        peer_index: usize,
        policy: EndpointResolutionPolicy,
    ) -> Result<(), WgConfigError> {
        match key {
            "PublicKey" => {
                self.public_key = Some(value.to_string());
            }
            "PresharedKey" => {
                self.preshared_key = Some(value.to_string());
            }
            "AllowedIPs" => {
                for piece in value.split(',') {
                    let piece = piece.trim();
                    if piece.is_empty() {
                        continue;
                    }
                    let cidr = parse_cidr(piece).map_err(|reason| WgConfigError::InvalidValue {
                        line,
                        key: "AllowedIPs".to_string(),
                        reason,
                    })?;
                    self.allowed_ips.push(AllowedIp {
                        ip: cidr.ip,
                        prefix_len: cidr.prefix_len,
                    });
                }
            }
            "Endpoint" => {
                self.endpoint = Some(parse_endpoint(value, policy).map_err(|reason| {
                    WgConfigError::InvalidValue {
                        line,
                        key: "Endpoint".to_string(),
                        reason,
                    }
                })?);
            }
            "PersistentKeepalive" => {
                self.persistent_keepalive =
                    Some(value.parse::<u16>().map_err(|e: ParseIntError| {
                        WgConfigError::InvalidValue {
                            line,
                            key: "PersistentKeepalive".to_string(),
                            reason: e.to_string(),
                        }
                    })?);
            }
            other => {
                return Err(WgConfigError::UnknownKey {
                    line,
                    section: "Peer".to_string(),
                    key: other.to_string(),
                });
            }
        }
        // peer_index is included so unknown-key errors could reference it;
        // currently we surface via section name only, but the parameter is
        // kept for symmetry with `build`.
        let _ = peer_index;
        Ok(())
    }

    fn build(self, peer_index: usize) -> Result<PeerConfig, WgConfigError> {
        let public_key = self.public_key.ok_or(WgConfigError::MissingPeerKey {
            peer_index,
            key: "PublicKey",
        })?;
        validate_base64_key(&public_key)
            .map_err(|reason| WgConfigError::InvalidPublicKey { peer_index, reason })?;
        if self.allowed_ips.is_empty() {
            return Err(WgConfigError::MissingPeerKey {
                peer_index,
                key: "AllowedIPs",
            });
        }
        let endpoint = self.endpoint.ok_or(WgConfigError::MissingPeerKey {
            peer_index,
            key: "Endpoint",
        })?;
        Ok(PeerConfig {
            public_key,
            preshared_key: self.preshared_key.map(SecretString::from),
            allowed_ips: self.allowed_ips,
            endpoint,
            persistent_keepalive: self.persistent_keepalive,
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

struct ParsedCidr {
    ip: IpAddr,
    prefix_len: u8,
}

fn strip_comment(line: &str) -> &str {
    line.split('#').next().unwrap_or(line)
}

fn parse_section_header(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let stripped = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    let name = stripped.trim();
    if name.is_empty() { None } else { Some(name) }
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

fn parse_cidr(raw: &str) -> Result<ParsedCidr, String> {
    if let Some((ip_part, prefix_part)) = raw.split_once('/') {
        let ip: IpAddr = ip_part.parse().map_err(|e: AddrParseError| e.to_string())?;
        let prefix_len: u8 = prefix_part
            .parse()
            .map_err(|e: ParseIntError| e.to_string())?;
        let max = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix_len > max {
            return Err(format!("prefix {prefix_len} exceeds max for IP version"));
        }
        Ok(ParsedCidr { ip, prefix_len })
    } else {
        let ip: IpAddr = raw.parse().map_err(|e: AddrParseError| e.to_string())?;
        let prefix_len = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        Ok(ParsedCidr { ip, prefix_len })
    }
}

fn parse_endpoint(raw: &str, policy: EndpointResolutionPolicy) -> Result<Endpoint, String> {
    // IPv6 endpoints in `wg-quick` use bracketed form: `[::1]:51820`.
    if let Some(rest) = raw.strip_prefix('[') {
        let (addr, port) = rest
            .split_once("]:")
            .ok_or_else(|| "expected `[ipv6]:port`".to_string())?;
        let ip: Ipv6Addr = addr.parse().map_err(|e: AddrParseError| e.to_string())?;
        let port: u16 = port.parse().map_err(|e: ParseIntError| e.to_string())?;
        return Ok(Endpoint::Ip(SocketAddr::new(IpAddr::V6(ip), port)));
    }

    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| "expected `host:port`".to_string())?;
    let port: u16 = port.parse().map_err(|e: ParseIntError| e.to_string())?;

    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(Endpoint::Ip(SocketAddr::new(IpAddr::V4(ip), port)));
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        return Ok(Endpoint::Ip(SocketAddr::new(IpAddr::V6(ip), port)));
    }
    match policy {
        EndpointResolutionPolicy::RequireIpLiteral => Err(format!(
            "endpoint `{host}` is a hostname; policy requires IP literal"
        )),
        EndpointResolutionPolicy::AllowHostnamePreTunnelDns => Ok(Endpoint::Hostname {
            host: host.to_string(),
            port,
        }),
    }
}

/// Validates a base64-encoded `WireGuard` key.  We accept the standard
/// alphabet (with `+/` and `=` padding) and verify the decoded length
/// is 32 bytes.  We do not verify that the key represents a valid
/// `Curve25519` point; the kernel will reject invalid keys at `wg set`
/// time.
///
/// Inline base64 decode (no external dep) — `WireGuard` keys are exactly
/// 44 base64 characters (32 bytes encoded, including `=` padding).
fn validate_base64_key(input: &str) -> Result<(), String> {
    let trimmed = input.trim();
    if trimmed.len() != 44 {
        return Err(format!(
            "expected 44 base64 characters, got {}",
            trimmed.len()
        ));
    }
    if !trimmed.ends_with('=') {
        return Err("expected base64 `=` padding".to_string());
    }
    let bytes = decode_base64_44(trimmed)?;
    if bytes.len() != 32 {
        return Err(format!(
            "decoded length is {} bytes, expected 32",
            bytes.len()
        ));
    }
    Ok(())
}

/// Decodes 44 base64 characters into 32 bytes.  Inline implementation
/// so the parser has no base64 dependency.
fn decode_base64_44(input: &str) -> Result<Vec<u8>, String> {
    // Standard base64 alphabet.
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (i, c) in ALPHA.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        {
            table[*c as usize] = i as u8;
        }
    }
    table[b'=' as usize] = 0;

    let bytes = input.as_bytes();
    // 44 characters → 11 groups of 4 → 33 bytes raw, minus `=` padding.
    // A 32-byte key encodes as 43 data chars + 1 `=`.
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/' || *b == b'=')
    {
        return Err("contains non-base64 character".to_string());
    }
    let mut out = Vec::with_capacity(33);
    for chunk in bytes.chunks(4) {
        let v0 = u32::from(table[chunk[0] as usize]);
        let v1 = u32::from(table[chunk[1] as usize]);
        let v2 = u32::from(table[chunk[2] as usize]);
        let v3 = u32::from(table[chunk[3] as usize]);
        if v0 > 63 || v1 > 63 || v2 > 64 || v3 > 64 {
            return Err("invalid base64 character".to_string());
        }
        let combined = (v0 << 18) | (v1 << 12) | (v2 << 6) | v3;
        #[allow(clippy::cast_possible_truncation)]
        {
            out.push((combined >> 16) as u8);
            if chunk[2] != b'=' {
                out.push((combined >> 8) as u8);
            }
            if chunk[3] != b'=' {
                out.push(combined as u8);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    /// A realistic, syntactically valid ProtonVPN-style config.  The
    /// keys are placeholders that happen to be 44 base64 chars / 32
    /// raw bytes, not real keys.
    const VALID_CONFIG: &str = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32
DNS = 10.2.0.1

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";

    #[test]
    fn parses_minimal_valid_config() {
        let cfg = WgConfig::parse(VALID_CONFIG, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("valid config should parse");
        assert_eq!(cfg.interface.addresses.len(), 1);
        assert_eq!(cfg.interface.dns_servers.len(), 1);
        assert_eq!(cfg.peers.len(), 1);
        match &cfg.peers[0].endpoint {
            Endpoint::Ip(addr) => {
                assert_eq!(addr.port(), 51820);
                assert_eq!(addr.ip().to_string(), "198.51.100.7");
            }
            Endpoint::Hostname { .. } => panic!("expected IP endpoint"),
        }
    }

    #[test]
    fn private_key_is_secret_and_not_in_debug() {
        let cfg = WgConfig::parse(VALID_CONFIG, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("valid config should parse");
        // The cleartext is reachable via expose_secret only.
        assert_eq!(
            cfg.interface.private_key.expose_secret(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        );
        // The Debug output of the WgConfig must not contain the cleartext.
        let dbg = format!("{cfg:?}");
        assert!(
            !dbg.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            "Debug leaked private key: {dbg}"
        );
    }

    #[test]
    fn empty_input_is_rejected() {
        let result = WgConfig::parse("", EndpointResolutionPolicy::RequireIpLiteral);
        assert_eq!(result.err(), Some(WgConfigError::Empty));
    }

    #[test]
    fn whitespace_only_input_is_rejected_as_empty() {
        let result = WgConfig::parse(
            "   \n\n  # only a comment\n",
            EndpointResolutionPolicy::RequireIpLiteral,
        );
        assert_eq!(result.err(), Some(WgConfigError::Empty));
    }

    #[test]
    fn missing_private_key_is_named_in_error() {
        let content = "\
[Interface]
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("should fail");
        assert_eq!(err, WgConfigError::MissingInterfaceKey("PrivateKey"));
    }

    #[test]
    fn missing_address_is_named_in_error() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("should fail");
        assert_eq!(err, WgConfigError::MissingInterfaceKey("Address"));
    }

    #[test]
    fn no_peer_section_is_rejected() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32
";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("should fail");
        assert_eq!(err, WgConfigError::NoPeers);
    }

    #[test]
    fn hostname_endpoint_rejected_when_policy_requires_ip() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = nl-free-123.protonvpn.net:51820
";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("should fail");
        match err {
            WgConfigError::InvalidValue { key, .. } => {
                assert_eq!(key, "Endpoint");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn hostname_endpoint_allowed_when_policy_permits() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = nl-free-123.protonvpn.net:51820
";
        let cfg = WgConfig::parse(content, EndpointResolutionPolicy::AllowHostnamePreTunnelDns)
            .expect("hostname endpoint should be accepted under permissive policy");
        match &cfg.peers[0].endpoint {
            Endpoint::Hostname { host, port } => {
                assert_eq!(host, "nl-free-123.protonvpn.net");
                assert_eq!(*port, 51820);
            }
            Endpoint::Ip(_) => panic!("expected Hostname endpoint"),
        }
    }

    #[test]
    fn ipv6_endpoint_parses() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = [2001:db8::1]:51820
";
        let cfg = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("valid IPv6 endpoint should parse");
        match &cfg.peers[0].endpoint {
            Endpoint::Ip(addr) => {
                assert!(addr.is_ipv6());
                assert_eq!(addr.port(), 51820);
            }
            Endpoint::Hostname { .. } => panic!("expected IP endpoint"),
        }
    }

    #[test]
    fn multiple_peers_parsed_in_order() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820

[Peer]
PublicKey = CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.8:51820
";
        let cfg = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("two-peer config should parse");
        assert_eq!(cfg.peers.len(), 2);
        assert_eq!(
            cfg.peers[0].public_key,
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB="
        );
        assert_eq!(
            cfg.peers[1].public_key,
            "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC="
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let content = "\
# top of file comment
# another

[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=  # inline comment
Address = 10.2.0.2/32

# between sections

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let cfg = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("comments and blanks should be tolerated");
        assert_eq!(cfg.peers.len(), 1);
    }

    #[test]
    fn key_outside_section_is_rejected() {
        let content = "PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("orphan key should fail");
        match err {
            WgConfigError::KeyOutsideSection { line: 1, key } => {
                assert_eq!(key, "PrivateKey");
            }
            other => panic!("expected KeyOutsideSection, got {other:?}"),
        }
    }

    #[test]
    fn unknown_section_is_rejected() {
        let content = "[NotASection]\n";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("unknown section should fail");
        match err {
            WgConfigError::UnknownSection(name) => assert_eq!(name, "NotASection"),
            other => panic!("expected UnknownSection, got {other:?}"),
        }
    }

    #[test]
    fn invalid_base64_private_key_is_rejected() {
        let content = "\
[Interface]
PrivateKey = not-base64
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("invalid key should fail");
        assert!(matches!(err, WgConfigError::InvalidPrivateKey(_)));
    }

    #[test]
    fn allowed_ips_default_route_v4_and_v6() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = 198.51.100.7:51820
";
        let cfg = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("dual-stack allowed-ips should parse");
        assert_eq!(cfg.peers[0].allowed_ips.len(), 2);
    }

    #[test]
    fn invalid_cidr_prefix_is_rejected() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/99

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("invalid prefix should fail");
        assert!(matches!(
            err,
            WgConfigError::InvalidValue { key, .. } if key == "Address"
        ));
    }

    #[test]
    fn persistent_keepalive_parsed() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
PersistentKeepalive = 25
";
        let cfg = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("keepalive should parse");
        assert_eq!(cfg.peers[0].persistent_keepalive, Some(25));
    }

    #[test]
    fn wg_quick_ignored_keys_do_not_error() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32
Table = off
PostUp = some-hook-command

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let _ = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("wg-quick directives should be ignored");
    }

    #[test]
    fn unknown_interface_key_is_rejected() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32
NotAKey = something

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let err = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral)
            .expect_err("unknown key should fail");
        assert!(matches!(err, WgConfigError::UnknownKey { .. }));
    }
}
