use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use windlass_local::{docker::DockerClient, vpn_files};
use windlass_machine::{Shell, Timed};
use windlass_types::VpnIp;
use windlass_vpn_core::{VerifiedIpInfo, VpnAction, VpnEvent};

/// §31: default ifconfig.co JSON endpoint.  Configurable via
/// `VpnShellConfig::public_ip_verify_url`.
const DEFAULT_PUBLIC_IP_VERIFY_URL: &str = "https://ifconfig.co/json";

#[derive(Deserialize)]
struct IfConfigResponse {
    ip: String,
    #[serde(default)]
    asn_org: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
}

pub struct VpnShellConfig {
    pub docker: DockerClient,
    pub vpn_ip_file: String,
    pub vpn_port_file: String,
    /// §31: HTTP proxy that routes through Gluetun for the public-IP
    /// verification.  `None` falls back to the host network — usable for
    /// tests but not for production leak-detection.
    pub vpn_proxy_url: Option<String>,
    /// §31: ifconfig.co (or equivalent) JSON endpoint.  `None` uses the
    /// default `https://ifconfig.co/json`.
    pub public_ip_verify_url: Option<String>,
}

pub struct VpnShell {
    docker: DockerClient,
    vpn_ip_file: String,
    vpn_port_file: String,
    http: Arc<reqwest::Client>,
    public_ip_verify_url: String,
}

impl Shell for VpnShell {
    type Config = VpnShellConfig;
    type Event = VpnEvent;
    type Action = VpnAction;

    async fn new(config: Self::Config, _event_tx: UnboundedSender<Timed<VpnEvent>>) -> Self {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(15));
        if let Some(url) = config.vpn_proxy_url.as_deref()
            && let Ok(proxy) = reqwest::Proxy::all(url)
        {
            builder = builder.proxy(proxy);
        }
        let http = builder.build().expect("reqwest client for ifconfig.co");
        Self {
            docker: config.docker,
            vpn_ip_file: config.vpn_ip_file,
            vpn_port_file: config.vpn_port_file,
            http: Arc::new(http),
            public_ip_verify_url: config
                .public_ip_verify_url
                .unwrap_or_else(|| DEFAULT_PUBLIC_IP_VERIFY_URL.to_string()),
        }
    }

    fn dispatch(&mut self, action: VpnAction, event_tx: &UnboundedSender<Timed<VpnEvent>>) {
        match action {
            VpnAction::StartMonitoring => {}
            VpnAction::InspectContainer => {
                let docker = self.docker.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = if docker.is_gluetun_healthy().await {
                        VpnEvent::ContainerHealthy
                    } else {
                        VpnEvent::ContainerUnhealthy
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            VpnAction::ReadPortFiles => {
                let ip_file = self.vpn_ip_file.clone();
                let port_file = self.vpn_port_file.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        vpn_files::read_port_files(&ip_file, &port_file)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    match result {
                        Ok((ip, port)) => {
                            let _ = tx.send(Timed::now(VpnEvent::PortFileChanged { port }));
                            // §31: emit the file IP too so the core can dedup
                            // and trigger verification.
                            let _ = tx.send(Timed::now(VpnEvent::PublicIpFromFile { ip }));
                        }
                        Err(reason) => {
                            // §31: a read failure is currently surfaced as
                            // `StateReadFailed`. Distinguishing "file
                            // missing" (PublicIpFileUnavailable) from a
                            // genuine I/O error is a follow-up.
                            let _ = tx.send(Timed::now(VpnEvent::StateReadFailed { reason }));
                        }
                    }
                });
            }
            // §31: ifconfig.co verification through the Gluetun proxy.
            VpnAction::VerifyPublicIp => {
                let http = self.http.clone();
                let url = self.public_ip_verify_url.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = verify_public_ip(&http, &url).await;
                    let _ = tx.send(Timed::now(event));
                });
            }
            VpnAction::ScheduleTimer { timer, after } => {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let scheduled_at = std::time::Instant::now() + after;
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::new(scheduled_at, VpnEvent::TimerFired(timer)));
                });
            }
        }
    }
}

async fn verify_public_ip(http: &reqwest::Client, url: &str) -> VpnEvent {
    match http.get(url).send().await {
        Err(e) => VpnEvent::PublicIpVerifyFailed {
            reason: format!("request: {e}"),
        },
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                return VpnEvent::PublicIpVerifyFailed {
                    reason: format!("HTTP {status}"),
                };
            }
            match resp.json::<IfConfigResponse>().await {
                Err(e) => VpnEvent::PublicIpVerifyFailed {
                    reason: format!("parse: {e}"),
                },
                Ok(body) => match body.ip.parse::<std::net::Ipv4Addr>() {
                    Err(e) => VpnEvent::PublicIpVerifyFailed {
                        reason: format!("ip parse: {e}"),
                    },
                    Ok(ip) => VpnEvent::PublicIpVerified {
                        info: VerifiedIpInfo {
                            ip: VpnIp(ip),
                            asn: body.asn_org,
                            country: body.country,
                            hostname: body.hostname,
                        },
                    },
                },
            }
        }
    }
}
