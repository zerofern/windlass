//! Push notices to ntfy. A push carries no words about the notice, only a link
//! to its entry on the dashboard's notifications page, so nothing about the
//! owner's torrents or account leaves their network.
//!
//! Inside the VPN namespace, ntfy is reached through the host's relay. The
//! server's name is resolved to the relay, so HTTPS still verifies ntfy's
//! certificate end to end.

use std::{net::SocketAddr, time::Duration};

use reqwest::{Client as HttpClient, Url};

use crate::core::{
    ClientWarning, DeliveryFailure, Notice, NotificationEvent, NtfyTopic, Partials, Push, Sale,
    SettingName, StartupMode, SwitchBlock, TorrentCount, Urgency,
};

use super::describe::{hold, purchase, qbittorrent_failure, rejection, timestamp};

/// MAM's main page, where the visit reminder leads.
const MAM_HOME: &str = "https://www.myanonamouse.net/";

/// What every push says.
const PUSH_TITLE: &str = "Windlass";
const PUSH_BODY: &str = "Windlass has a new notice. Open it to read it on the dashboard.";

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
    /// The dashboard page that explains it.
    pub(super) page: &'static str,
}

/// Push a notice and report the outcome.
pub(super) async fn send(channel: &Channel, topic: &NtfyTopic, push: &Push) -> NotificationEvent {
    match deliver(channel, topic, push).await {
        Ok(()) => NotificationEvent::Delivered,
        Err(reason) => NotificationEvent::DeliveryFailed { reason },
    }
}

/// The notice's entry on the notifications page, or the page itself.
pub(super) fn entry_page(push: &Push) -> String {
    push.entry.map_or_else(
        || String::from("/notifications"),
        |id| format!("/notifications#notice-{}", id.get()),
    )
}

async fn deliver(channel: &Channel, topic: &NtfyTopic, push: &Push) -> Result<(), DeliveryFailure> {
    let url = channel
        .server
        .as_ref()
        .and_then(|server| server.join(topic.expose_secret()).ok())
        .ok_or(DeliveryFailure::Unreachable)?;
    let mut request = channel
        .http
        .post(url)
        .header("Title", PUSH_TITLE)
        .header("Priority", message(&push.notice).priority.to_string());
    let link = channel
        .dashboard
        .as_ref()
        .and_then(|dashboard| dashboard.join(&entry_page(push)).ok());
    if let Some(link) = link {
        request = request.header("Click", link.as_str());
    }
    let response = request
        .body(PUSH_BODY)
        .send()
        .await
        .map_err(|_| DeliveryFailure::Unreachable)?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(DeliveryFailure::Rejected {
            status: crate::core::HttpStatus::new(status),
        })
    }
}

fn torrents(count: TorrentCount) -> String {
    if count.get() == 1 {
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
        SettingName::PointsReserve => "The bonus points reserve",
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
    let message = |title, body: String, priority, page| Message {
        title,
        body,
        priority,
        page,
    };
    let at_mam = |title, body, priority| message(title, body, priority, "/#mam");
    match notice {
        Notice::Started { mode } => message(
            "Windlass started",
            String::from(match mode {
                StartupMode::Initialise => "Windlass started with new storage.",
                StartupMode::Resume => "Windlass started and resumed its saved state.",
            }),
            2,
            "/",
        ),
        Notice::StartupFailed => message(
            "Windlass could not start",
            String::from(
                "Startup failed, so Windlass does nothing until it is restarted. The dashboard shows the storage error.",
            ),
            5,
            "/",
        ),
        Notice::VpnProtectionLost { failures } => message(
            "VPN not working",
            format!("The last {failures} tunnel checks failed, so torrent traffic is stopped."),
            4,
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
            "/#tunnel",
        ),
        Notice::FailoverStarted { server } => message(
            "Switching VPN server",
            format!("The tunnel stopped working, so Windlass is switching to {server}."),
            4,
            "/#tunnel",
        ),
        Notice::FailoverSucceeded { server } => message(
            "VPN server switched",
            format!("Windlass now uses {server} and is checking the tunnel."),
            3,
            "/#tunnel",
        ),
        Notice::FailoverFailed { server } => message(
            "VPN server switch failed",
            format!("Switching to {server} failed. Windlass keeps checking the tunnel."),
            5,
            "/#tunnel",
        ),
        Notice::FailoverBlocked { reason } => message(
            "VPN switch blocked",
            format!("The tunnel is not working. {}", blocked(*reason)),
            5,
            "/#tunnel",
        ),
        Notice::ForwardedPortLost => message(
            "No forwarded port",
            String::from(
                "ProtonVPN has not granted a forwarded port for 5 minutes, so peers cannot connect in.",
            ),
            3,
            "/#port-forwarding",
        ),
        Notice::ForwardedPortRestored { port } => message(
            "Forwarded port back",
            format!("Peers can connect in on port {} again.", port.get()),
            2,
            "/#port-forwarding",
        ),
        Notice::QbittorrentUnreachable { reason } => message(
            "qBittorrent not responding",
            format!("{} for 5 minutes.", qbittorrent_failure(*reason)),
            4,
            "/#qbittorrent",
        ),
        Notice::QbittorrentReachable => message(
            "qBittorrent responding again",
            String::from("Windlass is in contact with qBittorrent again."),
            2,
            "/#qbittorrent",
        ),
        Notice::QbittorrentVersionNotAllowed { version } => message(
            "qBittorrent version not allowed",
            format!("MAM does not allow qBittorrent {version}. Use a 5.0.1 to 5.2.x release."),
            5,
            "/#qbittorrent",
        ),
        Notice::QbittorrentVersionAllowed { version } => message(
            "qBittorrent version allowed",
            format!("MAM allows qBittorrent {version}."),
            2,
            "/#qbittorrent",
        ),
        Notice::TorrentErrors { count } => message(
            "Torrents with errors",
            format!("qBittorrent reports errors for {}.", torrents(*count)),
            4,
            "/#torrents",
        ),
        Notice::TorrentErrorsCleared => message(
            "Torrent errors cleared",
            String::from("No torrent reports an error."),
            2,
            "/#torrents",
        ),
        Notice::FilesMissing { count } => message(
            "Files missing on disk",
            format!(
                "{} {} missing files. Windlass does not download them again without you.",
                torrents(*count),
                if count.get() == 1 { "has" } else { "have" }
            ),
            4,
            "/#torrents",
        ),
        Notice::FilesMissingCleared => message(
            "Missing files resolved",
            String::from("No torrent has missing files."),
            2,
            "/#torrents",
        ),
        Notice::TorrentsMissing { count } => message(
            "Torrents missing from qBittorrent",
            format!(
                "{} Windlass manages {} not in qBittorrent.",
                torrents(*count),
                if count.get() == 1 { "is" } else { "are" }
            ),
            4,
            "/#torrents",
        ),
        Notice::TorrentsMissingCleared => message(
            "Torrents back in qBittorrent",
            String::from("Every torrent Windlass manages is in qBittorrent."),
            2,
            "/#torrents",
        ),
        Notice::RegistryNotSaved => message(
            "Torrent records not saved",
            String::from(
                "Windlass has not saved its torrent records for 2 minutes and keeps retrying.",
            ),
            4,
            "/#torrents",
        ),
        Notice::RegistrySaved => message(
            "Torrent records saved",
            String::from("Windlass saves its torrent records again."),
            2,
            "/#torrents",
        ),
        Notice::RegistryUnreadable => message(
            "Torrent records unreadable",
            String::from(
                "Windlass could not read its torrent records, so torrents are shown but not managed.",
            ),
            5,
            "/#torrents",
        ),
        Notice::SettingNotSaved { setting: name } => message(
            "Setting not saved",
            format!("{} could not be saved.", setting(*name)),
            4,
            "/settings",
        ),
        Notice::HistoryUnavailable => message(
            "History unavailable",
            String::from("History storage could not be opened, so events are not recorded."),
            4,
            "/history",
        ),
        Notice::HistoryEntriesLost { count } => message(
            "History incomplete",
            format!("{} history entries could not be saved.", count.get()),
            3,
            "/history",
        ),
        Notice::Test => message(
            "Test notification",
            String::from("Notifications from Windlass reach this topic."),
            3,
            "/settings",
        ),
        Notice::MamSessionRejected { rejection: reason } => at_mam(
            "MAM refused the API session",
            format!(
                "MAM requests that need the cookie have stopped: {}. Create a new session from the VPN and enter it on the settings page.",
                rejection(*reason)
            ),
            5,
        ),
        Notice::MamSessionAccepted => at_mam(
            "MAM session working",
            String::from("MAM accepts the API session again."),
            2,
        ),
        Notice::SeedboxNotRegistered => at_mam(
            "Seedbox IP not registered",
            String::from(
                "MAM has not registered the VPN's IP as the seedbox, so new downloads wait. The dashboard shows why and when Windlass tries again.",
            ),
            4,
        ),
        Notice::SeedboxRegistered { ip } => at_mam(
            "Seedbox IP registered",
            format!("MAM has {ip} as the seedbox IP."),
            2,
        ),
        Notice::MamIpUnknown { failures } => at_mam(
            "Cannot see MAM's view of the IP",
            format!("The last {failures} IP lookups at MAM failed, so new downloads wait."),
            3,
        ),
        Notice::MamIpKnown => at_mam(
            "MAM IP lookups working",
            String::from("Windlass sees the IP MAM sees again."),
            2,
        ),
        Notice::MamStateUnreadable => at_mam(
            "MAM state unreadable",
            String::from(
                "Windlass could not read when it last registered the seedbox IP, so it will not register until restarted.",
            ),
            5,
        ),
        Notice::RatioLow {
            ratio: value,
            urgency,
        } => at_mam(
            "Ratio below 2.0",
            format!(
                "MAM reports a ratio of {value}. Every download is held until you find out why."
            ),
            match urgency {
                Urgency::Urgent => 5,
                Urgency::Normal => 4,
            },
        ),
        Notice::RatioRestored => at_mam(
            "Ratio back above 2.0",
            String::from("MAM reports a ratio of 2.0 or more."),
            2,
        ),
        Notice::UnsatisfiedNearLimit { count, limit } => at_mam(
            "Unsatisfied torrents near the limit",
            format!("MAM counts {count} unsatisfied torrents of a limit of {limit}."),
            4,
        ),
        Notice::UnsatisfiedBelowLimit => at_mam(
            "Unsatisfied torrents below 80%",
            String::from("MAM's unsatisfied count is below 80% of the limit again."),
            2,
        ),
        Notice::UnsatisfiedNotSeeding { count } => at_mam(
            "Unsatisfied torrents not seeding",
            format!(
                "MAM sees {count} unsatisfied torrents that are not seeding. They must seed 72 hours within 30 days."
            ),
            4,
        ),
        Notice::UnsatisfiedSeeding => at_mam(
            "Unsatisfied torrents seeding",
            String::from("MAM sees every unsatisfied torrent seeding."),
            2,
        ),
        Notice::MamFlagged {
            partial,
            tracker_errors,
            flagged,
        } => at_mam(
            "MAM flags the account's torrents",
            format!(
                "MAM reports{}{}{}.",
                match partial {
                    Partials::Seen => " a partial download;",
                    Partials::NotSeen => "",
                },
                if tracker_errors.is_zero() {
                    String::new()
                } else {
                    format!(" {tracker_errors} tracker errors;")
                },
                if flagged.is_zero() {
                    String::new()
                } else {
                    format!(" {flagged} torrents in categories it marks red")
                }
            ),
            4,
        ),
        Notice::MamFlagsCleared => at_mam(
            "MAM flags cleared",
            String::from("MAM no longer flags any of the account's torrents."),
            2,
        ),
        Notice::MamMessages {
            count,
            client_warning,
        } => at_mam(
            if *client_warning == ClientWarning::Given {
                "MAM warns about a client"
            } else {
                "MAM has notifications"
            },
            format!(
                "MAM has {count} notifications for you{}. Windlass does not read them.",
                if *client_warning == ClientWarning::Given {
                    ", including a warning that it is about to stop accepting a client"
                } else {
                    ""
                }
            ),
            match client_warning {
                ClientWarning::Given => 5,
                ClientWarning::NotGiven => 3,
            },
        ),
        Notice::DownloadsHeld { hold: reason } => at_mam(
            "Downloads held",
            format!(
                "Bookmarked torrents wait for 30 minutes now: {}.",
                hold(*reason)
            ),
            3,
        ),
        Notice::DownloadsResumed => at_mam(
            "Downloads resumed",
            String::from("Bookmarked torrents download again."),
            2,
        ),
        Notice::NoWedgesLeft => at_mam(
            "No wedges left",
            String::from(
                "Torrents that are not freeleech for you wait, because no wedge is left and the store does not sell them through the API.",
            ),
            4,
        ),
        Notice::PointsBelowReserve { points, reserve } => at_mam(
            "Bonus points below the reserve",
            format!(
                "MAM reports {points} points, below the reserve of {reserve}. Windlass buys nothing."
            ),
            3,
        ),
        Notice::VipEnding { until } => at_mam(
            "VIP ending",
            format!(
                "VIP ends at {} and Windlass has not renewed it, usually because the points would fall below the reserve.",
                timestamp(*until)
            ),
            4,
        ),
        Notice::DownloadsRefused => at_mam(
            "MAM refused a download",
            String::from(
                "MAM answered a download without a torrent file, so downloads are held until Windlass restarts or the cookie changes.",
            ),
            5,
        ),
        Notice::SeedingBehind { count } => at_mam(
            "Torrents behind on seeding",
            format!(
                "{} completed 20 or more days ago {} not seeded 96 hours yet. MAM requires 72 within 30 days; Windlass aims a day higher.",
                torrents(*count),
                if count.get() == 1 { "has" } else { "have" }
            ),
            4,
        ),
        Notice::SeedingOnTrack => at_mam(
            "Seeding on track",
            String::from("Every torrent is on track to seed 96 hours, a day more than MAM's 72."),
            2,
        ),
        Notice::PurchaseMade {
            purchase: bought,
            points,
        } => at_mam(
            "Bonus points spent",
            format!(
                "Windlass bought {}; {points} points are left.",
                purchase(*bought)
            ),
            3,
        ),
        Notice::PurchaseRefused { purchase: wanted } => at_mam(
            "Purchase refused",
            format!(
                "MAM refused to sell {}. Windlass waits 6 hours before trying again.",
                purchase(*wanted)
            ),
            4,
        ),
        Notice::PurchaseUncertain { purchase: wanted } => at_mam(
            "Purchase uncertain",
            format!(
                "Windlass does not know whether MAM sold {}, so it buys nothing until it reads MAM's figures again.",
                purchase(*wanted)
            ),
            4,
        ),
        Notice::DownloadNotFree {
            outcome: crate::core::NotFreeOutcome::Stopped,
        } => at_mam(
            "Download stopped",
            String::from(
                "A download is no longer free for you, and no wedge could be spent on it, so Windlass stopped it to protect the ratio. It resumes if it becomes free or a wedge is available.",
            ),
            4,
        ),
        Notice::DownloadNotFree {
            outcome: crate::core::NotFreeOutcome::FinishedFirst,
        } => at_mam(
            "Download may have cost ratio",
            String::from(
                "A download finished before Windlass could confirm it was free for you. Check your ratio on MAM.",
            ),
            4,
        ),
        Notice::WedgedMidDownload => at_mam(
            "Wedge spent on a running download",
            String::from(
                "A download's freeleech ended before it finished, so Windlass spent a wedge on it.",
            ),
            2,
        ),
        Notice::PurchaseSettled {
            purchase: wanted,
            sale,
        } => at_mam(
            "Purchase settled",
            format!(
                "The uncertain purchase of {} {}; Windlass buys again only as needed.",
                purchase(*wanted),
                if *sale == Sale::Made {
                    "went through"
                } else {
                    "did not go through"
                }
            ),
            3,
        ),
        Notice::PurchaseNeedsChecking { purchase: wanted } => at_mam(
            "Check a purchase on MAM",
            format!(
                "MAM's figures do not show whether it sold {}. Check your bonus points on MAM, then say on the dashboard whether it went through; Windlass buys nothing until then.",
                purchase(*wanted)
            ),
            4,
        ),
        Notice::WedgeNotSpent => at_mam(
            "Wedge not spent",
            String::from(
                "MAM's bonus history shows no wedge spent on a wedged download, so Windlass stopped the torrent to protect the ratio.",
            ),
            5,
        ),
        Notice::MamClientNotListed { standing } => at_mam(
            "qBittorrent not listed by MAM",
            format!(
                "Downloads are held: {}. Check MAM's client list on its site.",
                super::describe::client_standing(*standing)
            ),
            4,
        ),
        Notice::MamClientListed => at_mam(
            "qBittorrent listed by MAM",
            String::from("MAM lists the running qBittorrent again."),
            2,
        ),
        Notice::NotConnectable { findings } => at_mam(
            "qBittorrent not connectable",
            format!(
                "MAM still cannot reach qBittorrent after Windlass's repair. {}. Check that the namespace firewall admits the forwarded port.",
                super::describe::findings(findings)
            ),
            4,
        ),
        Notice::ConnectableAgain => at_mam(
            "qBittorrent connectable",
            String::from("MAM reaches qBittorrent again."),
            2,
        ),
        Notice::MamVisitReminder => message(
            "Visit MAM",
            String::from(
                "Your weekly reminder: visit MAM to keep the account active, and read its announcements about approved clients.",
            ),
            3,
            MAM_HOME,
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

    use super::{PUSH_BODY, PUSH_TITLE};
    use crate::core::{
        ClientStanding, DeliveryFailure, EntryCount, Hold, Notice, NotificationEvent, NtfyTopic,
        Points, Port, Purchase, Push, QbittorrentFailure, SessionRejection, SettingName,
        StartupMode, SwitchBlock, UploadCredit, VpnServer,
    };
    use crate::core::{NoticeCount, Partials, TorrentCount, UnsatisfiedCount, UnsatisfiedLimit};

    use super::{Channel, message, send};

    type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

    fn notices() -> Result<Vec<Notice>, Box<dyn Error + Send + Sync>> {
        let server = VpnServer::try_new("two")?;
        let ip = std::net::IpAddr::from([192, 0, 2, 7]);
        let version =
            crate::core::ReportedVersion::Release(crate::core::ClientVersion::new(5, 0, 0));
        let mut notices = vec![
            Notice::Started {
                mode: StartupMode::Initialise,
            },
            Notice::Started {
                mode: StartupMode::Resume,
            },
            Notice::StartupFailed,
            Notice::VpnProtectionLost {
                failures: crate::core::FailureCount::new(3),
            },
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
        for count in [TorrentCount::new(1), TorrentCount::new(2)] {
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
                SettingName::PointsReserve,
            ]
            .map(|setting| Notice::SettingNotSaved { setting }),
        );
        notices.extend(mam_notices()?);
        Ok(notices)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one list of every MAM notice reads best together"
    )]
    fn mam_notices() -> Result<Vec<Notice>, Box<dyn Error + Send + Sync>> {
        let upload = Purchase::Upload(UploadCredit::try_new(50)?);
        let mut notices = vec![
            Notice::MamSessionAccepted,
            Notice::SeedboxNotRegistered,
            Notice::SeedboxRegistered {
                ip: std::net::IpAddr::from([192, 0, 2, 10]),
            },
            Notice::MamIpUnknown {
                failures: crate::core::FailureCount::new(3),
            },
            Notice::MamIpKnown,
            Notice::MamStateUnreadable,
            Notice::RatioLow {
                ratio: crate::core::Ratio::from_thousandths(1500),
                urgency: crate::core::Urgency::Normal,
            },
            Notice::RatioLow {
                ratio: crate::core::Ratio::from_thousandths(1050),
                urgency: crate::core::Urgency::Urgent,
            },
            Notice::RatioRestored,
            Notice::UnsatisfiedNearLimit {
                count: UnsatisfiedCount::new(121),
                limit: UnsatisfiedLimit::new(150),
            },
            Notice::UnsatisfiedBelowLimit,
            Notice::UnsatisfiedNotSeeding {
                count: TorrentCount::new(2),
            },
            Notice::UnsatisfiedSeeding,
            Notice::MamFlagged {
                partial: Partials::Seen,
                tracker_errors: TorrentCount::new(1),
                flagged: TorrentCount::new(2),
            },
            Notice::MamFlagged {
                partial: Partials::NotSeen,
                tracker_errors: TorrentCount::new(0),
                flagged: TorrentCount::new(1),
            },
            Notice::MamFlagged {
                partial: Partials::NotSeen,
                tracker_errors: TorrentCount::new(2),
                flagged: TorrentCount::new(0),
            },
            Notice::MamFlagsCleared,
            Notice::MamMessages {
                count: NoticeCount::new(2),
                client_warning: crate::core::ClientWarning::NotGiven,
            },
            Notice::MamMessages {
                count: NoticeCount::new(1),
                client_warning: crate::core::ClientWarning::Given,
            },
            Notice::DownloadsResumed,
            Notice::NoWedgesLeft,
            Notice::PointsBelowReserve {
                points: Points::new(10_000),
                reserve: Points::new(20_000),
            },
            Notice::VipEnding {
                until: SystemTime::UNIX_EPOCH,
            },
            Notice::DownloadsRefused,
            Notice::SeedingBehind {
                count: TorrentCount::new(1),
            },
            Notice::SeedingBehind {
                count: TorrentCount::new(3),
            },
            Notice::SeedingOnTrack,
            Notice::PurchaseMade {
                purchase: upload,
                points: Points::new(35_000),
            },
            Notice::PurchaseMade {
                purchase: Purchase::Vip,
                points: Points::new(35_000),
            },
            Notice::PurchaseRefused { purchase: upload },
            Notice::PurchaseUncertain { purchase: upload },
            Notice::WedgeNotSpent,
            Notice::MamVisitReminder,
            Notice::MamClientListed,
            Notice::ConnectableAgain,
            Notice::PurchaseSettled {
                purchase: Purchase::Vip,
                sale: crate::core::Sale::Made,
            },
            Notice::PurchaseSettled {
                purchase: Purchase::Vip,
                sale: crate::core::Sale::NotMade,
            },
            Notice::PurchaseNeedsChecking {
                purchase: Purchase::Vip,
            },
            Notice::DownloadNotFree {
                outcome: crate::core::NotFreeOutcome::Stopped,
            },
            Notice::DownloadNotFree {
                outcome: crate::core::NotFreeOutcome::FinishedFirst,
            },
            Notice::WedgedMidDownload,
            Notice::NotConnectable {
                findings: crate::core::Findings {
                    listen_port: crate::core::PortFinding::NoForwardedPort,
                    lease: crate::core::LeaseRenewal::NotRequested,
                    exit_ip: crate::core::IpFinding::Unknown,
                    seedbox: crate::core::SeedboxConfirmation::Unconfirmed,
                    interface: crate::core::NetworkInterface::try_new("wg0").ok(),
                },
            },
        ];
        notices.extend(
            [
                ClientStanding::NotListed,
                ClientStanding::Refused,
                ClientStanding::NotUnderstood,
            ]
            .map(|standing| Notice::MamClientNotListed { standing }),
        );
        notices.extend(
            [
                SessionRejection::InvalidCookie,
                SessionRejection::IpMismatch,
                SessionRejection::AsnMismatch,
                SessionRejection::NotAllowed,
                SessionRejection::Other,
            ]
            .map(|rejection| Notice::MamSessionRejected { rejection }),
        );
        notices.extend(
            [
                Hold::SeedboxUnconfirmed,
                Hold::DownloadsRefused,
                Hold::ClientUnavailable,
                Hold::AccountUnknown,
                Hold::RatioLow,
                Hold::UnsatisfiedLimit {
                    count: UnsatisfiedCount::new(148),
                    limit: UnsatisfiedLimit::new(150),
                },
                Hold::NoWedge,
                Hold::NotBookmarked,
                Hold::MamUnavailable,
            ]
            .map(|hold| Notice::DownloadsHeld { hold }),
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
            assert!(
                message.page.starts_with('/') || message.page == super::MAM_HOME,
                "{notice:?}"
            );
            bodies.insert(message.body);
        }
        assert_eq!(
            bodies.len(),
            notices.len(),
            "every notice reads differently"
        );
        let one = message(&Notice::FilesMissing {
            count: TorrentCount::new(1),
        })
        .body;
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
    async fn pushes_reach_ntfy_through_the_relay_with_only_a_link() -> TestResult {
        let (relay, recorded) = ntfy(StatusCode::OK).await?;
        let channel = Channel::new(
            Some("http://ntfy.test/".parse()?),
            Some(relay),
            Some("http://windlass.test:8740/".parse()?),
        );
        let topic = NtfyTopic::parse("windlass-alerts")?;
        let push = Push {
            entry: crate::core::NoticeId::new(42),
            notice: Notice::FilesMissing {
                count: TorrentCount::new(2),
            },
        };
        assert_eq!(
            send(&channel, &topic, &push).await,
            NotificationEvent::Delivered
        );
        let received = std::mem::take(&mut *recorded.lock().map_err(|_| "poisoned")?);
        assert_eq!(received.host.as_deref(), Some("ntfy.test"));
        assert_eq!(received.path, "windlass-alerts");
        assert_eq!(
            received.headers,
            [
                (String::from("Title"), String::from(PUSH_TITLE)),
                (String::from("Priority"), String::from("4")),
                (
                    String::from("Click"),
                    String::from("http://windlass.test:8740/notifications#notice-42")
                ),
            ]
        );
        assert_eq!(received.body, PUSH_BODY);

        // Without an entry, the push leads to the notifications page.
        let push = Push {
            entry: None,
            notice: Notice::MamVisitReminder,
        };
        assert_eq!(
            send(&channel, &topic, &push).await,
            NotificationEvent::Delivered
        );
        let received = std::mem::take(&mut *recorded.lock().map_err(|_| "poisoned")?);
        assert!(received.headers.contains(&(
            String::from("Click"),
            String::from("http://windlass.test:8740/notifications")
        )));
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
            (
                refused,
                DeliveryFailure::Rejected {
                    status: crate::core::HttpStatus::new(http::StatusCode::TOO_MANY_REQUESTS),
                },
            ),
            (closed, DeliveryFailure::Unreachable),
            (unconfigured, DeliveryFailure::Unreachable),
        ] {
            let push = Push {
                entry: None,
                notice: Notice::Test,
            };
            assert_eq!(
                send(&channel, &topic, &push).await,
                NotificationEvent::DeliveryFailed { reason }
            );
        }
        Ok(())
    }
}
