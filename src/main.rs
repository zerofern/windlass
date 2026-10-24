//! Run Windlass: the web dashboard, or a minimal console for inspecting core
//! and shell interactions.

use std::{
    error::Error,
    ffi::OsString,
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
};

use tokio::net::TcpListener;
use windlass::{
    core::{StartupMode, VpnServer},
    shell::{Integrations, TunnelChecks, console, web},
};

const USAGE: &str = "usage: windlass <initialise|resume> <database-path>\n       \
windlass serve <initialise|resume> <database-path> --listen <address>... [--allowed-host <name>]... \
[--qbittorrent <webui-url>] [--nat-pmp-gateway <address>] \
[--exit-ip-service <url> [--vpn-server <name>]... [--vpn-current-file <path>]] \
[--ntfy-server <url>] [--ntfy-relay <address>] [--dashboard-url <url>]";

type MainResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn usage(detail: &str) -> Box<dyn Error + Send + Sync> {
    io::Error::other(format!("{detail}\n{USAGE}")).into()
}

fn startup_mode(argument: Option<&std::ffi::OsStr>) -> MainResult<StartupMode> {
    match argument.and_then(std::ffi::OsStr::to_str) {
        Some("initialise") => Ok(StartupMode::Initialise),
        Some("resume") => Ok(StartupMode::Resume),
        _ => Err(usage("expected initialise or resume")),
    }
}

#[tokio::main]
async fn main() -> MainResult<()> {
    let mut arguments = std::env::args_os().skip(1).peekable();
    if arguments.peek().is_some_and(|argument| argument == "serve") {
        arguments.next();
        return serve(arguments).await;
    }
    let mode = startup_mode(arguments.next().as_deref())?;
    let database = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage("missing database path"))?;
    if arguments.next().is_some() {
        return Err(usage("unexpected extra argument"));
    }
    let status = console::run(
        &database,
        mode,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await?;
    if status.has_failed() {
        return Err(io::Error::other("startup or preference persistence failed").into());
    }
    Ok(())
}

async fn serve(mut arguments: impl Iterator<Item = OsString>) -> MainResult<()> {
    let mode = startup_mode(arguments.next().as_deref())?;
    let database = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage("missing database path"))?;
    let mut listen = Vec::new();
    let mut allowed_hosts = Vec::new();
    let mut integrations = Integrations::default();
    let mut exit_ip_service = None;
    let mut servers = Vec::new();
    let mut current_file = None;
    while let Some(option) = arguments.next() {
        let value = arguments
            .next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| usage("missing option value"))?;
        match option.to_str() {
            Some("--listen") => listen.push(
                value
                    .parse::<SocketAddr>()
                    .map_err(|_| usage("invalid listen address"))?,
            ),
            Some("--allowed-host") => allowed_hosts.push(value),
            Some("--qbittorrent") => {
                integrations.qbittorrent = Some(
                    value
                        .parse()
                        .map_err(|_| usage("invalid qBittorrent WebUI URL"))?,
                );
            }
            Some("--nat-pmp-gateway") => {
                integrations.nat_pmp_gateway = Some(nat_pmp_gateway(&value)?);
            }
            Some("--exit-ip-service") => {
                exit_ip_service = Some(
                    value
                        .parse()
                        .map_err(|_| usage("invalid exit-IP service URL"))?,
                );
            }
            Some("--vpn-server") => servers
                .push(VpnServer::try_new(value).map_err(|_| usage("invalid VPN server name"))?),
            Some("--vpn-current-file") => current_file = Some(PathBuf::from(value)),
            Some("--ntfy-server") => {
                integrations.ntfy_server = Some(
                    value
                        .parse()
                        .map_err(|_| usage("invalid ntfy server URL"))?,
                );
            }
            Some("--ntfy-relay") => {
                integrations.ntfy_relay = Some(
                    value
                        .parse()
                        .map_err(|_| usage("invalid ntfy relay address"))?,
                );
            }
            Some("--dashboard-url") => {
                integrations.dashboard_url =
                    Some(value.parse().map_err(|_| usage("invalid dashboard URL"))?);
            }
            _ => return Err(usage("unknown option")),
        }
    }
    integrations.tunnel = match exit_ip_service {
        Some(exit_ip_service) => Some(TunnelChecks {
            exit_ip_service,
            servers,
            current: current_file.as_deref().and_then(current_server),
            unit_starter: PathBuf::from("systemctl"),
        }),
        None if servers.is_empty() && current_file.is_none() => None,
        None => return Err(usage("VPN servers need --exit-ip-service")),
    };
    if listen.is_empty() {
        return Err(usage("at least one --listen address is required"));
    }
    let mut listeners = Vec::new();
    for address in listen {
        listeners.push(TcpListener::bind(address).await?);
    }
    let options = web::Serve {
        database,
        mode,
        listeners,
        allowed_hosts,
        integrations,
    };
    let state = web::serve(options, stop_signal()).await?;
    if state.has_failed() {
        return Err(io::Error::other("startup or preference persistence failed").into());
    }
    Ok(())
}

/// An IPv4 gateway address, with NAT-PMP's port 5351 unless another is given.
fn nat_pmp_gateway(value: &str) -> MainResult<SocketAddrV4> {
    value
        .parse::<SocketAddrV4>()
        .or_else(|_| {
            value
                .parse::<Ipv4Addr>()
                .map(|address| SocketAddrV4::new(address, 5351))
        })
        .map_err(|_| usage("invalid NAT-PMP gateway address"))
}

/// The server the NixOS module last connected to, as it records it; absent
/// or unreadable means unknown.
fn current_server(path: &std::path::Path) -> Option<VpnServer> {
    let name = std::fs::read_to_string(path).ok()?;
    VpnServer::try_new(name.trim()).ok()
}

/// Complete on Ctrl-C or, from systemd, SIGTERM.
async fn stop_signal() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(_) => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = interrupt => {}
            () = terminate => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = interrupt.await;
    }
}
