//! Real-WireGuard integration suite.
//!
//! Drives the actual `TunnelMachine` + `TunnelShell` pair — real
//! `wg0`, real nftables kill switch, real NAT-PMP/UDP and HTTP
//! exchanges — against the `wg-server` fixture peer.  This verifies
//! the contracts no unit test can: the kernel handshake actually
//! completes, the dual NAT-PMP mapping round-trips on the wire, the
//! exit-IP query observes the tunnel address, and the kill switch
//! drops direct underlay egress while the tunnel path stays open.
//!
//! Requires NET_ADMIN and the fixture topology; run via:
//!
//! ```bash
//! just integration-wg
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use windlass_machine::{CoreId, ExternalCause, NullRuntimeTap, PublishEnvelope, Timed};
use windlass_net::{TunnelShell, TunnelShellConfig};
use windlass_tunnel_core::config::{EndpointResolutionPolicy, WgConfig};
use windlass_tunnel_core::{
    PeerCount, TunnelConfig, TunnelEvent, TunnelMachine, TunnelPublish, TunnelTopic,
};

/// Generous boot budget: image-cold WireGuard handshakes plus the
/// NAT-PMP retry backoff (first request can race the handshake and
/// retry after 5 s) all fit comfortably.
const BOOT_DEADLINE: Duration = Duration::from_secs(60);

/// One environment-supplied parameter of the fixture topology.
fn fixture_env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} must be set (run via `just integration-wg`)"))
}

#[tokio::test]
#[ignore = "requires NET_ADMIN + the wg-server fixture: just integration-wg"]
async fn tunnel_lifecycle_against_real_wireguard_peer() {
    let conf_path = fixture_env("WG_INT_CONF");
    let underlay_reflector = fixture_env("WG_INT_UNDERLAY_REFLECTOR");
    let expected_port: u16 = fixture_env("WG_INT_EXPECTED_PORT")
        .parse()
        .expect("WG_INT_EXPECTED_PORT must be a port number");

    let wg_content = tokio::fs::read_to_string(&conf_path)
        .await
        .expect("read fixture wg.conf");
    let wg = WgConfig::parse(&wg_content, EndpointResolutionPolicy::RequireIpLiteral)
        .expect("fixture wg.conf parses");
    let peer_count = PeerCount::try_new(wg.peers.len()).expect("fixture has one peer");

    let machine_config = TunnelConfig {
        peer_count,
        // Tight cadences so several watchdog, leak-probe, and
        // exit-IP cycles run within the suite's window.
        handshake_poll_interval: Duration::from_secs(1),
        leak_probe_interval: Duration::from_secs(2),
        exit_ip_query_interval: Duration::from_secs(2),
        ..TunnelConfig::default()
    };
    // `TunnelShellConfig::new` already defaults the NAT-PMP gateway
    // to 10.2.0.1:5351 — the fixture mirrors ProtonVPN's inside
    // addressing, so only the exit-IP URL needs pointing at the
    // fixture's reflector (reachable through the tunnel only).
    let mut shell_config = TunnelShellConfig::new(wg);
    shell_config.exit_ip_urls = vec!["http://10.2.0.1:8080/".to_string()];

    let (handles, _join) = windlass_machine::spawn::<TunnelMachine, TunnelShell>(
        CoreId::Tunnel,
        Arc::new(NullRuntimeTap),
        machine_config,
        shell_config,
    )
    .await;

    let (pub_tx, mut pub_rx) = tokio::sync::mpsc::channel::<PublishEnvelope<TunnelPublish>>(128);
    handles
        .subscribe
        .send((
            vec![
                TunnelTopic::Health,
                TunnelTopic::Port,
                TunnelTopic::Leak,
                TunnelTopic::PublicIp,
            ],
            pub_tx,
        ))
        .expect("subscribe to tunnel publishes");

    handles
        .events
        .send(Timed::external(
            Instant::now(),
            ExternalCause::Init,
            TunnelEvent::Init,
        ))
        .expect("send Init");

    // ── Phase 1: boot to a fully-verified tunnel ─────────────────────────
    // Up (real handshake), PortReady (dual NAT-PMP grant), and
    // ExitIpObserved (HTTP through the tunnel) in any order; any
    // degradation publish during boot fails the suite.
    let mut up = false;
    let mut port: Option<u16> = None;
    let mut exit_ip: Option<String> = None;
    let deadline = tokio::time::Instant::now() + BOOT_DEADLINE;
    while !(up && port.is_some() && exit_ip.is_some()) {
        let envelope = tokio::time::timeout_at(deadline, pub_rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "deadline waiting for tunnel boot; saw up={up} port={port:?} \
                     exit_ip={exit_ip:?}"
                )
            })
            .expect("publish channel stays open");
        match envelope.payload {
            TunnelPublish::Up { .. } | TunnelPublish::Recovered { .. } => up = true,
            TunnelPublish::PortReady { port: p } => port = Some(p.into_inner()),
            TunnelPublish::ExitIpObserved { ip } => exit_ip = Some(ip.0.to_string()),
            bad @ (TunnelPublish::Down { .. }
            | TunnelPublish::Stuck { .. }
            | TunnelPublish::LeakDetected { .. }
            | TunnelPublish::PortUnavailable
            | TunnelPublish::PortForwardingDegraded { .. }
            | TunnelPublish::ExitIpVerificationDegraded { .. }) => {
                panic!("tunnel degraded during boot: {bad:?}");
            }
        }
    }
    assert_eq!(
        port.expect("checked above"),
        expected_port,
        "PortReady must carry the fixture gateway's granted port"
    );
    assert_eq!(
        exit_ip.expect("checked above"),
        "10.2.0.2",
        "the reflector must observe the client's tunnel address"
    );

    // ── Phase 2: kill-switch semantics ───────────────────────────────────
    // The same reflector service answers on both addresses; through
    // the tunnel it must work, on the underlay it must be dropped.
    let through_tunnel = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect("10.2.0.1:8080"),
    )
    .await;
    assert!(
        matches!(through_tunnel, Ok(Ok(_))),
        "tunnel path must stay open: {through_tunnel:?}"
    );
    let direct = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(&underlay_reflector),
    )
    .await;
    assert!(
        !matches!(direct, Ok(Ok(_))),
        "kill switch must drop direct underlay egress to {underlay_reflector}"
    );

    // ── Phase 3: steady state ────────────────────────────────────────────
    // Ride out several watchdog, leak-probe, and renewal cycles.
    // The tunnel must stay healthy: no Down (the watchdog chain must
    // survive healthy handshakes) and no LeakDetected (the fenced
    // control-network interface is expected topology, not a leak).
    let quiet_until = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        match tokio::time::timeout_at(quiet_until, pub_rx.recv()).await {
            Err(_) => break,
            Ok(None) => panic!("publish channel closed during steady state"),
            Ok(Some(envelope)) => match envelope.payload {
                bad @ (TunnelPublish::Down { .. }
                | TunnelPublish::Stuck { .. }
                | TunnelPublish::LeakDetected { .. }
                | TunnelPublish::PortUnavailable
                | TunnelPublish::PortForwardingDegraded { .. }
                | TunnelPublish::ExitIpVerificationDegraded { .. }) => {
                    panic!("tunnel degraded in steady state: {bad:?}");
                }
                TunnelPublish::Up { .. }
                | TunnelPublish::Recovered { .. }
                | TunnelPublish::PortReady { .. }
                | TunnelPublish::ExitIpObserved { .. } => {}
            },
        }
    }
}
