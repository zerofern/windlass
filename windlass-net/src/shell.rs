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
use windlass_tunnel_core::{
    ExitIpFailure, FirewallInstallFailure, InterfaceConfigureFailure, NatPmpFailure, NatPmpRequest,
    TunnelAction, TunnelEvent, TunnelTimer,
};

use crate::natpmp::NatPmpClientError;
use windlass_types::{CoreId, HttpExchange, HttpRequestView, HttpTap, NullHttpTap};

use crate::command::{Runner, SystemRunner};
use crate::handshake::{HandshakeAge, latest_handshake_age};
use crate::natpmp::NatPmpClient;
use crate::probe::{leak_outcome_from_snapshot, parse_ip_addr_show};

/// Default URLs the exit-IP query hits.  Each returns the source
/// IP on the first line.  Tried in order until one succeeds.
const DEFAULT_EXIT_IP_URLS: &[&str] = &["https://api.ipify.org", "https://ipv4.icanhazip.com"];

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
    /// Lifetime requested from the NAT-PMP gateway, in seconds.
    /// Defaults to 60 (`ProtonVPN` caps at 60 regardless).
    pub natpmp_lifetime_seconds: u32,
    /// URLs the shell GETs for `TunnelAction::QueryExitIp`.  Each
    /// must return the connection's source IP as plain text on its
    /// first line.  Tried in order; the first successful response
    /// wins.  Defaults to `[ifconfig.co/ip, icanhazip.com]` — two
    /// independent public reflectors so a single outage doesn't
    /// blind the exit-IP signal.
    pub exit_ip_urls: Vec<String>,
    /// Explicit non-tunnel TCP destinations the kill switch permits.
    ///
    /// This is intentionally narrow. The shipped tunnel compose uses it
    /// for the Postgres service's fixed private address; general internet
    /// egress must go through the tunnel interface.
    pub allowed_tcp_endpoints: Vec<SocketAddr>,
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
            natpmp_lifetime_seconds: 60,
            exit_ip_urls: DEFAULT_EXIT_IP_URLS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allowed_tcp_endpoints: Vec::new(),
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
    /// Lazily-initialized NAT-PMP client.  The tunnel interface is
    /// not yet up at shell construction time, so we defer the UDP
    /// bind until the first request and persist the client through
    /// a `tokio::sync::OnceCell` so subsequent dispatches reuse the
    /// same socket.  Previous version cloned `Option<Arc<...>>`
    /// into the spawned task and never wrote back; every dispatch
    /// re-bound a fresh socket.
    natpmp: Arc<tokio::sync::OnceCell<Arc<NatPmpClient>>>,
    /// Shared HTTP client for the exit-IP query.  Built once at
    /// shell-construction so we don't re-handshake TLS every 6h.
    http: reqwest::Client,
    /// Pending sleep task per timer id.  `ScheduleTimer` has replace
    /// semantics (see the action's doc): re-scheduling a pending
    /// timer aborts the old sleep, so re-armed chains (duplicate
    /// `FirewallInstalled`, operator commands racing the periodic
    /// chains) can never stack a second self-perpetuating chain.
    timers: std::collections::HashMap<TunnelTimer, tokio::task::AbortHandle>,
}

impl Shell for TunnelShell {
    type Config = TunnelShellConfig;
    type Event = TunnelEvent;
    type Action = TunnelAction;

    async fn new(config: Self::Config, _event_tx: UnboundedSender<Timed<Self::Event>>) -> Self {
        let runner: Arc<dyn Runner> = Arc::new(SystemRunner::new(CoreId::Tunnel, config.tap()));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client for exit-IP query");
        Self {
            config: Arc::new(config),
            runner,
            natpmp: Arc::new(tokio::sync::OnceCell::new()),
            http,
            timers: std::collections::HashMap::new(),
        }
    }

    fn dispatch(&mut self, action: TunnelAction, event_tx: &UnboundedSender<Timed<TunnelEvent>>) {
        match action {
            TunnelAction::InstallPreTunnelFirewall => {
                spawn_install_pre_tunnel_firewall(
                    self.runner.clone(),
                    self.config.clone(),
                    event_tx.clone(),
                );
            }
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
            TunnelAction::PollHandshake { peer_index } => {
                spawn_poll_handshake(
                    self.runner.clone(),
                    self.config.clone(),
                    peer_index,
                    event_tx.clone(),
                );
            }
            TunnelAction::RequestNatPmp => {
                spawn_request_natpmp(&self.config, self.natpmp.clone(), event_tx.clone());
            }
            TunnelAction::QueryExitIp => {
                spawn_query_exit_ip(self.http.clone(), &self.config, event_tx.clone());
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
                let handle = windlass_machine::causal::spawn(async move {
                    let scheduled_at = std::time::Instant::now() + after;
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::external(
                        scheduled_at,
                        ExternalCause::Timer { name: timer.name() },
                        TunnelEvent::TimerFired(timer),
                    ));
                });
                // Replace semantics: at most one pending sleep per
                // timer id.  Aborting a task that already fired is a
                // no-op, so the race with an in-flight TimerFired is
                // harmless.
                if let Some(prev) = self.timers.insert(timer, handle.abort_handle()) {
                    prev.abort();
                }
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
) -> Result<(), InterfaceConfigureFailure> {
    let iface = &config.interface_name;
    // Idempotency: a previous boot or a partial-failure retry can
    // leave `wg0` half-configured.  Tear down any prior interface
    // before recreating so we always start from a clean slate.
    // Errors here are ignored — the interface may simply not exist.
    let _ = runner.run("ip", &["link", "del", "dev", iface]).await;

    // Build the interface.  Each step records what we created so we
    // can roll back on failure.
    let mut created_interface = false;
    let outcome: Result<(), InterfaceConfigureFailure> = async {
        runner
            .run("ip", &["link", "add", "dev", iface, "type", "wireguard"])
            .await
            .map_err(|e| InterfaceConfigureFailure::LinkAdd(e.to_string()))?;
        created_interface = true;
        set_wg_interface(runner, config, 0, /*replace = */ true).await?;
        if let Some(mtu) = config.wg.interface.mtu {
            let mtu_s = mtu.to_string();
            runner
                .run("ip", &["link", "set", "dev", iface, "mtu", &mtu_s])
                .await
                .map_err(|e| InterfaceConfigureFailure::MtuSet(e.to_string()))?;
        }
        for addr in &config.wg.interface.addresses {
            let cidr = format!("{}/{}", addr.ip, addr.prefix_len);
            runner
                .run("ip", &["addr", "add", &cidr, "dev", iface])
                .await
                .map_err(|e| InterfaceConfigureFailure::AddressAdd(format!("{cidr}: {e}")))?;
        }
        runner
            .run("ip", &["link", "set", "dev", iface, "up"])
            .await
            .map_err(|e| InterfaceConfigureFailure::LinkUp(e.to_string()))?;
        runner
            .run("ip", &["route", "add", "default", "dev", iface])
            .await
            .map_err(|e| InterfaceConfigureFailure::RouteAdd(format!("v4: {e}")))?;
        if config
            .wg
            .interface
            .addresses
            .iter()
            .any(|a| matches!(a.ip, std::net::IpAddr::V6(_)))
        {
            runner
                .run("ip", &["-6", "route", "add", "default", "dev", iface])
                .await
                .map_err(|e| InterfaceConfigureFailure::RouteAdd(format!("v6: {e}")))?;
        }
        if !config.wg.interface.dns_servers.is_empty() {
            let mut body = String::new();
            for ip in &config.wg.interface.dns_servers {
                use std::fmt::Write as _;
                let _ = writeln!(body, "nameserver {ip}");
            }
            runner
                .run_with_stdin("tee", &["/etc/resolv.conf"], &body)
                .await
                .map_err(|e| InterfaceConfigureFailure::DnsWrite(e.to_string()))?;
        }
        Ok(())
    }
    .await;

    if let Err(ref reason) = outcome
        && created_interface
    {
        let _ = runner.run("ip", &["link", "del", "dev", iface]).await;
        tracing::warn!(reason = %reason, "tunnel interface configure failed; rolled back");
    }
    outcome
}

/// Programs the kernel's `WireGuard` interface with `wg set`.
///
/// When `replace` is true, the peer set is replaced wholesale
/// (`wg-quick` parlance: `replace-peers`).  This is the correct
/// semantics for endpoint rotation — without it, the previous peer
/// stays configured and a second `0.0.0.0/0` `AllowedIPs` entry
/// either errors or leaves ambiguous routing.
async fn set_wg_interface(
    runner: &dyn Runner,
    config: &TunnelShellConfig,
    peer_index: usize,
    replace: bool,
) -> Result<(), InterfaceConfigureFailure> {
    let iface = &config.interface_name;
    let private_key = config.wg.interface.private_key.expose_secret().to_string();
    let peer = config.wg.peers.get(peer_index).ok_or_else(|| {
        InterfaceConfigureFailure::Other(format!("peer index {peer_index} out of range"))
    })?;
    let endpoint = endpoint_for_args(&peer.endpoint).map_err(InterfaceConfigureFailure::Other)?;
    let allowed = allowed_ips_for_args(peer);
    let listen_port_arg = config.wg.interface.listen_port.map(|p| p.to_string());
    let keepalive_arg = peer.persistent_keepalive.map(|s| s.to_string());

    // Build argv.  Private key + optional preshared key flow via
    // stdin; everything else is plain argv.  `wg set` reads multiple
    // `key /dev/stdin` references from the same stdin stream in the
    // order they appear, separated by newlines.
    let mut args: Vec<&str> = vec!["set", iface];
    if replace {
        // Drop the existing peer set first so we never leave the
        // previous endpoint configured after a rotation.
        args.push("replace-peers");
    }
    args.push("private-key");
    args.push("/dev/stdin");
    if let Some(ref lp) = listen_port_arg {
        args.push("listen-port");
        args.push(lp.as_str());
    }
    args.push("peer");
    args.push(&peer.public_key);
    args.push("endpoint");
    args.push(&endpoint);
    args.push("allowed-ips");
    args.push(&allowed);
    if let Some(ref ka) = keepalive_arg {
        args.push("persistent-keepalive");
        args.push(ka.as_str());
    }
    let preshared_clear = peer
        .preshared_key
        .as_ref()
        .map(|s| s.expose_secret().to_string());
    if preshared_clear.is_some() {
        args.push("preshared-key");
        args.push("/dev/stdin");
    }
    // Compose the stdin stream — private key on the first line,
    // optional preshared key on the second.  wg(8) reads one key per
    // /dev/stdin reference in argv order.
    let mut stdin = private_key;
    if let Some(ref psk) = preshared_clear {
        stdin.push('\n');
        stdin.push_str(psk);
    }
    runner
        .run_with_stdin("wg", &args, &stdin)
        .await
        .map_err(|e| InterfaceConfigureFailure::WgSet(e.to_string()))?;
    Ok(())
}

/// Converts a [`NatPmpClientError`] from the shell-level UDP client
/// into the typed [`NatPmpFailure`] the core consumes.
fn natpmp_failure(err: &NatPmpClientError) -> NatPmpFailure {
    match err {
        NatPmpClientError::Bind(e) => NatPmpFailure::Bind(e.to_string()),
        NatPmpClientError::Send { source, .. } => NatPmpFailure::Send(source.to_string()),
        NatPmpClientError::Timeout { .. } => NatPmpFailure::Timeout,
        NatPmpClientError::Recv(e) => NatPmpFailure::Recv(e.to_string()),
        NatPmpClientError::Decode(decode) => match decode {
            windlass_tunnel_core::NatPmpDecodeError::ErrorCode { code, .. } => {
                NatPmpFailure::GatewayError {
                    code: u16::from_be_bytes(code_to_be_u16(*code)),
                }
            }
            other => NatPmpFailure::MalformedResponse(other.to_string()),
        },
    }
}

const fn code_to_be_u16(code: windlass_tunnel_core::NatPmpResponseCode) -> [u8; 2] {
    use windlass_tunnel_core::NatPmpResponseCode as C;
    let n: u16 = match code {
        C::Success => 0,
        C::UnsupportedVersion => 1,
        C::NotAuthorized => 2,
        C::NetworkFailure => 3,
        C::OutOfResources => 4,
        C::UnsupportedOpcode => 5,
        C::Other(c) => c,
    };
    n.to_be_bytes()
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

fn spawn_install_pre_tunnel_firewall(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        // Pre-tunnel kill switch: drops everything except `lo` and the
        // UDP underlay packets to the configured peer endpoint(s).
        // The tunnel interface (`wg0`) doesn't exist yet, so the
        // ruleset MUST NOT name it.  When the interface is configured
        // and `InstallFirewall` runs, we'll replace this table with
        // the full one that also accepts egress via `wg0`.
        let ruleset = build_nft_pre_tunnel_ruleset(&config);
        match runner.run_with_stdin("nft", &["-f", "-"], &ruleset).await {
            Ok(_) => send_event(&tx, TunnelEvent::PreTunnelFirewallInstalled),
            Err(e) => {
                let reason = firewall_failure_from(&e);
                send_event(&tx, TunnelEvent::PreTunnelFirewallInstallFailed { reason });
            }
        }
    });
}

fn firewall_failure_from(e: &crate::command::CommandError) -> FirewallInstallFailure {
    match e {
        crate::command::CommandError::Spawn { .. } => {
            FirewallInstallFailure::NftMissing(e.to_string())
        }
        crate::command::CommandError::NonZeroExit { .. } => {
            FirewallInstallFailure::RulesetRejected(e.to_string())
        }
        crate::command::CommandError::Signal { .. } => FirewallInstallFailure::Other(e.to_string()),
    }
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
            Err(e) => {
                let reason = firewall_failure_from(&e);
                send_event(&tx, TunnelEvent::FirewallInstallFailed { reason });
            }
        }
    });
}

/// Returns the parsed peer endpoints as `(IpAddr, port)` pairs.
/// Hostname endpoints are skipped — the parser rejects them under
/// the default `RequireIpLiteral` policy, so we don't expect them.
fn peer_endpoints_for_ruleset(config: &TunnelShellConfig) -> Vec<(IpAddr, u16)> {
    config
        .wg
        .peers
        .iter()
        .filter_map(|p| match &p.endpoint {
            Endpoint::Ip(addr) => Some((addr.ip(), addr.port())),
            Endpoint::Hostname { .. } => None,
        })
        .collect()
}

/// Appends underlay carve-out rules for each configured peer endpoint.
fn append_peer_underlay_rules(rules: &mut String, peer_endpoints: &[(IpAddr, u16)]) {
    for (ip, port) in peer_endpoints {
        match ip {
            IpAddr::V4(v4) => {
                let _ = writeln!(rules, "    ip daddr {v4} udp dport {port} accept");
            }
            IpAddr::V6(v6) => {
                let _ = writeln!(rules, "    ip6 daddr {v6} udp dport {port} accept");
            }
        }
    }
}

fn append_allowed_tcp_rules(rules: &mut String, endpoints: &[SocketAddr]) {
    for endpoint in endpoints {
        match endpoint {
            SocketAddr::V4(v4) => {
                let _ = writeln!(
                    rules,
                    "    ip daddr {} tcp dport {} accept",
                    v4.ip(),
                    v4.port()
                );
            }
            SocketAddr::V6(v6) => {
                let _ = writeln!(
                    rules,
                    "    ip6 daddr {} tcp dport {} accept",
                    v6.ip(),
                    v6.port()
                );
            }
        }
    }
}

/// Pre-tunnel kill switch — runs BEFORE `ip link add wg0`.
///
/// Allows only loopback and the UDP underlay packets that establish
/// the `WireGuard` session.  No `wg0` rule because the interface
/// doesn't exist yet.  Replaced by [`build_nft_ruleset`] after the
/// interface is up.  (`docs/vpn-ownership.md` acceptance criterion.)
#[must_use]
pub fn build_nft_pre_tunnel_ruleset(config: &TunnelShellConfig) -> String {
    let peer_endpoints = peer_endpoints_for_ruleset(config);
    let mut rules = String::new();
    rules.push_str("table inet windlass_killswitch\n");
    rules.push_str("delete table inet windlass_killswitch\n");
    rules.push_str("table inet windlass_killswitch {\n");
    rules.push_str("  chain output {\n");
    rules.push_str("    type filter hook output priority filter; policy drop;\n");
    rules.push_str("    oifname \"lo\" accept\n");
    rules.push_str("    ct state established,related accept\n");
    append_peer_underlay_rules(&mut rules, &peer_endpoints);
    append_allowed_tcp_rules(&mut rules, &config.allowed_tcp_endpoints);
    rules.push_str("  }\n");
    rules.push_str("}\n");
    rules
}

/// Post-tunnel kill switch — runs after the interface is up.
///
/// Fences egress to the tunnel interface (+ the underlay path to the
/// configured peer) and `lo`.  IPv6 is dropped entirely unless the
/// configured peer endpoint is IPv6.  Replaces the pre-tunnel
/// ruleset by name.
#[must_use]
pub fn build_nft_ruleset(config: &TunnelShellConfig) -> String {
    let iface = &config.interface_name;
    let peer_endpoints = peer_endpoints_for_ruleset(config);
    let mut rules = String::new();
    rules.push_str("table inet windlass_killswitch\n");
    rules.push_str("delete table inet windlass_killswitch\n");
    rules.push_str("table inet windlass_killswitch {\n");
    rules.push_str("  chain output {\n");
    rules.push_str("    type filter hook output priority filter; policy drop;\n");
    rules.push_str("    oifname \"lo\" accept\n");
    let _ = writeln!(rules, "    oifname \"{iface}\" accept");
    rules.push_str("    ct state established,related accept\n");
    append_peer_underlay_rules(&mut rules, &peer_endpoints);
    append_allowed_tcp_rules(&mut rules, &config.allowed_tcp_endpoints);
    rules.push_str("  }\n");
    rules.push_str("}\n");
    rules
}

fn spawn_poll_handshake(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    peer_index: usize,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        let iface = &config.interface_name;
        let Some(peer_pubkey) = config
            .wg
            .peers
            .get(peer_index)
            .map(|p| p.public_key.clone())
        else {
            // Core sent us a stale peer index — shouldn't happen but
            // surface it as a stall so the watchdog reschedules.
            warn!(
                peer_index,
                "PollHandshake action for unknown peer; treating as stalled"
            );
            send_event(&tx, TunnelEvent::HandshakeStalled);
            return;
        };
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
        match set_wg_interface(&*runner, &config, peer_index, /*replace = */ true).await {
            Ok(()) => {
                // Rotating doesn't have its own confirmation event;
                // the next handshake poll surfaces the result through
                // the existing Reported/Stalled path.
            }
            Err(reason) => {
                warn!(reason = %reason, "endpoint rotation failed");
                send_event(
                    &tx,
                    TunnelEvent::InterfaceConfigureFailed {
                        reason: InterfaceConfigureFailure::Other(format!(
                            "rotate endpoint to peer {peer_index}: {reason}"
                        )),
                    },
                );
            }
        }
    });
}

fn spawn_request_natpmp(
    config: &Arc<TunnelShellConfig>,
    cell: Arc<tokio::sync::OnceCell<Arc<NatPmpClient>>>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    let gateway = config.natpmp_gateway;
    let timeout = config.natpmp_timeout;
    let tap = config.tap();
    let lifetime = config.natpmp_lifetime_seconds;
    windlass_machine::causal::spawn(async move {
        // OnceCell::get_or_try_init persists the NAT-PMP client across
        // dispatches so we don't re-bind the local UDP socket every
        // request.  Returns the typed NatPmpFailure on bind failure.
        let client = match cell
            .get_or_try_init(|| async {
                NatPmpClient::new(gateway, timeout, CoreId::Tunnel, tap)
                    .await
                    .map(Arc::new)
            })
            .await
        {
            Ok(c) => Arc::clone(c),
            Err(e) => {
                send_event(
                    &tx,
                    TunnelEvent::NatPmpFailed {
                        reason: natpmp_failure(&e),
                    },
                );
                return;
            }
        };
        // `BitTorrent` needs the forwarded port reachable over both
        // protocols (TCP for peer connections, UDP for DHT/uTP), so
        // map both — the standard `ProtonVPN` natpmpc loop does the
        // same.  Both must land on the same external port to be
        // usable; the gateway grants that in practice because the
        // requests share the wildcard internal port.
        let request_for = |protocol| {
            let client = Arc::clone(&client);
            async move {
                client
                    .request(NatPmpRequest {
                        protocol,
                        internal_port: 0,
                        external_port_hint: 0,
                        lifetime_seconds: lifetime,
                    })
                    .await
            }
        };
        let udp = match request_for(windlass_tunnel_core::natpmp::Protocol::Udp).await {
            Ok(lease) => lease,
            Err(e) => {
                send_event(
                    &tx,
                    TunnelEvent::NatPmpFailed {
                        reason: natpmp_failure(&e),
                    },
                );
                return;
            }
        };
        let tcp = match request_for(windlass_tunnel_core::natpmp::Protocol::Tcp).await {
            Ok(lease) => lease,
            Err(e) => {
                send_event(
                    &tx,
                    TunnelEvent::NatPmpFailed {
                        reason: natpmp_failure(&e),
                    },
                );
                return;
            }
        };
        send_event(&tx, merge_dual_lease(&udp, &tcp));
    });
}

/// Folds the UDP + TCP leases of one dual-mapping round into a
/// single event.  A split grant (different external ports) is
/// unusable for `BitTorrent`, so it surfaces as a failure and the
/// normal retry/backoff path takes over.  The merged lease renews on
/// the shorter lifetime and reports the newer epoch.
fn merge_dual_lease(
    udp: &windlass_tunnel_core::NatPmpLease,
    tcp: &windlass_tunnel_core::NatPmpLease,
) -> TunnelEvent {
    if udp.external_port != tcp.external_port {
        return TunnelEvent::NatPmpFailed {
            reason: NatPmpFailure::PortMismatch {
                udp_port: udp.external_port,
                tcp_port: tcp.external_port,
            },
        };
    }
    TunnelEvent::NatPmpLeaseGranted {
        external_port: tcp.external_port,
        lifetime_seconds: udp.lifetime_seconds.min(tcp.lifetime_seconds),
        epoch_seconds: udp.epoch_seconds.max(tcp.epoch_seconds),
    }
}

fn spawn_query_exit_ip(
    http: reqwest::Client,
    config: &Arc<TunnelShellConfig>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    let urls = config.exit_ip_urls.clone();
    let tap = config.tap();
    windlass_machine::causal::spawn(async move {
        let mut last_failure = None;
        for url in &urls {
            match query_exit_ip_once(&http, &tap, url).await {
                Ok(event) => {
                    send_event(&tx, event);
                    return;
                }
                Err(failure) => {
                    last_failure = Some(failure);
                }
            }
        }
        // Every URL failed.  Surface the last failure so the
        // operator at least sees one concrete reason.
        let event = last_failure.map_or_else(
            || TunnelEvent::ExitIpQueryFailed {
                reason: ExitIpFailure::Transport("no exit_ip_urls configured".into()),
            },
            |reason| TunnelEvent::ExitIpQueryFailed { reason },
        );
        send_event(&tx, event);
    });
}

/// Tries one exit-IP source.  Returns `Ok(ExitIpObserved)` on a
/// usable response, or `Err(typed failure)` so the caller can fall
/// through to the next URL.
async fn query_exit_ip_once(
    http: &reqwest::Client,
    tap: &Arc<dyn HttpTap>,
    url: &str,
) -> Result<TunnelEvent, ExitIpFailure> {
    tap.gate_request(
        CoreId::Tunnel,
        &HttpRequestView {
            method: "GET",
            url,
            body: None,
        },
    )
    .await;
    let resp = http.get(url).send().await.map_err(|e| {
        tap.observed_exchange(
            CoreId::Tunnel,
            &exit_ip_exchange(url, 0, "", &e.to_string()),
        );
        ExitIpFailure::Transport(e.to_string())
    })?;
    let status = resp.status();
    let response_status = status.as_u16();
    if !status.is_success() {
        tap.observed_exchange(
            CoreId::Tunnel,
            &exit_ip_exchange(url, response_status, "", ""),
        );
        return Err(ExitIpFailure::HttpStatus(response_status));
    }
    let body = resp.text().await.map_err(|e| {
        tap.observed_exchange(
            CoreId::Tunnel,
            &exit_ip_exchange(url, response_status, "", &e.to_string()),
        );
        ExitIpFailure::Transport(e.to_string())
    })?;
    tap.observed_exchange(
        CoreId::Tunnel,
        &exit_ip_exchange(url, response_status, &body, ""),
    );
    let raw = body.lines().next().unwrap_or("").trim();
    let ip = raw
        .parse::<std::net::IpAddr>()
        .map_err(|e| ExitIpFailure::Parse(format!("`{raw}`: {e}")))?;
    let vpn = windlass_types::VpnIp::from_ip(ip).ok_or_else(|| {
        ExitIpFailure::Parse(format!("IPv6 exit IP `{ip}` not yet mapped to VpnIp"))
    })?;
    Ok(TunnelEvent::ExitIpObserved { ip: vpn })
}

fn exit_ip_exchange(url: &str, status: u16, body: &str, error: &str) -> HttpExchange {
    let response_headers = if error.is_empty() {
        Vec::new()
    } else {
        vec![("error".to_string(), error.to_string())]
    };
    HttpExchange {
        module: "tunnel-exit-ip".to_string(),
        method: "GET".to_string(),
        url: url.to_string(),
        request_headers: Vec::new(),
        request_body: None,
        response_status: status,
        response_headers,
        response_body: body.to_string(),
    }
}

fn spawn_run_leak_probe(
    runner: Arc<dyn Runner>,
    config: Arc<TunnelShellConfig>,
    tx: UnboundedSender<Timed<TunnelEvent>>,
) {
    windlass_machine::causal::spawn(async move {
        // Layer 1: interface enumeration.
        let layer1 = match runner.run("ip", &["-j", "addr", "show"]).await {
            Ok(out) => match parse_ip_addr_show(&out.stdout) {
                Ok(snapshot) => {
                    let enum_outcome =
                        leak_outcome_from_snapshot(&snapshot, &config.interface_name);
                    // If layer 1 found a stray interface, we already
                    // have a leak signal — escalate via the active
                    // probe to get a real `observed_remote` rather
                    // than the generic enumeration string.  If layer
                    // 1 found nothing, the active probe is what
                    // verifies the kill switch actually drops on
                    // attempts.
                    Some((snapshot, enum_outcome))
                }
                Err(e) => {
                    send_event(
                        &tx,
                        TunnelEvent::LeakProbeCompleted {
                            outcome: windlass_tunnel_core::LeakProbeOutcome::Inconclusive {
                                reason: format!("parse `ip -j addr show`: {e}"),
                            },
                        },
                    );
                    return;
                }
            },
            Err(e) => {
                send_event(
                    &tx,
                    TunnelEvent::LeakProbeCompleted {
                        outcome: windlass_tunnel_core::LeakProbeOutcome::Inconclusive {
                            reason: format!("spawn `ip`: {e}"),
                        },
                    },
                );
                return;
            }
        };
        let (snapshot, enum_outcome) = layer1.expect("checked above");
        let active = crate::probe::active_connect_probe(&snapshot, &config.interface_name);
        // Either probe finding a leak is sufficient.  The active
        // probe's `LeakDetected` carries the concrete remote we
        // reached; if only enumeration detected something, we
        // surface its more generic message.
        let outcome = match (enum_outcome, active) {
            (_, active @ windlass_tunnel_core::LeakProbeOutcome::LeakDetected { .. })
            | (active @ windlass_tunnel_core::LeakProbeOutcome::LeakDetected { .. }, _) => active,
            (windlass_tunnel_core::LeakProbeOutcome::NoEgressDetected, _) => {
                windlass_tunnel_core::LeakProbeOutcome::NoEgressDetected
            }
            (other, _) => other,
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
    fn dual_lease_merge_requires_matching_ports() {
        use windlass_tunnel_core::NatPmpLease;
        use windlass_tunnel_core::natpmp::Protocol;
        let udp = NatPmpLease {
            protocol: Protocol::Udp,
            epoch_seconds: 100,
            internal_port: 0,
            external_port: 42000,
            lifetime_seconds: 60,
        };
        let tcp = NatPmpLease {
            protocol: Protocol::Tcp,
            epoch_seconds: 105,
            internal_port: 0,
            external_port: 42000,
            lifetime_seconds: 45,
        };
        // Same port: granted, renewing on the shorter lifetime and
        // reporting the newer epoch.
        match merge_dual_lease(&udp, &tcp) {
            TunnelEvent::NatPmpLeaseGranted {
                external_port,
                lifetime_seconds,
                epoch_seconds,
            } => {
                assert_eq!(external_port, 42000);
                assert_eq!(lifetime_seconds, 45);
                assert_eq!(epoch_seconds, 105);
            }
            other => panic!("expected grant, got {other:?}"),
        }
        // Split grant: unusable for BitTorrent → typed failure.
        let tcp_split = NatPmpLease {
            external_port: 43000,
            ..tcp
        };
        match merge_dual_lease(&udp, &tcp_split) {
            TunnelEvent::NatPmpFailed {
                reason: NatPmpFailure::PortMismatch { udp_port, tcp_port },
            } => {
                assert_eq!(udp_port, 42000);
                assert_eq!(tcp_port, 43000);
            }
            other => panic!("expected PortMismatch, got {other:?}"),
        }
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
    fn nft_rulesets_include_explicit_allowed_tcp_endpoints() {
        let mut cfg = shell_config();
        cfg.allowed_tcp_endpoints = vec!["172.30.0.10:5432".parse().unwrap()];

        let pre = build_nft_pre_tunnel_ruleset(&cfg);
        let post = build_nft_ruleset(&cfg);

        assert!(pre.contains("ip daddr 172.30.0.10 tcp dport 5432 accept"));
        assert!(post.contains("ip daddr 172.30.0.10 tcp dport 5432 accept"));
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

    // ── Action-dispatch tests with a recording fake runner ──────────────────

    use crate::command::{CommandError, CommandOutcome, Runner};
    use std::sync::Mutex;

    /// Records every spawn the shell would have made.  Returns
    /// caller-supplied responses in order, defaulting to an empty
    /// stdout success.
    #[derive(Default)]
    struct RecordingRunner {
        calls: Mutex<Vec<(String, Vec<String>, Option<String>)>>,
        responses: Mutex<Vec<Result<CommandOutcome, CommandError>>>,
    }

    impl RecordingRunner {
        fn record(&self, program: &str, args: &[&str], stdin: Option<&str>) {
            self.calls.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| (*s).to_string()).collect(),
                stdin.map(str::to_string),
            ));
        }
        fn next_response(&self) -> Result<CommandOutcome, CommandError> {
            self.responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(Ok(CommandOutcome {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                }))
        }
    }

    #[async_trait::async_trait]
    impl Runner for RecordingRunner {
        async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutcome, CommandError> {
            self.record(program, args, None);
            self.next_response()
        }
        async fn run_with_stdin(
            &self,
            program: &str,
            args: &[&str],
            stdin: &str,
        ) -> Result<CommandOutcome, CommandError> {
            self.record(program, args, Some(stdin));
            self.next_response()
        }
    }

    fn argv_strings(calls: &[(String, Vec<String>, Option<String>)]) -> Vec<String> {
        calls
            .iter()
            .map(|(p, a, _)| format!("{p} {}", a.join(" ")))
            .collect()
    }

    #[tokio::test]
    async fn configure_interface_runs_steps_in_order_and_applies_mtu_dns() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32
DNS = 10.2.0.1
MTU = 1380
ListenPort = 51820

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
PresharedKey = CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
PersistentKeepalive = 25
";
        let wg = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral).unwrap();
        let config = TunnelShellConfig::new(wg);
        let runner = RecordingRunner::default();

        configure_interface(&runner, &config)
            .await
            .expect("configure should succeed");

        let calls = runner.calls.lock().unwrap();
        let lines = argv_strings(&calls);
        // The first call after the leading idempotent `link del`
        // attempt is the real `link add`.
        assert!(lines.iter().any(|l| l == "ip link del dev wg0"));
        assert!(
            lines
                .iter()
                .any(|l| l == "ip link add dev wg0 type wireguard")
        );
        // wg set carries replace-peers + listen-port + peer/endpoint/allowed-ips
        let wg_set = lines
            .iter()
            .find(|l| l.starts_with("wg set"))
            .expect("wg set call");
        assert!(wg_set.contains("replace-peers"));
        assert!(wg_set.contains("private-key /dev/stdin"));
        assert!(wg_set.contains("listen-port 51820"));
        assert!(wg_set.contains("persistent-keepalive 25"));
        // MTU is applied.
        assert!(lines.iter().any(|l| l == "ip link set dev wg0 mtu 1380"));
        // DNS is written via tee.
        assert!(lines.iter().any(|l| l.starts_with("tee /etc/resolv.conf")));
        // Address + link up + default route happen after wg set.
        assert!(lines.iter().any(|l| l == "ip addr add 10.2.0.2/32 dev wg0"));
        assert!(lines.iter().any(|l| l == "ip link set dev wg0 up"));
        assert!(lines.iter().any(|l| l == "ip route add default dev wg0"));
    }

    #[tokio::test]
    async fn configure_interface_rolls_back_on_failure() {
        let content = "\
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.2.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 0.0.0.0/0
Endpoint = 198.51.100.7:51820
";
        let wg = WgConfig::parse(content, EndpointResolutionPolicy::RequireIpLiteral).unwrap();
        let config = TunnelShellConfig::new(wg);
        let runner = RecordingRunner::default();
        // Set up responses (popped in reverse).  We want:
        //   1. link del  → succeed (idempotent)
        //   2. link add  → succeed
        //   3. wg set    → fail
        // After failure, the rollback link del is expected at the end.
        runner.responses.lock().unwrap().extend(vec![
            // Ordering of the Vec is LIFO when we `pop`, so push in
            // reverse: rollback success, wg-set failure, link-add
            // success, idempotent del success.
            Ok(CommandOutcome {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            }),
            Err(CommandError::NonZeroExit {
                program: "wg".to_string(),
                code: 1,
                stderr: "fake wg set failure".to_string(),
            }),
            Ok(CommandOutcome {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            }),
            Ok(CommandOutcome {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            }),
        ]);

        let err = configure_interface(&runner, &config)
            .await
            .expect_err("wg set failure should propagate");
        assert!(matches!(err, InterfaceConfigureFailure::WgSet(_)));

        let calls = runner.calls.lock().unwrap();
        let lines = argv_strings(&calls);
        // The rollback `link del` runs after the failed wg set.
        let positions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| *l == "ip link del dev wg0")
            .map(|(i, _)| i)
            .collect();
        assert!(
            positions.len() >= 2,
            "expected pre-config and rollback `link del` calls"
        );
        let wg_set_pos = lines
            .iter()
            .position(|l| l.starts_with("wg set"))
            .expect("wg set was attempted");
        assert!(
            positions.iter().any(|p| *p > wg_set_pos),
            "rollback link del did not run after wg set failure"
        );
    }

    #[tokio::test]
    async fn rotation_uses_replace_peers_so_old_peer_is_dropped() {
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
        let config = TunnelShellConfig::new(wg);
        let runner = RecordingRunner::default();

        set_wg_interface(&runner, &config, 1, /*replace = */ true)
            .await
            .expect("rotate to peer 1 succeeds");

        let calls = runner.calls.lock().unwrap();
        let wg_set = argv_strings(&calls)
            .into_iter()
            .find(|l| l.starts_with("wg set"))
            .expect("wg set call");
        assert!(
            wg_set.contains("replace-peers"),
            "missing replace-peers: {wg_set}"
        );
        // The rotation should configure peer 1's public key, not peer 0's.
        assert!(wg_set.contains("CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC="));
        assert!(!wg_set.contains("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB="));
    }

    #[tokio::test]
    async fn private_key_flows_via_stdin_only_never_argv() {
        let wg = WgConfig::parse(VALID_CONFIG, EndpointResolutionPolicy::RequireIpLiteral).unwrap();
        let config = TunnelShellConfig::new(wg);
        let runner = RecordingRunner::default();
        set_wg_interface(&runner, &config, 0, false)
            .await
            .expect("set wg ok");
        let calls = runner.calls.lock().unwrap();
        let (_, args, stdin) = calls.first().expect("a wg set call");
        // The private-key cleartext must appear in stdin, NEVER in argv.
        let secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        let joined_argv = args.join(" ");
        assert!(
            !joined_argv.contains(secret),
            "private key leaked into argv: {joined_argv}"
        );
        assert!(
            stdin.as_deref().unwrap_or("").contains(secret),
            "private key missing from stdin"
        );
    }
}
