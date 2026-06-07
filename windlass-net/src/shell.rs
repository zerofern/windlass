//! [`TunnelShell`] — the [`windlass_machine::Shell`] impl for the
//! in-process `WireGuard` tunnel.
//!
//! Each [`windlass_tunnel_core::TunnelAction`] is dispatched to a
//! spawned task that performs the I/O (subprocess, UDP, file I/O,
//! etc.) and reports the typed outcome back as a
//! [`windlass_tunnel_core::TunnelEvent`].

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;
use windlass_machine::{ExternalCause, Shell, Timed};
use windlass_tunnel_core::config::{Endpoint, PeerConfig, WgConfig};
use windlass_tunnel_core::natpmp::Protocol;
use windlass_tunnel_core::{NatPmpRequest, TunnelAction, TunnelEvent};
use windlass_types::{CoreId, HttpTap, NullHttpTap};

use crate::command::{Runner, SystemRunner};
use crate::handshake::{HandshakeAge, latest_handshake_age};
use crate::natpmp::NatPmpClient;
use crate::probe::{leak_outcome_from_snapshot, parse_ip_addr_show};

/// Operator-supplied configuration for the shell.
///
/// `WgConfig` is the parsed `wg.conf` file (see
/// [`windlass_tunnel_core::config::WgConfig::parse`]).  Everything
/// else is operational policy.
pub struct TunnelShellConfig {
    pub wg: WgConfig,
    /// Kernel interface name.  Defaults to `wg0`.
    pub interface_name: String,
    /// NAT-PMP gateway socket address — the address NAT-PMP requests
    /// are sent to inside the tunnel.  For `ProtonVPN`, this is
    /// typically `10.2.0.1:5351`.
    pub natpmp_gateway: SocketAddr,
    /// Per-request NAT-PMP timeout.  Default 2 s.
    pub natpmp_timeout: Duration,
    /// Observability tap.  Every subprocess invocation and NAT-PMP
    /// exchange is captured here so the `/observability` ring sees
    /// tunnel ops alongside MAM/qBit HTTP.
    pub tap: Arc<dyn HttpTap>,
}

impl TunnelShellConfig {
    /// Convenience constructor with the `ProtonVPN`-typical defaults
    /// and a no-op observability tap.  Production callers (and
    /// runtime wiring) inject a live [`HttpTap`] before spawn.
    ///
    /// # Panics
    ///
    /// Panics only if the hard-coded NAT-PMP gateway literal
    /// (`10.2.0.1:5351`) fails to parse — that would be a compile-
    /// time error in practice.
    #[must_use]
    pub fn new(wg: WgConfig) -> Self {
        Self {
            wg,
            interface_name: "wg0".to_string(),
            natpmp_gateway: "10.2.0.1:5351".parse().expect("static literal"),
            natpmp_timeout: Duration::from_secs(2),
            tap: NullHttpTap::arc(),
        }
    }

    /// Returns a clone of the supplied [`HttpTap`].
    #[must_use]
    pub fn tap(&self) -> Arc<dyn HttpTap> {
        Arc::clone(&self.tap)
    }
}

pub struct TunnelShell {
    config: Arc<TunnelShellConfig>,
    runner: Arc<dyn Runner>,
    natpmp: Option<Arc<NatPmpClient>>,
}

impl Shell for TunnelShell {
    type Config = TunnelShellConfig;
    type Event = TunnelEvent;
    type Action = TunnelAction;

    async fn new(config: Self::Config, _event_tx: UnboundedSender<Timed<Self::Event>>) -> Self {
        let runner: Arc<dyn Runner> = Arc::new(SystemRunner::new(CoreId::Tunnel, config.tap()));
        // The NAT-PMP client binds a UDP socket; we defer the bind
        // until the first request because the tunnel interface is
        // not yet up at shell-construction time.  `Option<Arc<...>>`
        // lazily initializes on first use.
        Self {
            config: Arc::new(config),
            runner,
            natpmp: None,
        }
    }

    fn dispatch(&mut self, action: TunnelAction, event_tx: &UnboundedSender<Timed<TunnelEvent>>) {
        match action {
            TunnelAction::ConfigureInterface => {
                spawn_configure_interface(
                    self.runner.clone(),
                    self.config.clone(),
                    event_tx.clone(),
                );
            }
            TunnelAction::InstallFirewall => {
                spawn_install_firewall(self.runner.clone(), self.config.clone(), event_tx.clone());
            }
            TunnelAction::PollHandshake => {
                spawn_poll_handshake(self.runner.clone(), self.config.clone(), event_tx.clone());
            }
            TunnelAction::RequestNatPmp => {
                let gateway = self.config.natpmp_gateway;
                let timeout = self.config.natpmp_timeout;
                let existing = self.natpmp.clone();
                let tap = self.config.tap();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let client = match existing {
                        Some(c) => c,
                        None => {
                            match NatPmpClient::new(gateway, timeout, CoreId::Tunnel, tap).await {
                                Ok(c) => Arc::new(c),
                                Err(e) => {
                                    send_event(
                                        &tx,
                                        TunnelEvent::NatPmpFailed {
                                            reason: e.to_string(),
                                        },
                                    );
                                    return;
                                }
                            }
                        }
                    };
                    let req = NatPmpRequest {
                        protocol: Protocol::Tcp,
                        internal_port: 0,
                        external_port_hint: 0,
                        lifetime_seconds: 60,
                    };
                    match client.request(req).await {
                        Ok(lease) => send_event(
                            &tx,
                            TunnelEvent::NatPmpLeaseGranted {
                                external_port: lease.external_port,
                                lifetime_seconds: lease.lifetime_seconds,
                                epoch_seconds: lease.epoch_seconds,
                            },
                        ),
                        Err(e) => send_event(
                            &tx,
                            TunnelEvent::NatPmpFailed {
                                reason: e.to_string(),
                            },
                        ),
                    }
                });
            }
            TunnelAction::RotateEndpoint { peer_index } => {
                spawn_rotate_endpoint(
                    self.runner.clone(),
                    self.config.clone(),
                    peer_index,
                    event_tx.clone(),
                );
            }
            TunnelAction::RunLeakProbe => {
                spawn_run_leak_probe(self.runner.clone(), self.config.clone(), event_tx.clone());
            }
            TunnelAction::ScheduleTimer { timer, after } => {
                // Same pattern as VpnShell: sleep then re-inject a
                // `TimerFired` event causally tagged with the named
                // timer so the observability layer can render the
                // timer as the external cause of the next step.
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let scheduled_at = std::time::Instant::now() + after;
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::external(
                        scheduled_at,
                        ExternalCause::Timer { name: timer.name() },
                        TunnelEvent::TimerFired(timer),
                    ));
                });
            }
        }
    }
}

// ── Action handlers ──────────────────────────────────────────────────────────

fn spawn_configure_interface(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        match configure_interface(&*runner, &config).await {
            Ok(()) => send_event(&tx, TunnelEvent::InterfaceConfigured),
            Err(reason) => send_event(&tx, TunnelEvent::InterfaceConfigureFailed { reason }),
        }
    });
}

async fn configure_interface(
    runner: &dyn Runner,
    config: &TunnelShellConfig,
) -> Result<(), String> {
    let iface = &config.interface_name;
    // `ip link add wg0 type wireguard` — create the device.
    runner
        .run("ip", &["link", "add", "dev", iface, "type", "wireguard"])
        .await
        .map_err(|e| format!("ip link add: {e}"))?;
    // Configure WG: private key + first peer.
    set_wg_interface(runner, config, 0).await?;
    // Add addresses.
    for addr in &config.wg.interface.addresses {
        let cidr = format!("{}/{}", addr.ip, addr.prefix_len);
        runner
            .run("ip", &["addr", "add", &cidr, "dev", iface])
            .await
            .map_err(|e| format!("ip addr add {cidr}: {e}"))?;
    }
    // Bring it up.
    runner
        .run("ip", &["link", "set", "dev", iface, "up"])
        .await
        .map_err(|e| format!("ip link set up: {e}"))?;
    // Default route through the tunnel.  We add 0.0.0.0/0 and ::/0
    // separately to mirror `wg-quick`'s behavior under `Table = off`
    // semantics — Windlass owns the routing decision.
    runner
        .run("ip", &["route", "add", "default", "dev", iface])
        .await
        .map_err(|e| format!("ip route add default v4: {e}"))?;
    Ok(())
}

async fn set_wg_interface(
    runner: &dyn Runner,
    config: &TunnelShellConfig,
    peer_index: usize,
) -> Result<(), String> {
    let iface = &config.interface_name;
    let private_key = config.wg.interface.private_key.expose_secret().to_string();
    let peer = config
        .wg
        .peers
        .get(peer_index)
        .ok_or_else(|| format!("peer index {peer_index} out of range"))?;
    let endpoint = endpoint_for_args(&peer.endpoint)?;
    let allowed = allowed_ips_for_args(peer);
    // Use stdin for the private key so it doesn't appear on the
    // process argv (visible to other processes via `/proc`).  `wg set`
    // accepts `private-key /dev/stdin` to read from stdin.
    let args: Vec<&str> = vec![
        "set",
        iface,
        "private-key",
        "/dev/stdin",
        "peer",
        &peer.public_key,
        "endpoint",
        &endpoint,
        "allowed-ips",
        &allowed,
    ];
    runner
        .run_with_stdin("wg", &args, &private_key)
        .await
        .map_err(|e| format!("wg set: {e}"))?;
    Ok(())
}

fn endpoint_for_args(endpoint: &Endpoint) -> Result<String, String> {
    match endpoint {
        Endpoint::Ip(addr) => Ok(addr.to_string()),
        Endpoint::Hostname { host, port } => {
            // Pre-tunnel DNS hasn't been wired in this phase; refuse
            // a hostname endpoint at the I/O boundary so the failure
            // is loud and named.  Parser-level guarantee
            // (EndpointResolutionPolicy::RequireIpLiteral) is the
            // belt-and-braces path the deployment should rely on.
            Err(format!(
                "hostname endpoint `{host}:{port}` requires pre-tunnel DNS \
                 (not implemented this phase) — supply an IP literal in wg.conf"
            ))
        }
    }
}

fn allowed_ips_for_args(peer: &PeerConfig) -> String {
    peer.allowed_ips
        .iter()
        .map(|a| format!("{}/{}", a.ip, a.prefix_len))
        .collect::<Vec<_>>()
        .join(",")
}

fn spawn_install_firewall(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        let ruleset = build_nft_ruleset(&config);
        match runner.run_with_stdin("nft", &["-f", "-"], &ruleset).await {
            Ok(_) => send_event(&tx, TunnelEvent::FirewallInstalled),
            Err(e) => send_event(
                &tx,
                TunnelEvent::FirewallInstallFailed {
                    reason: e.to_string(),
                },
            ),
        }
    });
}

/// Builds the nftables ruleset that fences egress to the tunnel
/// interface (+ the underlay path to the configured peer) and `lo`.
/// IPv6 is dropped entirely unless the configured peer endpoint is
/// IPv6.
#[must_use]
pub fn build_nft_ruleset(config: &TunnelShellConfig) -> String {
    let iface = &config.interface_name;
    let peer_endpoints: Vec<(IpAddr, u16)> = config
        .wg
        .peers
        .iter()
        .filter_map(|p| match &p.endpoint {
            Endpoint::Ip(addr) => Some((addr.ip(), addr.port())),
            // Hostname endpoints are rejected at configure time; not
            // expected here.  Skip if encountered.
            Endpoint::Hostname { .. } => None,
        })
        .collect();
    let mut rules = String::new();
    rules.push_str("table inet windlass_killswitch\n");
    rules.push_str("delete table inet windlass_killswitch\n");
    rules.push_str("table inet windlass_killswitch {\n");
    rules.push_str("  chain output {\n");
    rules.push_str("    type filter hook output priority filter; policy drop;\n");
    rules.push_str("    oifname \"lo\" accept\n");
    let _ = writeln!(rules, "    oifname \"{iface}\" accept");
    rules.push_str("    ct state established,related accept\n");
    for (ip, port) in &peer_endpoints {
        match ip {
            IpAddr::V4(v4) => {
                let _ = writeln!(rules, "    ip daddr {v4} udp dport {port} accept");
            }
            IpAddr::V6(v6) => {
                let _ = writeln!(rules, "    ip6 daddr {v6} udp dport {port} accept");
            }
        }
    }
    rules.push_str("  }\n");
    rules.push_str("}\n");
    rules
}

fn spawn_poll_handshake(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        let iface = &config.interface_name;
        let peer_pubkey = config
            .wg
            .peers
            .first()
            .map(|p| p.public_key.clone())
            .unwrap_or_default();
        match runner
            .run("wg", &["show", iface, "latest-handshakes"])
            .await
        {
            Err(e) => {
                warn!(error = %e, "wg show failed; treating as stalled");
                send_event(&tx, TunnelEvent::HandshakeStalled);
            }
            Ok(out) => match latest_handshake_age(&out.stdout, &peer_pubkey, chrono::Utc::now()) {
                Ok(HandshakeAge::Observed { age_seconds }) => {
                    send_event(&tx, TunnelEvent::HandshakeReported { age_seconds });
                }
                Ok(HandshakeAge::NeverHandshook) => {
                    send_event(&tx, TunnelEvent::HandshakeStalled);
                }
                Err(e) => {
                    warn!(error = %e, "wg show parse failed; treating as stalled");
                    send_event(&tx, TunnelEvent::HandshakeStalled);
                }
            },
        }
    });
}

fn spawn_rotate_endpoint(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    peer_index: usize,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        match set_wg_interface(&*runner, &config, peer_index).await {
            Ok(()) => {
                // Rotating doesn't have its own confirmation event;
                // the next handshake poll surfaces the result through
                // the existing Reported/Stalled path.
            }
            Err(reason) => {
                warn!(reason, "endpoint rotation failed");
                send_event(
                    &tx,
                    TunnelEvent::InterfaceConfigureFailed {
                        reason: format!("rotate endpoint to peer {peer_index}: {reason}"),
                    },
                );
            }
        }
    });
}

fn spawn_run_leak_probe(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        let outcome = match runner.run("ip", &["-j", "addr", "show"]).await {
            Ok(out) => match parse_ip_addr_show(&out.stdout) {
                Ok(snapshot) => leak_outcome_from_snapshot(&snapshot, &config.interface_name),
                Err(e) => windlass_tunnel_core::LeakProbeOutcome::Inconclusive {
                    reason: format!("parse `ip -j addr show`: {e}"),
                },
            },
            Err(e) => windlass_tunnel_core::LeakProbeOutcome::Inconclusive {
                reason: format!("spawn `ip`: {e}"),
            },
        };
        send_event(&tx, TunnelEvent::LeakProbeCompleted { outcome });
    });
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn send_event(tx: &UnboundedSender<Timed<TunnelEvent>>, event: TunnelEvent) {
    // External cause is shell-originated — these events have no
    // upstream action/publish id.  Once the runtime causal-tx
    // plumbing is wired in Phase 4, this will carry the
    // originating action id.
    let _ = tx.send(Timed::external(
        std::time::Instant::now(),
        ExternalCause::Unknown,
        event,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_tunnel_core::config::EndpointResolutionPolicy;

    const VALID_CONFIG: &str = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";

    fn shell_config() -> TunnelShellConfig {
        let wg = WgConfig::parse(VALID_CONFIG, EndpointResolutionPolicy::RequireIpLiteral)
            .expect("test config parses");
        TunnelShellConfig::new(wg)
    }

    #[test]
    fn nft_ruleset_includes_tunnel_interface_and_peer_underlay() {
        let cfg = shell_config();
        let ruleset = build_nft_ruleset(&cfg);
        assert!(ruleset.contains("oifname \"wg0\" accept"));
        assert!(ruleset.contains("oifname \"lo\" accept"));
        assert!(ruleset.contains("policy drop"));
        // Underlay carve-out for the WireGuard peer endpoint.
        assert!(
            ruleset.contains("ip daddr 198.51.100.7 udp dport 51820 accept"),
            "ruleset missing peer underlay rule: {ruleset}"
        );
    }

    #[test]
    fn nft_ruleset_respects_custom_interface_name() {
        let mut cfg = shell_config();
        cfg.interface_name = "vpn0".to_string();
        let ruleset = build_nft_ruleset(&cfg);
        assert!(ruleset.contains("oifname \"vpn0\" accept"));
        assert!(!ruleset.contains("oifname \"wg0\""));
    }

    #[test]
    fn nft_ruleset_handles_multiple_peers() {
        let multi_peer = "\
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
Endpoint = 198.51.100.8:51821
";
        let wg = WgConfig::parse(multi_peer, EndpointResolutionPolicy::RequireIpLiteral).unwrap();
        let cfg = TunnelShellConfig::new(wg);
        let ruleset = build_nft_ruleset(&cfg);
        assert!(ruleset.contains("198.51.100.7"));
        assert!(ruleset.contains("198.51.100.8"));
    }

    #[test]
    fn endpoint_for_args_rejects_hostname() {
        let result = endpoint_for_args(&Endpoint::Hostname {
            host: "nl-free.protonvpn.net".to_string(),
            port: 51820,
        });
        assert!(result.is_err());
    }

    #[test]
    fn endpoint_for_args_accepts_ipv4_literal() {
        let result = endpoint_for_args(&Endpoint::Ip("198.51.100.7:51820".parse().unwrap()));
        assert_eq!(result.unwrap(), "198.51.100.7:51820");
    }

    #[test]
    fn allowed_ips_for_args_joins_cidrs() {
        let cfg = shell_config();
        let peer = &cfg.wg.peers[0];
        let joined = allowed_ips_for_args(peer);
        assert_eq!(joined, "0.0.0.0/0");
    }
}
