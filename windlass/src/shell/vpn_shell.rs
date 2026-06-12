//! [`windlass_machine::Shell`] for [`windlass_vpn_core::VpnMachine`].
//!
//! In tunnel mode, [`windlass_tunnel_core`] + [`windlass_net`] own the
//! privileged `WireGuard` I/O and this shell is intentionally inert.
//! In legacy Gluetun-compatible mode, this shell only translates Docker
//! health and Gluetun file observations into `VpnEvent`s; `VpnMachine`
//! still owns every state transition and policy decision.

use std::net::IpAddr;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use windlass_local::docker::DockerClient;
use windlass_machine::{KeyedTimers, Shell, Timed};
use windlass_types::{VpnIp, VpnPort};
use windlass_vpn_core::{VpnAction, VpnEvent, VpnTimer};

pub enum VpnShellConfig {
    TunnelMode,
    LegacyGluetun {
        docker: DockerClient,
        vpn_ip_file: String,
        vpn_port_file: String,
    },
}

enum VpnShellMode {
    TunnelMode,
    LegacyGluetun {
        docker: DockerClient,
        vpn_ip_file: String,
        vpn_port_file: String,
    },
}

pub struct VpnShell {
    mode: VpnShellMode,
    /// Replace-semantics timers: at most one pending sleep per
    /// [`VpnTimer`] id (see [`KeyedTimers`]).
    timers: KeyedTimers<VpnTimer>,
}

impl Shell for VpnShell {
    type Config = VpnShellConfig;
    type Event = VpnEvent;
    type Action = VpnAction;

    async fn new(config: Self::Config, _event_tx: UnboundedSender<Timed<Self::Event>>) -> Self {
        let mode = match config {
            VpnShellConfig::TunnelMode => VpnShellMode::TunnelMode,
            VpnShellConfig::LegacyGluetun {
                docker,
                vpn_ip_file,
                vpn_port_file,
            } => VpnShellMode::LegacyGluetun {
                docker,
                vpn_ip_file,
                vpn_port_file,
            },
        };
        Self {
            mode,
            timers: KeyedTimers::new(),
        }
    }

    fn dispatch(&mut self, action: VpnAction, event_tx: &UnboundedSender<Timed<VpnEvent>>) {
        if matches!(self.mode, VpnShellMode::TunnelMode) {
            return;
        }
        if let VpnAction::ScheduleTimer { timer, after } = action {
            self.timers.schedule(
                timer,
                timer.name(),
                after,
                event_tx,
                VpnEvent::TimerFired(timer),
            );
            return;
        }
        match &self.mode {
            VpnShellMode::TunnelMode => {}
            VpnShellMode::LegacyGluetun {
                docker,
                vpn_ip_file,
                vpn_port_file,
            } => dispatch_legacy(
                &action,
                docker.clone(),
                vpn_ip_file.clone(),
                vpn_port_file.clone(),
                event_tx.clone(),
            ),
        }
    }
}

fn dispatch_legacy(
    action: &VpnAction,
    docker: DockerClient,
    vpn_ip_file: String,
    vpn_port_file: String,
    tx: UnboundedSender<Timed<VpnEvent>>,
) {
    match action {
        VpnAction::InspectContainer => {
            windlass_machine::causal::spawn(async move {
                let event = if docker.is_gluetun_healthy().await {
                    VpnEvent::ContainerHealthy
                } else {
                    VpnEvent::ContainerUnhealthy
                };
                send_event(&tx, event);
            });
        }
        VpnAction::ReadPortFiles => {
            windlass_machine::causal::spawn(async move {
                read_legacy_files(&vpn_ip_file, &vpn_port_file, &tx).await;
            });
        }
        VpnAction::StartMonitoring => {
            windlass_machine::causal::spawn(async move {
                poll_legacy_files(vpn_ip_file, vpn_port_file, tx).await;
            });
        }
        VpnAction::VerifyPublicIp => {
            send_event(
                &tx,
                VpnEvent::PublicIpVerifyFailed {
                    reason: "legacy verification disabled in shell".to_string(),
                },
            );
        }
        VpnAction::VerifyMamIp => {
            send_event(
                &tx,
                VpnEvent::MamIpVerifyFailed {
                    reason: "legacy verification disabled in shell".to_string(),
                },
            );
        }
        // Handled in `dispatch` (needs the shell's KeyedTimers).
        VpnAction::ScheduleTimer { .. } => {}
    }
}

async fn read_legacy_files(
    vpn_ip_file: &str,
    vpn_port_file: &str,
    tx: &UnboundedSender<Timed<VpnEvent>>,
) {
    match read_legacy_ip(vpn_ip_file).await {
        Some(ip) => send_event(tx, VpnEvent::PublicIpFromFile { ip }),
        None => send_event(tx, VpnEvent::PublicIpFileUnavailable),
    }

    match read_legacy_port(vpn_port_file).await {
        Ok(port) => match port {
            Some(port) => send_event(tx, VpnEvent::PortFileChanged { port }),
            None => send_event(
                tx,
                VpnEvent::StateReadFailed {
                    reason: format!("invalid VPN port file `{vpn_port_file}`"),
                },
            ),
        },
        Err(reason) => send_event(tx, VpnEvent::StateReadFailed { reason }),
    }
}

async fn poll_legacy_files(
    vpn_ip_file: String,
    vpn_port_file: String,
    tx: UnboundedSender<Timed<VpnEvent>>,
) {
    let mut last_ip: Option<Option<VpnIp>> = None;
    let mut last_port: Option<Option<VpnPort>> = None;
    // Rising-edge gate for port read errors: surface the first
    // failure of a streak as `StateReadFailed` (so the machine
    // reacts), then stay quiet until the file reads again — the
    // 1 s cadence would otherwise storm the retry timer and logs.
    let mut port_read_errored = false;

    loop {
        let ip = read_legacy_ip(&vpn_ip_file).await;
        if last_ip != Some(ip) {
            match ip {
                Some(ip) => send_event(&tx, VpnEvent::PublicIpFromFile { ip }),
                None => send_event(&tx, VpnEvent::PublicIpFileUnavailable),
            }
            last_ip = Some(ip);
        }

        match read_legacy_port(&vpn_port_file).await {
            Ok(port) => {
                port_read_errored = false;
                if last_port != Some(port) {
                    match port {
                        Some(port) => send_event(&tx, VpnEvent::PortFileChanged { port }),
                        // Readable but unparseable content is an
                        // error too — mirroring `read_legacy_files`
                        // — not a silent no-op that leaves the
                        // machine trusting a stale port.
                        None => send_event(
                            &tx,
                            VpnEvent::StateReadFailed {
                                reason: format!("invalid VPN port file `{vpn_port_file}`"),
                            },
                        ),
                    }
                    last_port = Some(port);
                }
            }
            Err(reason) => {
                if !port_read_errored {
                    send_event(&tx, VpnEvent::StateReadFailed { reason });
                    port_read_errored = true;
                }
                // Forget the last good value so the file coming back
                // with the same port still re-fires `PortFileChanged`.
                last_port = None;
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn read_legacy_ip(vpn_ip_file: &str) -> Option<VpnIp> {
    tokio::fs::read_to_string(vpn_ip_file)
        .await
        .ok()
        .and_then(|raw| raw.trim().parse::<IpAddr>().ok())
        .and_then(VpnIp::from_ip)
}

async fn read_legacy_port(vpn_port_file: &str) -> Result<Option<VpnPort>, String> {
    let raw = tokio::fs::read_to_string(vpn_port_file)
        .await
        .map_err(|e| format!("read VPN port file `{vpn_port_file}`: {e}"))?;
    Ok(raw
        .trim()
        .parse::<u16>()
        .ok()
        .and_then(|port| VpnPort::try_new(port).ok()))
}

fn send_event(tx: &UnboundedSender<Timed<VpnEvent>>, event: VpnEvent) {
    let _ = tx.send(Timed::from_dispatch(std::time::Instant::now(), event));
}
