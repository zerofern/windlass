use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use windlass_local::vpn_files;
use windlass_machine::{ExternalCause, Shell, Timed};
use windlass_types::VpnIp;
use windlass_vpn_core::{VerifiedIpInfo, VpnAction, VpnEvent};

/// §31: default ifconfig.co JSON endpoint.  Configurable via
/// `VpnShellConfig::public_ip_verify_url`.
const DEFAULT_PUBLIC_IP_VERIFY_URL: &str = "https://ifconfig.co/json";

/// §33: default MAM `/json/jsonIp.php` endpoint.  Configurable via
/// `VpnShellConfig::mam_ip_verify_url`.  Uses the `t.` subdomain because
/// it's the same host MAM publishes for dynamic-seedbox tooling.
const DEFAULT_MAM_IP_VERIFY_URL: &str = "https://t.myanonamouse.net/json/jsonIp.php";

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

#[derive(Deserialize)]
struct MamJsonIpResponse {
    ip: String,
    #[serde(rename = "ASN", default)]
    asn: Option<u32>,
    #[serde(rename = "AS", default)]
    as_org: Option<String>,
}

pub struct VpnShellConfig {
    pub vpn_ip_file: String,
    pub vpn_port_file: String,
    /// §31: HTTP proxy that routes through Gluetun for the public-IP
    /// verification.  `None` falls back to the host network — usable for
    /// tests but not for production leak-detection.
    pub vpn_proxy_url: Option<String>,
    /// §31: ifconfig.co (or equivalent) JSON endpoint.  `None` uses the
    /// default `https://ifconfig.co/json`.
    pub public_ip_verify_url: Option<String>,
    /// §33: MAM `/json/jsonIp.php` endpoint.  `None` uses the default
    /// `https://t.myanonamouse.net/json/jsonIp.php`.
    pub mam_ip_verify_url: Option<String>,
}

pub struct VpnShell {
    vpn_ip_file: String,
    vpn_port_file: String,
    http: Arc<reqwest::Client>,
    public_ip_verify_url: String,
    mam_ip_verify_url: String,
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
            vpn_ip_file: config.vpn_ip_file,
            vpn_port_file: config.vpn_port_file,
            http: Arc::new(http),
            public_ip_verify_url: config
                .public_ip_verify_url
                .unwrap_or_else(|| DEFAULT_PUBLIC_IP_VERIFY_URL.to_string()),
            mam_ip_verify_url: config
                .mam_ip_verify_url
                .unwrap_or_else(|| DEFAULT_MAM_IP_VERIFY_URL.to_string()),
        }
    }

    fn dispatch(&mut self, action: VpnAction, event_tx: &UnboundedSender<Timed<VpnEvent>>) {
        match action {
            // §38 PR 6: Docker core (via its own bollard watcher +
            // boot-time anchor inspect) is now the source of
            // ContainerHealthy/Unhealthy.  Domain forwards them as
            // VpnCommand, so this action no longer needs to drive a
            // poll.  Kept as a no-op so the VPN machine's existing
            // emit sites (Init / HealthPoll timer / RefreshState) stay
            // stable; a follow-up can drop them from the enum.
            VpnAction::StartMonitoring | VpnAction::InspectContainer => {}
            VpnAction::ReadPortFiles => {
                let ip_file = self.vpn_ip_file.clone();
                let port_file = self.vpn_port_file.clone();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        vpn_files::read_port_files(&ip_file, &port_file)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    match result {
                        Ok((ip, port)) => {
                            let _ = tx.send(Timed::external(
                                std::time::Instant::now(),
                                ExternalCause::Unknown,
                                VpnEvent::PortFileChanged { port },
                            ));
                            // §31: emit the file IP too so the core can dedup
                            // and trigger verification.
                            let _ = tx.send(Timed::external(
                                std::time::Instant::now(),
                                ExternalCause::Unknown,
                                VpnEvent::PublicIpFromFile { ip },
                            ));
                        }
                        Err(reason) => {
                            // §31: a read failure is currently surfaced as
                            // `StateReadFailed`. Distinguishing "file
                            // missing" (PublicIpFileUnavailable) from a
                            // genuine I/O error is a follow-up.
                            let _ = tx.send(Timed::external(
                                std::time::Instant::now(),
                                ExternalCause::Unknown,
                                VpnEvent::StateReadFailed { reason },
                            ));
                        }
                    }
                });
            }
            // §31: ifconfig.co verification through the Gluetun proxy.
            VpnAction::VerifyPublicIp => {
                let http = self.http.clone();
                let url = self.public_ip_verify_url.clone();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let event = verify_public_ip(&http, &url).await;
                    let _ = tx.send(Timed::external(
                        std::time::Instant::now(),
                        ExternalCause::Unknown,
                        event,
                    ));
                });
            }
            // §33: MAM /json/jsonIp.php verification through the same
            // Gluetun-routed client.  Independent failure path so a flaky
            // ifconfig.co doesn't poison the MAM-side counter.
            VpnAction::VerifyMamIp => {
                let http = self.http.clone();
                let url = self.mam_ip_verify_url.clone();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let event = verify_mam_ip(&http, &url).await;
                    let _ = tx.send(Timed::external(
                        std::time::Instant::now(),
                        ExternalCause::Unknown,
                        event,
                    ));
                });
            }
            VpnAction::ScheduleTimer { timer, after } => {
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let scheduled_at = std::time::Instant::now() + after;
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::external(
                        scheduled_at,
                        ExternalCause::Timer { name: timer.name() },
                        VpnEvent::TimerFired(timer),
                    ));
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

/// §33: same shape as `verify_public_ip` but hits MAM's
/// `/json/jsonIp.php`.  Maps to `MamIpVerified`/`MamIpVerifyFailed` so the
/// VPN core can track the per-source failure counter independently.
async fn verify_mam_ip(http: &reqwest::Client, url: &str) -> VpnEvent {
    match http.get(url).send().await {
        Err(e) => VpnEvent::MamIpVerifyFailed {
            reason: format!("request: {e}"),
        },
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                return VpnEvent::MamIpVerifyFailed {
                    reason: format!("HTTP {status}"),
                };
            }
            match resp.json::<MamJsonIpResponse>().await {
                Err(e) => VpnEvent::MamIpVerifyFailed {
                    reason: format!("parse: {e}"),
                },
                Ok(body) => match body.ip.parse::<std::net::Ipv4Addr>() {
                    Err(e) => VpnEvent::MamIpVerifyFailed {
                        reason: format!("ip parse: {e}"),
                    },
                    // MAM only reports IP/ASN/AS, not country or hostname —
                    // pad the other fields with None.
                    Ok(ip) => VpnEvent::MamIpVerified {
                        info: VerifiedIpInfo {
                            ip: VpnIp(ip),
                            asn: body.as_org,
                            country: None,
                            hostname: body.asn.map(|n| format!("AS{n}")),
                        },
                    },
                },
            }
        }
    }
}
