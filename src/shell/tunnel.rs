//! Ask a neutral service for the tunnel's exit IP, and start the NixOS unit
//! that connects to another Proton VPN server.

use std::{net::IpAddr, path::PathBuf, time::Duration};

use reqwest::{Client as HttpClient, Url};

use crate::core::{ExitIpFailure, TunnelEvent, VpnServer};

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// How the shell reaches the exit-IP service and starts server units.
#[derive(Debug, Clone)]
pub(super) struct Client {
    http: HttpClient,
    service: Url,
    unit_starter: PathBuf,
}

impl Client {
    pub(super) fn new(service: Url, unit_starter: PathBuf) -> Self {
        // Proxies are ignored so that the check sees the tunnel itself. Building
        // fails only if the TLS backend cannot initialise; the default client
        // then reports the failure on use.
        let http = HttpClient::builder()
            .timeout(CHECK_TIMEOUT)
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self {
            http,
            service,
            unit_starter,
        }
    }
}

/// The tunnel work the core can ask for.
#[derive(Debug, Clone)]
pub(super) enum Request {
    CheckExitIp,
    Switch(VpnServer),
}

/// Perform a request; without configured tunnel checks it fails.
pub(super) async fn perform(client: Option<&Client>, request: Request) -> TunnelEvent {
    match request {
        Request::CheckExitIp => check(client).await,
        Request::Switch(server) => switch(client, &server).await,
    }
}

/// Ask for the exit IP; without a configured service the check fails.
async fn check(client: Option<&Client>) -> TunnelEvent {
    let outcome = match client {
        Some(client) => exit_ip(client).await,
        None => Err(ExitIpFailure::Unreachable),
    };
    match outcome {
        Ok(ip) => TunnelEvent::ExitIpObserved { ip },
        Err(reason) => TunnelEvent::ExitIpCheckFailed { reason },
    }
}

async fn exit_ip(client: &Client) -> Result<IpAddr, ExitIpFailure> {
    let response = client
        .http
        .get(client.service.clone())
        .send()
        .await
        .map_err(|_| ExitIpFailure::Unreachable)?;
    let status = response.status();
    if !status.is_success() {
        return Err(ExitIpFailure::Rejected {
            status: status.as_u16(),
        });
    }
    // A body that cannot be read is no address either.
    let body = response.text().await.unwrap_or_default();
    body.trim().parse().map_err(|_| ExitIpFailure::Malformed)
}

/// Start `windlass-wireguard@<server>.service` and wait for it.
async fn switch(client: Option<&Client>, server: &VpnServer) -> TunnelEvent {
    let Some(client) = client else {
        return TunnelEvent::SwitchFailed;
    };
    let status = tokio::process::Command::new(&client.unit_starter)
        .arg("start")
        .arg(format!("windlass-wireguard@{server}.service"))
        .status()
        .await;
    if status.is_ok_and(|status| status.success()) {
        TunnelEvent::Switched
    } else {
        TunnelEvent::SwitchFailed
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, net::IpAddr};

    use axum::{Router, http::StatusCode, routing::get};
    use reqwest::Url;

    use crate::core::{ExitIpFailure, TunnelEvent, VpnServer};

    use super::{Client, Request, perform};

    #[tokio::test]
    async fn checks_and_switches_report_their_outcome() -> Result<(), Box<dyn Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let service = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let routes = Router::new()
            .route("/ip", get(|| async { "192.0.2.7\n" }))
            .route("/garbage", get(|| async { "not an address" }))
            .route("/down", get(|| async { StatusCode::SERVICE_UNAVAILABLE }));
        tokio::spawn(async move { axum::serve(listener, routes).await });
        let server = VpnServer::try_new("two")?;
        let failed = |reason| TunnelEvent::ExitIpCheckFailed { reason };
        for (path, starter, checked, switched) in [
            (
                "ip",
                "true",
                TunnelEvent::ExitIpObserved {
                    ip: IpAddr::from([192, 0, 2, 7]),
                },
                TunnelEvent::Switched,
            ),
            (
                "garbage",
                "false",
                failed(ExitIpFailure::Malformed),
                TunnelEvent::SwitchFailed,
            ),
            (
                "down",
                "/nonexistent/systemctl",
                failed(ExitIpFailure::Rejected { status: 503 }),
                TunnelEvent::SwitchFailed,
            ),
            // Nothing listens on the discard port.
            (
                "http://127.0.0.1:9/ip",
                "true",
                failed(ExitIpFailure::Unreachable),
                TunnelEvent::Switched,
            ),
        ] {
            let client = Client::new(service.join(path)?, starter.into());
            let check = perform(Some(&client), Request::CheckExitIp).await;
            assert_eq!(check, checked, "{path}");
            let switch = perform(Some(&client), Request::Switch(server.clone())).await;
            assert_eq!(switch, switched, "{starter}");
        }
        let check = perform(None, Request::CheckExitIp).await;
        assert_eq!(check, failed(ExitIpFailure::Unreachable));
        let switch = perform(None, Request::Switch(server)).await;
        assert_eq!(switch, TunnelEvent::SwitchFailed);
        Ok(())
    }
}
