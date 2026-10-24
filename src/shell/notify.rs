//! Publish notices to ntfy, with a link to the dashboard page that explains them.
//!
//! Inside the VPN namespace, ntfy is reached through the host's relay. The
//! server's name is resolved to the relay, so HTTPS still verifies ntfy's
//! certificate end to end.

use std::{net::SocketAddr, time::Duration};

use reqwest::{Client as HttpClient, Url};

use crate::core::{
    DeliveryFailure, Notice, NotificationEvent, NtfyTopic, SettingName, StartupMode, SwitchBlock,
};

use super::describe::{qbittorrent_failure, timestamp};

const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// The hosted ntfy server, used unless another is configured.
pub(super) const NTFY_SH: &str = "https://ntfy.sh/";

/// Where notices are published, and the dashboard they link to.
#[derive(Debug, Clone)]
pub(super) struct Channel {
    http: HttpClient,
    server: Option<Url>,
    dashboard: Option<Url>,
}

impl Channel {
    /// A channel to `server`, connecting through `relay` when given.
    pub(super) fn new(
        server: Option<Url>,
        relay: Option<SocketAddr>,
        dashboard: Option<Url>,
    ) -> Self {
        let mut builder = HttpClient::builder().timeout(SEND_TIMEOUT).no_proxy();
        if let (Some(relay), Some(host)) = (relay, server.as_ref().and_then(Url::host_str)) {
            builder = builder.resolve(host, relay);
        }
        // Building fails only if the TLS backend cannot initialise; sends then
        // fail and are retried.
        let http = builder.build().unwrap_or_default();
        Self {
            http,
            server,
            dashboard,
        }
    }
}

/// What a notice says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Message {
    pub(super) title: &'static str,
    pub(super) body: String,
    /// ntfy's priority, from 1 (min) to 5 (urgent).
    pub(super) priority: u8,
    pub(super) tags: &'static str,
    /// The dashboard page that explains it.
    pub(super) page: &'static str,
}

/// Publish a notice and report the outcome.
pub(super) async fn send(
    channel: &Channel,
    topic: &NtfyTopic,
    notice: &Notice,
) -> NotificationEvent {
    match deliver(channel, topic, notice).await {
        Ok(()) => NotificationEvent::Delivered,
        Err(reason) => NotificationEvent::DeliveryFailed { reason },
    }
}

async fn deliver(
    channel: &Channel,
    topic: &NtfyTopic,
    notice: &Notice,
) -> Result<(), DeliveryFailure> {
    let url = channel
        .server
        .as_ref()
        .and_then(|server| server.join(topic.expose_secret()).ok())
        .ok_or(DeliveryFailure::Unreachable)?;
    let message = message(notice);
    let mut request = channel
        .http
        .post(url)
        .header("Title", message.title)
        .header("Priority", message.priority.to_string())
        .header("Tags", message.tags);
    if let Some(link) = channel
        .dashboard
        .as_ref()
        .and_then(|dashboard| dashboard.join(message.page).ok())
    {
        request = request.header("Click", link.as_str());
    }
    let response = request
        .body(message.body)
        .send()
        .await
        .map_err(|_| DeliveryFailure::Unreachable)?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(DeliveryFailure::Rejected {
            status: status.as_u16(),
        })
    }
}

fn torrents(count: usize) -> String {
    if count == 1 {
        String::from("1 torrent")
    } else {
        format!("{count} torrents")
    }
}

const fn setting(setting: SettingName) -> &'static str {
    match setting {
        SettingName::MamRequests => "The MAM request pause or resume",
        SettingName::HistoryLimit => "The history limit",
        SettingName::ApiCookie => "The MAM API cookie",
        SettingName::NtfyTopic => "The notification topic",
    }
}

fn blocked(reason: SwitchBlock) -> String {
    match reason {
        SwitchBlock::Until(at) => format!(
            "MAM allows one VPN IP change an hour, so Windlass can switch servers after {}.",
            timestamp(at)
        ),
        SwitchBlock::LastSwitchUnknown => String::from(
            "Windlass could not read when it last switched servers, so it will not switch.",
        ),
        SwitchBlock::NoOtherServer => String::from("No other VPN server is configured."),
    }
}

/// The words, priority, and dashboard page for a notice. Notices carry no
/// book titles or account details.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per notice reads best as one table"
)]
pub(super) fn message(notice: &Notice) -> Message {
    let message = |title, body: String, priority, tags, page| Message {
        title,
        body,
        priority,
        tags,
        page,
    };
    match notice {
        Notice::Started { mode } => message(
            "Windlass started",
            String::from(match mode {
                StartupMode::Initialise => "Windlass started with new storage.",
                StartupMode::Resume => "Windlass started and resumed its saved state.",
            }),
            2,
            "arrow_forward",
            "/",
        ),
        Notice::StartupFailed => message(
            "Windlass could not start",
            String::from(
                "Startup failed, so Windlass does nothing until it is restarted. The dashboard shows the storage error.",
            ),
            5,
            "rotating_light",
            "/",
        ),
        Notice::VpnProtectionLost { failures } => message(
            "VPN not working",
            format!("The last {failures} tunnel checks failed, so torrent traffic is stopped."),
            4,
            "warning",
            "/#tunnel",
        ),
        Notice::VpnProtectionRestored { exit_ip, server } => message(
            "VPN working again",
            format!(
                "Traffic flows through {} with exit IP {exit_ip}.",
                server
                    .as_ref()
                    .map_or_else(|| String::from("the VPN"), ToString::to_string)
            ),
            3,
            "white_check_mark",
            "/#tunnel",
        ),
        Notice::FailoverStarted { server } => message(
            "Switching VPN server",
            format!("The tunnel stopped working, so Windlass is switching to {server}."),
            4,
            "arrows_counterclockwise",
            "/#tunnel",
        ),
        Notice::FailoverSucceeded { server } => message(
            "VPN server switched",
            format!("Windlass now uses {server} and is checking the tunnel."),
            3,
            "white_check_mark",
            "/#tunnel",
        ),
        Notice::FailoverFailed { server } => message(
            "VPN server switch failed",
            format!("Switching to {server} failed. Windlass keeps checking the tunnel."),
            5,
            "rotating_light",
            "/#tunnel",
        ),
        Notice::FailoverBlocked { reason } => message(
            "VPN switch blocked",
            format!("The tunnel is not working. {}", blocked(*reason)),
            5,
            "rotating_light",
            "/#tunnel",
        ),
        Notice::ForwardedPortLost => message(
            "No forwarded port",
            String::from(
                "ProtonVPN has not granted a forwarded port for 5 minutes, so peers cannot connect in.",
            ),
            3,
            "warning",
            "/#port-forwarding",
        ),
        Notice::ForwardedPortRestored { port } => message(
            "Forwarded port back",
            format!("Peers can connect in on port {} again.", port.get()),
            2,
            "white_check_mark",
            "/#port-forwarding",
        ),
        Notice::QbittorrentUnreachable { reason } => message(
            "qBittorrent not responding",
            format!("{} for 5 minutes.", qbittorrent_failure(*reason)),
            4,
            "warning",
            "/#qbittorrent",
        ),
        Notice::QbittorrentReachable => message(
            "qBittorrent responding again",
            String::from("Windlass is in contact with qBittorrent again."),
            2,
            "white_check_mark",
            "/#qbittorrent",
        ),
        Notice::QbittorrentVersionNotAllowed { version } => message(
            "qBittorrent version not allowed",
            format!("MAM does not allow qBittorrent {version}. Use a 5.0.1 to 5.2.x release."),
            5,
            "rotating_light",
            "/#qbittorrent",
        ),
        Notice::QbittorrentVersionAllowed { version } => message(
            "qBittorrent version allowed",
            format!("MAM allows qBittorrent {version}."),
            2,
            "white_check_mark",
            "/#qbittorrent",
        ),
        Notice::TorrentErrors { count } => message(
            "Torrents with errors",
            format!("qBittorrent reports errors for {}.", torrents(*count)),
            4,
            "warning",
            "/#torrents",
        ),
        Notice::TorrentErrorsCleared => message(
            "Torrent errors cleared",
            String::from("No torrent reports an error."),
            2,
            "white_check_mark",
            "/#torrents",
        ),
        Notice::FilesMissing { count } => message(
            "Files missing on disk",
            format!(
                "{} {} missing files. Windlass does not download them again without you.",
                torrents(*count),
                if *count == 1 { "has" } else { "have" }
            ),
            4,
            "warning",
            "/#torrents",
        ),
        Notice::FilesMissingCleared => message(
            "Missing files resolved",
            String::from("No torrent has missing files."),
            2,
            "white_check_mark",
            "/#torrents",
        ),
        Notice::TorrentsMissing { count } => message(
            "Torrents missing from qBittorrent",
            format!(
                "{} Windlass manages {} not in qBittorrent.",
                torrents(*count),
                if *count == 1 { "is" } else { "are" }
            ),
            4,
            "warning",
            "/#torrents",
        ),
        Notice::TorrentsMissingCleared => message(
            "Torrents back in qBittorrent",
            String::from("Every torrent Windlass manages is in qBittorrent."),
            2,
            "white_check_mark",
            "/#torrents",
        ),
        Notice::RegistryNotSaved => message(
            "Torrent records not saved",
            String::from(
                "Windlass has not saved its torrent records for 2 minutes and keeps retrying.",
            ),
            4,
            "warning",
            "/#torrents",
        ),
        Notice::RegistrySaved => message(
            "Torrent records saved",
            String::from("Windlass saves its torrent records again."),
            2,
            "white_check_mark",
            "/#torrents",
        ),
        Notice::RegistryUnreadable => message(
            "Torrent records unreadable",
            String::from(
                "Windlass could not read its torrent records, so torrents are shown but not managed.",
            ),
            5,
            "rotating_light",
            "/#torrents",
        ),
        Notice::SettingNotSaved { setting: name } => message(
            "Setting not saved",
            format!("{} could not be saved.", setting(*name)),
            4,
            "warning",
            "/settings",
        ),
        Notice::HistoryUnavailable => message(
            "History unavailable",
            String::from("History storage could not be opened, so events are not recorded."),
            4,
            "warning",
            "/history",
        ),
        Notice::HistoryEntriesLost { count } => message(
            "History incomplete",
            format!("{} history entries could not be saved.", count.get()),
            3,
            "warning",
            "/history",
        ),
        Notice::Test => message(
            "Test notification",
            String::from("Notifications from Windlass reach this topic."),
            3,
            "bell",
            "/settings",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        sync::{Arc, Mutex},
        time::SystemTime,
    };

    use axum::{
        Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };

    use crate::core::{
        DeliveryFailure, EntryCount, Notice, NotificationEvent, NtfyTopic, Port,
        QbittorrentFailure, SettingName, StartupMode, SwitchBlock, VpnServer,
    };

    use super::{Channel, message, send};

    type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

    fn notices() -> Result<Vec<Notice>, Box<dyn Error + Send + Sync>> {
        let server = VpnServer::try_new("two")?;
        let ip = std::net::IpAddr::from([192, 0, 2, 7]);
        let version = String::from("v5.0.0");
        let mut notices = vec![
            Notice::Started {
                mode: StartupMode::Initialise,
            },
            Notice::Started {
                mode: StartupMode::Resume,
            },
            Notice::StartupFailed,
            Notice::VpnProtectionLost { failures: 3 },
            Notice::VpnProtectionRestored {
                exit_ip: ip,
                server: Some(server.clone()),
            },
            Notice::VpnProtectionRestored {
                exit_ip: ip,
                server: None,
            },
            Notice::FailoverStarted {
                server: server.clone(),
            },
            Notice::FailoverSucceeded {
                server: server.clone(),
            },
            Notice::FailoverFailed { server },
            Notice::ForwardedPortLost,
            Notice::ForwardedPortRestored {
                port: Port::new(41000).ok_or("zero port")?,
            },
            Notice::QbittorrentUnreachable {
                reason: QbittorrentFailure::Unreachable,
            },
            Notice::QbittorrentReachable,
            Notice::QbittorrentVersionNotAllowed {
                version: version.clone(),
            },
            Notice::QbittorrentVersionAllowed { version },
            Notice::TorrentErrorsCleared,
            Notice::FilesMissingCleared,
            Notice::TorrentsMissingCleared,
            Notice::RegistryNotSaved,
            Notice::RegistrySaved,
            Notice::RegistryUnreadable,
            Notice::HistoryUnavailable,
            Notice::HistoryEntriesLost {
                count: EntryCount::new(4),
            },
            Notice::Test,
        ];
        for count in [1, 2] {
            notices.extend([
                Notice::TorrentErrors { count },
                Notice::FilesMissing { count },
                Notice::TorrentsMissing { count },
            ]);
        }
        notices.extend(
            [
                SwitchBlock::Until(SystemTime::UNIX_EPOCH),
                SwitchBlock::LastSwitchUnknown,
                SwitchBlock::NoOtherServer,
            ]
            .map(|reason| Notice::FailoverBlocked { reason }),
        );
        notices.extend(
            [
                SettingName::MamRequests,
                SettingName::HistoryLimit,
                SettingName::ApiCookie,
                SettingName::NtfyTopic,
            ]
            .map(|setting| Notice::SettingNotSaved { setting }),
        );
        Ok(notices)
    }

    #[test]
    fn every_notice_has_words_a_priority_and_a_page() -> TestResult {
        let notices = notices()?;
        let mut bodies = std::collections::BTreeSet::new();
        for notice in &notices {
            let message = message(notice);
            assert!(
                !message.title.is_empty() && message.title.is_ascii(),
                "{notice:?}"
            );
            assert!((1..=5).contains(&message.priority), "{notice:?}");
            assert!(message.page.starts_with('/'), "{notice:?}");
            bodies.insert(message.body);
        }
        assert_eq!(
            bodies.len(),
            notices.len(),
            "every notice reads differently"
        );
        let one = message(&Notice::FilesMissing { count: 1 }).body;
        assert!(one.starts_with("1 torrent has"), "{one}");
        Ok(())
    }

    #[derive(Debug, Default)]
    struct Received {
        host: Option<String>,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    /// A fake ntfy that records the request and answers with `status`.
    async fn ntfy(
        status: StatusCode,
    ) -> Result<(std::net::SocketAddr, Arc<Mutex<Received>>), Box<dyn Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let received = Arc::new(Mutex::new(Received::default()));
        let record = Arc::clone(&received);
        let routes = Router::new().route(
            "/{topic}",
            post(
                move |axum::extract::Path(topic): axum::extract::Path<String>,
                      headers: HeaderMap,
                      body: String| async move {
                    if let Ok(mut received) = record.lock() {
                        let header = |name: &str| {
                            headers
                                .get(name)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned)
                        };
                        received.host = header("host");
                        received.path = topic;
                        received.headers = ["Title", "Priority", "Tags", "Click"]
                            .into_iter()
                            .filter_map(|name| Some((name.to_owned(), header(name)?)))
                            .collect();
                        received.body = body;
                    }
                    status
                },
            ),
        );
        tokio::spawn(async move { axum::serve(listener, routes).await });
        Ok((address, received))
    }

    #[tokio::test]
    async fn notices_reach_ntfy_through_the_relay_with_a_dashboard_link() -> TestResult {
        let (relay, received) = ntfy(StatusCode::OK).await?;
        let channel = Channel::new(
            Some("http://ntfy.test/".parse()?),
            Some(relay),
            Some("http://windlass.test:8740/".parse()?),
        );
        let topic = NtfyTopic::parse("windlass-alerts")?;
        assert_eq!(
            send(&channel, &topic, &Notice::FilesMissing { count: 2 }).await,
            NotificationEvent::Delivered
        );
        let received = std::mem::take(&mut *received.lock().map_err(|_| "poisoned")?);
        assert_eq!(received.host.as_deref(), Some("ntfy.test"));
        assert_eq!(received.path, "windlass-alerts");
        let header = |name: &str| {
            received
                .headers
                .iter()
                .find(|(header, _)| header == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("Title"), Some("Files missing on disk"));
        assert_eq!(header("Priority"), Some("4"));
        assert_eq!(header("Tags"), Some("warning"));
        assert_eq!(header("Click"), Some("http://windlass.test:8740/#torrents"));
        assert!(received.body.starts_with("2 torrents have missing files"));
        Ok(())
    }

    #[tokio::test]
    async fn failed_deliveries_say_why() -> TestResult {
        let (refusing, _) = ntfy(StatusCode::TOO_MANY_REQUESTS).await?;
        let topic = NtfyTopic::parse("windlass-alerts")?;
        let base = format!("http://{refusing}/").parse()?;
        // Without a dashboard address, notices carry no link.
        let refused = Channel::new(Some(base), None, None);
        let closed = Channel::new(Some("http://127.0.0.1:9/".parse()?), None, None);
        let unconfigured = Channel::new(None, None, None);
        for (channel, reason) in [
            (refused, DeliveryFailure::Rejected { status: 429 }),
            (closed, DeliveryFailure::Unreachable),
            (unconfigured, DeliveryFailure::Unreachable),
        ] {
            assert_eq!(
                send(&channel, &topic, &Notice::Test).await,
                NotificationEvent::DeliveryFailed { reason }
            );
        }
        Ok(())
    }
}
