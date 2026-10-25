//! Plain-language descriptions of events and actions for the history log.

use std::time::SystemTime;

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::core::{
    Action, ByteSize, ClientStanding, DeliveryFailure, Event, ExitIpFailure, Findings,
    HistoryEvent, Hold, IpFinding, MamEvent, MamFailure, MamPauseEvent, MamReply, MamRequest,
    MamRequestMode, MappingFailure, NotificationEvent, PersonalFreeleech, PortFinding,
    PortForwardingEvent, Purchase, QbittorrentEvent, QbittorrentFailure, Registration,
    RegistryEvent, SessionRejection, SettingsEvent, StartupEvent, StartupMode, TimerName,
    TorrentCommand, TunnelEvent,
};

/// History categories, used for filtering.
pub(super) const CATEGORIES: [(&str, &str); 8] = [
    ("startup", "Startup"),
    ("mam", "MAM requests"),
    ("settings", "Settings"),
    ("history", "History"),
    ("vpn", "VPN"),
    ("qbittorrent", "qBittorrent"),
    ("torrents", "Torrent registry"),
    ("notifications", "Notifications"),
];

pub(super) const fn category(event: &Event) -> &'static str {
    match event {
        Event::Startup(_) => "startup",
        Event::MamPause(_)
        | Event::Mam(_)
        | Event::TimerExpired {
            name: TimerName::Mam,
            ..
        } => "mam",
        Event::Settings(_) => "settings",
        Event::History(_) => "history",
        Event::PortForwarding(_)
        | Event::Tunnel(_)
        | Event::TimerExpired {
            name: TimerName::PortLease | TimerName::TunnelCheck,
            ..
        } => "vpn",
        Event::Qbittorrent(_)
        | Event::TimerExpired {
            name: TimerName::QbittorrentPoll,
            ..
        } => "qbittorrent",
        Event::Registry(_) => "torrents",
        Event::Notification(_)
        | Event::TimerExpired {
            name: TimerName::NotificationGrace | TimerName::NotificationRetry,
            ..
        } => "notifications",
    }
}

/// Whether an entry is periodic work that succeeded, hidden from history by default.
pub(super) const fn is_routine(event: &Event) -> bool {
    match event {
        Event::TimerExpired { .. }
        | Event::PortForwarding(PortForwardingEvent::Mapped { .. })
        | Event::Qbittorrent(QbittorrentEvent::PreferencesRead(_))
        | Event::Registry(RegistryEvent::Saved)
        | Event::Notification(NotificationEvent::Saved)
        | Event::Tunnel(TunnelEvent::ExitIpObserved { .. })
        | Event::Mam(
            MamEvent::ReminderSaved
            | MamEvent::Answered {
                result: Ok(MamReply::Ip(_) | MamReply::UserData(_) | MamReply::Bookmarks(_)),
                ..
            },
        ) => true,
        Event::Qbittorrent(QbittorrentEvent::Synced(update)) => {
            !update.full && update.torrents.is_empty() && update.removed.is_empty()
        }
        _ => false,
    }
}

const fn timer(name: TimerName) -> &'static str {
    match name {
        TimerName::QbittorrentPoll => "qBittorrent poll",
        TimerName::PortLease => "forwarded-port lease",
        TimerName::TunnelCheck => "tunnel check",
        TimerName::NotificationGrace => "notification grace period",
        TimerName::NotificationRetry => "notification retry",
        TimerName::Mam => "MAM work",
    }
}

/// A size in GiB with one decimal.
pub(super) fn gib(size: ByteSize) -> String {
    let tenths = u128::from(size.0) * 10 / (1 << 30);
    format!("{}.{} GiB", tenths / 10, tenths % 10)
}

pub(super) const fn rejection(rejection: SessionRejection) -> &'static str {
    match rejection {
        SessionRejection::InvalidCookie => "the API cookie is missing, invalid, or expired",
        SessionRejection::IpMismatch => "the session is locked to another IP",
        SessionRejection::AsnMismatch => "the session is locked to another network (ASN)",
        SessionRejection::NotAllowed => "the session may not set the dynamic seedbox IP",
        SessionRejection::Other => "MAM refused the session",
    }
}

pub(super) fn mam_failure(failure: &MamFailure) -> String {
    match failure {
        MamFailure::Session(reason) => format!("MAM refused the session: {}", rejection(*reason)),
        MamFailure::TooSoon {
            retry_after: Some(after),
        } => format!("MAM asked Windlass to wait {} seconds", after.as_secs()),
        MamFailure::TooSoon { retry_after: None } => String::from("MAM asked Windlass to wait"),
        MamFailure::Refused {
            message: Some(message),
        } => format!("MAM refused: {message}"),
        MamFailure::Refused { message: None } => {
            String::from("MAM refused without a torrent file or reason")
        }
        MamFailure::NotKept => String::from("the torrent file could not be kept in storage"),
        MamFailure::Unreachable => String::from("MAM did not answer"),
        MamFailure::Rejected { status } => format!("MAM answered with HTTP status {status}"),
        MamFailure::Malformed => String::from("MAM's answer was not understood"),
    }
}

/// What Windlass checked and found when MAM could not reach qBittorrent.
pub(super) fn findings(findings: &Findings) -> String {
    let port = match findings.listen_port {
        PortFinding::Matches(port) => {
            format!("qBittorrent listens on the forwarded port {}", port.get())
        }
        PortFinding::Differs {
            listening: Some(listening),
            forwarded,
        } => format!(
            "qBittorrent listened on {}, not the forwarded port {}; Windlass is setting it",
            listening.get(),
            forwarded.get()
        ),
        PortFinding::Differs {
            listening: None,
            forwarded,
        } => format!(
            "qBittorrent's listen port is unknown; the forwarded port is {}",
            forwarded.get()
        ),
        PortFinding::NoForwardedPort => String::from("the VPN forwards no port"),
    };
    let lease = if findings.lease_renewed {
        "a new forwarded-port lease was asked for"
    } else {
        "no lease could be renewed"
    };
    let ip = match findings.exit_ip {
        IpFinding::Matches(ip) => format!("MAM sees {ip}, the tunnel's exit IP"),
        IpFinding::Differs { mam, tunnel } => {
            format!("MAM sees {mam}, but the tunnel's exit IP is {tunnel}")
        }
        IpFinding::Unknown => String::from("the IP MAM sees, or the tunnel's, is unknown"),
    };
    let registration = if findings.registered {
        "the seedbox registration was confirmed and is renewed when allowed"
    } else {
        "the seedbox registration was not confirmed; Windlass registers when allowed"
    };
    let interface = findings.interface.as_ref().map_or_else(
        || String::from("qBittorrent is not bound to an interface"),
        |interface| format!("qBittorrent is bound to {interface}"),
    );
    format!("{port}; {lease}; {ip}; {registration}; {interface}")
}

/// What MAM's client list says about the running qBittorrent.
pub(super) const fn client_standing(standing: ClientStanding) -> &'static str {
    match standing {
        ClientStanding::Unknown => "not read yet",
        ClientStanding::NoneListed => "MAM lists no client yet",
        ClientStanding::Listed => "MAM lists the running version",
        ClientStanding::NotListed => "MAM lists qBittorrent, but not the running version",
        ClientStanding::Refused => "MAM marks the running version as not allowed",
        ClientStanding::NotUnderstood => {
            "MAM lists clients in a form Windlass cannot match to qBittorrent"
        }
    }
}

/// Why the next bookmarked torrent waits.
pub(super) fn hold(hold: Hold) -> String {
    match hold {
        Hold::MamUnavailable => {
            String::from("MAM requests are paused, the VPN is down, or MAM refused the session")
        }
        Hold::SeedboxUnconfirmed => {
            String::from("MAM has not confirmed the seedbox for the current IP")
        }
        Hold::DownloadsRefused => String::from(
            "MAM refused a download; downloads stay held until Windlass restarts or the cookie changes",
        ),
        Hold::ClientUnavailable => {
            String::from("qBittorrent is not connected, or MAM does not allow its version")
        }
        Hold::MamClientList(standing) => format!(
            "MAM's client list does not show the running qBittorrent as allowed: {}",
            client_standing(standing)
        ),
        Hold::DownloadingLimit { downloading, limit } => {
            format!("{downloading} downloads are running, and the limit is {limit}")
        }
        Hold::AccountUnknown => String::from("the account has not been read from MAM yet"),
        Hold::RatioLow => String::from("the ratio is below 2.0"),
        Hold::UnsatisfiedLimit { count, limit } => format!(
            "{count} unsatisfied torrents of MAM's limit of {limit} leave no slot after the reserve of 2"
        ),
        Hold::NoWedge => String::from("the torrent needs a wedge and none is left"),
        Hold::NotBookmarked => String::from("the queued torrents are no longer bookmarked"),
    }
}

pub(super) fn purchase(purchase: Purchase) -> String {
    match purchase {
        Purchase::Upload(amount) => format!("{amount} GiB of upload credit"),
        Purchase::Vip => String::from("VIP, filled to 90 days"),
    }
}

fn mam_request(request: &MamRequest) -> String {
    match request {
        MamRequest::LookUpIp => String::from("Ask MAM which IP it sees"),
        MamRequest::RegisterSeedbox => String::from("Register the seedbox IP with MAM"),
        MamRequest::LoadUserData { client_stats } => format!(
            "Read the account from MAM{}",
            if *client_stats {
                ", with the client list"
            } else {
                ""
            }
        ),
        MamRequest::ListBookmarks { start } => format!("List bookmarks from number {start}"),
        MamRequest::FetchTorrent { id, wedge } => format!(
            "Download the torrent file of MAM torrent {}, {}",
            id.get(),
            if *wedge {
                "spending a wedge"
            } else {
                "without a wedge"
            }
        ),
        MamRequest::Buy(bought) => format!("Buy {}", purchase(*bought)),
        MamRequest::WedgeHistory => String::from("Read the wedges spent, from MAM's bonus history"),
        MamRequest::LookUpTorrent { id } => {
            format!("Look up MAM torrent {} as it is now", id.get())
        }
    }
}

fn optional_time(time: Option<SystemTime>) -> String {
    time.map_or_else(|| String::from("none"), timestamp)
}

fn mam(event: &MamEvent) -> String {
    match event {
        MamEvent::Enable => String::from("MAM work started"),
        MamEvent::Answered { result, rotated } => {
            let answer = match result {
                Ok(MamReply::Ip(observed)) => format!(
                    "MAM sees IP {} in {} ({})",
                    observed.ip, observed.asn, observed.provider
                ),
                Ok(MamReply::Registered {
                    registration: Registration::Completed,
                    ip,
                }) => format!("MAM registered {ip} as the seedbox IP"),
                Ok(MamReply::Registered {
                    registration: Registration::NoChange,
                    ip,
                }) => format!("MAM already had {ip} as the seedbox IP"),
                Ok(MamReply::UserData(data)) => format!(
                    "Account read: {} points, {} wedges, {} uploaded beyond downloads{}",
                    data.points,
                    data.wedges,
                    gib(data.buffer()),
                    data.summary
                        .as_ref()
                        .map(|summary| format!(
                            ", {} of {} unsatisfied",
                            summary.unsatisfied, summary.unsatisfied_limit
                        ))
                        .unwrap_or_default()
                ),
                Ok(MamReply::Bookmarks(page)) => format!(
                    "Bookmarks listed: {} on this page of {}",
                    page.bookmarks.len(),
                    page.found
                ),
                Ok(MamReply::Fetched(fetched)) => format!(
                    "Torrent file fetched and kept: {}, {}",
                    fetched.hash.as_str(),
                    gib(fetched.size)
                ),
                Ok(MamReply::Purchased { points }) => {
                    format!("Purchase made; {points} points left")
                }
                Ok(MamReply::Torrent(Some(listing))) => format!(
                    "MAM torrent {} looked up: {}{}{}{}",
                    listing.id.get(),
                    if listing.free { "freeleech, " } else { "" },
                    if listing.vip { "VIP, " } else { "" },
                    match listing.personal {
                        PersonalFreeleech::No => "not free for you",
                        PersonalFreeleech::ThroughVip => "free for you through VIP",
                        PersonalFreeleech::ThroughWedge => "free for you through a wedge",
                    },
                    if listing.snatched {
                        ", snatched before"
                    } else {
                        ""
                    }
                ),
                Ok(MamReply::Torrent(None)) => String::from("MAM no longer lists the torrent"),
                Ok(MamReply::WedgeHistory(spent)) => {
                    format!("Bonus history read: {} wedges spent", spent.len())
                }
                Err(failure) => format!("MAM request failed: {}", mam_failure(failure)),
            };
            if rotated.is_some() {
                format!("{answer} (MAM issued a new cookie, not recorded)")
            } else {
                answer
            }
        }
        MamEvent::StateRead {
            last_registration,
            last_reminder,
        } => format!(
            "MAM state read: last seedbox registration {}, last visit reminder {}",
            optional_time(*last_registration),
            optional_time(*last_reminder)
        ),
        MamEvent::StateReadFailed => {
            String::from("MAM state could not be read; seedbox registration waits")
        }
        MamEvent::RegistrationAttemptSaved => String::from("Seedbox registration attempt recorded"),
        MamEvent::RegistrationAttemptSaveFailed => {
            String::from("Seedbox registration attempt could not be recorded, so it was not sent")
        }
        MamEvent::PurchaseChecked { made: true } => {
            String::from("You checked on MAM: the uncertain purchase went through")
        }
        MamEvent::PurchaseChecked { made: false } => {
            String::from("You checked on MAM: the uncertain purchase did not go through")
        }
        MamEvent::ReminderSaved => String::from("MAM visit reminder time saved"),
        MamEvent::ReminderSaveFailed => String::from("MAM visit reminder time could not be saved"),
    }
}

fn mapping_failure(reason: MappingFailure) -> String {
    match reason {
        MappingFailure::NoResponse => String::from("the gateway did not answer"),
        MappingFailure::Refused { result_code } => {
            format!("the gateway refused with NAT-PMP result {result_code}")
        }
        MappingFailure::Malformed => String::from("the gateway's answer was not valid"),
    }
}

pub(super) fn qbittorrent_failure(reason: QbittorrentFailure) -> String {
    match reason {
        QbittorrentFailure::Unreachable => String::from("qBittorrent did not answer"),
        QbittorrentFailure::Rejected { status } => {
            format!("qBittorrent answered with HTTP status {status}")
        }
        QbittorrentFailure::Malformed => String::from("qBittorrent's answer was not understood"),
    }
}

const fn startup_mode(mode: StartupMode) -> &'static str {
    match mode {
        StartupMode::Initialise => "initialise new storage",
        StartupMode::Resume => "resume existing storage",
    }
}

const fn request_mode(mode: MamRequestMode) -> &'static str {
    match mode {
        MamRequestMode::Paused => "paused",
        MamRequestMode::Enabled => "running",
    }
}

/// Format wall time as UTC RFC 3339; the page converts it to local time.
///
/// Times before 1970 or after 9999 cannot be formatted and are left empty.
pub(super) fn timestamp(time: SystemTime) -> String {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| time::Duration::try_from(since).ok())
        .and_then(|since| OffsetDateTime::UNIX_EPOCH.checked_add(since))
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_default()
}

pub(super) fn event(event: &Event) -> String {
    match event {
        Event::Startup(event) => match event {
            StartupEvent::Start { mode } => format!("Startup requested ({})", startup_mode(*mode)),
            StartupEvent::StorageOpened => String::from("Storage opened"),
            StartupEvent::StorageOpenFailed => String::from("Storage could not be opened"),
        },
        Event::MamPause(event) => mam_pause(*event),
        Event::Settings(event) => settings(event),
        Event::History(event) => history(event),
        Event::TimerExpired { name, .. } => format!("Timer expired: {}", timer(*name)),
        Event::PortForwarding(event) => port_forwarding(*event),
        Event::Qbittorrent(event) => qbittorrent(event),
        Event::Registry(event) => registry(event),
        Event::Tunnel(event) => tunnel(event),
        Event::Notification(event) => notification(event),
        Event::Mam(event) => mam(event),
    }
}

fn notification(event: &NotificationEvent) -> String {
    match event {
        NotificationEvent::SendTest => String::from("Test notification requested"),
        NotificationEvent::Delivered => String::from("Notification delivered"),
        NotificationEvent::DeliveryFailed { reason } => {
            format!("Notification not delivered: {}", delivery_failure(*reason))
        }
        NotificationEvent::LogRead { last: Some(last) } => {
            format!("Notifications log read; newest notice {}", last.get())
        }
        NotificationEvent::LogRead { last: None } => String::from("Notifications log read; empty"),
        NotificationEvent::LogReadFailed => String::from("Notifications log could not be read"),
        NotificationEvent::Saved => String::from("Notice saved"),
        NotificationEvent::SaveFailed => String::from("Notice could not be saved"),
    }
}

pub(super) fn delivery_failure(reason: DeliveryFailure) -> String {
    match reason {
        DeliveryFailure::Unreachable => String::from("ntfy did not answer"),
        DeliveryFailure::Rejected { status } => {
            format!("ntfy answered with HTTP status {status}")
        }
    }
}

pub(super) fn exit_ip_failure(reason: ExitIpFailure) -> String {
    match reason {
        ExitIpFailure::Unreachable => String::from("the exit-IP service did not answer"),
        ExitIpFailure::Rejected { status } => {
            format!("the exit-IP service answered with HTTP status {status}")
        }
        ExitIpFailure::Malformed => String::from("the exit-IP service's answer was not an address"),
    }
}

fn tunnel(event: &TunnelEvent) -> String {
    match event {
        TunnelEvent::Enable { servers, current } => format!(
            "Tunnel checks started with {} servers; connected to {}",
            servers.len(),
            current.as_ref().map_or("an unknown server", AsRef::as_ref)
        ),
        TunnelEvent::ExitIpObserved { ip } => format!("Tunnel works; exit IP {ip}"),
        TunnelEvent::ExitIpCheckFailed { reason } => {
            format!("Tunnel check failed: {}", exit_ip_failure(*reason))
        }
        TunnelEvent::LastSwitchRead { last_switch: None } => {
            String::from("No earlier VPN server switch is recorded")
        }
        TunnelEvent::LastSwitchRead {
            last_switch: Some(switch),
        } => format!(
            "Last VPN server switch read: to {} at {}",
            switch.server,
            timestamp(switch.at)
        ),
        TunnelEvent::LastSwitchReadFailed => {
            String::from("The last VPN server switch could not be read; switches are blocked")
        }
        TunnelEvent::SwitchSaved => String::from("VPN server switch recorded"),
        TunnelEvent::SwitchSaveFailed => {
            String::from("VPN server switch could not be recorded, so it did not start")
        }
        TunnelEvent::Switched => String::from("VPN server switch completed"),
        TunnelEvent::SwitchFailed => String::from("VPN server switch failed"),
    }
}

fn registry(event: &RegistryEvent) -> String {
    match event {
        RegistryEvent::Redownload(id) => {
            format!(
                "Download torrent row {} again, as asked on the dashboard",
                id.get()
            )
        }
        RegistryEvent::Read(records) => format!("Torrent registry read: {} rows", records.len()),
        RegistryEvent::ReadFailed => String::from("Torrent registry could not be read"),
        RegistryEvent::Saved => String::from("Torrent registry saved"),
        RegistryEvent::SaveFailed => String::from("Torrent registry could not be saved"),
    }
}

fn torrent_command(command: &TorrentCommand) -> String {
    match command {
        TorrentCommand::Start(hashes) => format!("Start {} stopped torrents", hashes.len()),
        TorrentCommand::SelectAllFiles(hash) => {
            format!("Select every file of torrent {}", hash.as_str())
        }
        TorrentCommand::Remove(hashes) => {
            format!("Remove {} torrents and their files", hashes.len())
        }
        TorrentCommand::Stop(hashes) => format!("Stop {} held torrents", hashes.len()),
        TorrentCommand::ForceStart(hashes) => format!(
            "Force-start {} torrents still short of MAM's 72 hours of seeding",
            hashes.len()
        ),
        TorrentCommand::EndForceStart(hashes) => format!(
            "Return {} torrents that have seeded 72 hours to qBittorrent's queue",
            hashes.len()
        ),
        TorrentCommand::Add { mam, hash } => format!(
            "Add MAM torrent {} ({}) from its kept torrent file",
            mam.get(),
            hash.as_str()
        ),
    }
}

fn port_forwarding(event: PortForwardingEvent) -> String {
    match event {
        PortForwardingEvent::Enable => String::from("Port forwarding started"),
        PortForwardingEvent::Mapped { tcp, udp, lifetime } => format!(
            "Forwarded ports granted: TCP {}, UDP {}, for {} seconds",
            tcp.get(),
            udp.get(),
            lifetime.as_secs()
        ),
        PortForwardingEvent::MappingFailed { reason } => {
            format!("Port forwarding failed: {}", mapping_failure(reason))
        }
    }
}

fn qbittorrent(event: &QbittorrentEvent) -> String {
    match event {
        QbittorrentEvent::Enable => String::from("Connecting to qBittorrent"),
        QbittorrentEvent::Connected { version, .. } => {
            format!("Connected to qBittorrent {version}")
        }
        QbittorrentEvent::ConnectFailed { reason } => {
            format!("Could not connect: {}", qbittorrent_failure(*reason))
        }
        QbittorrentEvent::Synced(update) => format!(
            "qBittorrent update: {} torrents changed, {} removed{}",
            update.torrents.len(),
            update.removed.len(),
            if update.full { " (full list)" } else { "" }
        ),
        QbittorrentEvent::SyncFailed { reason } => {
            format!(
                "Reading qBittorrent failed: {}",
                qbittorrent_failure(*reason)
            )
        }
        QbittorrentEvent::PreferencesRead(preferences) => format!(
            "qBittorrent preferences read: listen port {}, interface {}",
            preferences
                .listen_port
                .map_or_else(|| String::from("none"), |port| port.get().to_string()),
            preferences.interface.as_deref().unwrap_or("any")
        ),
        QbittorrentEvent::PreferencesReadFailed { reason } => format!(
            "Reading qBittorrent preferences failed: {}",
            qbittorrent_failure(*reason)
        ),
        QbittorrentEvent::ListenPortSet => String::from("qBittorrent accepted the new listen port"),
        QbittorrentEvent::QueueSet => String::from("qBittorrent accepted the new queue settings"),
        QbittorrentEvent::QueueSetFailed { reason } => format!(
            "Setting qBittorrent's queue failed: {}",
            qbittorrent_failure(*reason)
        ),
        QbittorrentEvent::ListenPortSetFailed { reason } => format!(
            "Setting qBittorrent's listen port failed: {}",
            qbittorrent_failure(*reason)
        ),
        QbittorrentEvent::CommandDone => String::from("qBittorrent accepted the torrent change"),
        QbittorrentEvent::CommandFailed { reason } => format!(
            "Changing qBittorrent's torrents failed: {}",
            qbittorrent_failure(*reason)
        ),
    }
}

fn mam_pause(event: MamPauseEvent) -> String {
    String::from(match event {
        MamPauseEvent::Set {
            mode: MamRequestMode::Paused,
        } => "Pause of MAM requests requested",
        MamPauseEvent::Set {
            mode: MamRequestMode::Enabled,
        } => "Resume of MAM requests requested",
        MamPauseEvent::Delete => "Removal of the saved MAM request choice requested",
        MamPauseEvent::Read { mode: None } => {
            "MAM request choice read: none saved, so requests are paused"
        }
        MamPauseEvent::Read {
            mode: Some(MamRequestMode::Paused),
        } => "MAM request choice read: paused",
        MamPauseEvent::Read {
            mode: Some(MamRequestMode::Enabled),
        } => "MAM request choice read: running",
        MamPauseEvent::ReadFailed => "MAM request choice could not be read",
        MamPauseEvent::Written => "MAM request choice saved",
        MamPauseEvent::WriteFailed => "MAM request choice could not be saved",
        MamPauseEvent::Deleted => "Saved MAM request choice removed",
        MamPauseEvent::DeleteFailed => "Saved MAM request choice could not be removed",
    })
}

fn settings(event: &SettingsEvent) -> String {
    match event {
        SettingsEvent::Read {
            history_limit,
            api_cookie,
            ntfy_topic,
            points_reserve,
            torrent_limits,
        } => {
            let configured = |set: bool| if set { "configured" } else { "not configured" };
            let limits = torrent_limits.unwrap_or_default();
            format!(
                "Settings read: history limit {}, API cookie {}, notification topic {}, \
                 points reserve {}, at most {} downloading and {} active torrents",
                history_limit.unwrap_or_default(),
                configured(api_cookie.is_some()),
                configured(ntfy_topic.is_some()),
                points_reserve.unwrap_or_default(),
                limits.downloading(),
                limits.active()
            )
        }
        SettingsEvent::ReadFailed => String::from("Settings could not be read"),
        SettingsEvent::SetHistoryLimit { limit } => {
            format!("History limit change to {limit} entries requested")
        }
        SettingsEvent::HistoryLimitWritten => String::from("History limit saved"),
        SettingsEvent::HistoryLimitWriteFailed => String::from("History limit could not be saved"),
        SettingsEvent::SetApiCookie { .. } => {
            String::from("API cookie replaced from the dashboard (value not recorded)")
        }
        SettingsEvent::ClearApiCookie => String::from("API cookie removal requested"),
        SettingsEvent::ApiCookieWritten => String::from("API cookie change saved"),
        SettingsEvent::ApiCookieWriteFailed => String::from("API cookie change could not be saved"),
        SettingsEvent::SetNtfyTopic { .. } => {
            String::from("Notification topic replaced from the dashboard (value not recorded)")
        }
        SettingsEvent::ClearNtfyTopic => String::from("Notification topic removal requested"),
        SettingsEvent::NtfyTopicWritten => String::from("Notification topic change saved"),
        SettingsEvent::NtfyTopicWriteFailed => {
            String::from("Notification topic change could not be saved")
        }
        SettingsEvent::SetPointsReserve { reserve } => {
            format!("Points reserve change to {reserve} requested")
        }
        SettingsEvent::PointsReserveWritten => String::from("Points reserve saved"),
        SettingsEvent::PointsReserveWriteFailed => {
            String::from("Points reserve could not be saved")
        }
        SettingsEvent::SetTorrentLimits { limits } => format!(
            "Torrent limits change to {} downloading and {} active requested",
            limits.downloading(),
            limits.active()
        ),
        SettingsEvent::TorrentLimitsWritten => String::from("Torrent limits saved"),
        SettingsEvent::TorrentLimitsWriteFailed => {
            String::from("Torrent limits could not be saved")
        }
        SettingsEvent::RotatedApiCookieWritten => {
            String::from("The new API cookie MAM issued was saved")
        }
        SettingsEvent::RotatedApiCookieWriteFailed => String::from(
            "The new API cookie MAM issued could not be saved; the saved one stays in use",
        ),
    }
}

fn history(event: &HistoryEvent) -> String {
    match event {
        HistoryEvent::Opened { retained } => {
            format!("History opened with {} entries", retained.get())
        }
        HistoryEvent::OpenFailed => String::from("History could not be opened"),
        HistoryEvent::Trimmed { retained } => {
            format!("Oldest history removed; {} entries kept", retained.get())
        }
        HistoryEvent::TrimFailed => String::from("Oldest history could not be removed"),
        HistoryEvent::EntriesLost { count, from, until } => format!(
            "History could not be saved: {} entries are missing, processed from {} to {}",
            count.get(),
            timestamp(*from),
            timestamp(*until)
        ),
    }
}

pub(super) fn action(action: &Action) -> String {
    match action {
        Action::OpenStorage { mode } => format!("Open storage ({})", startup_mode(*mode)),
        Action::ReadMamRequestMode => String::from("Read the saved MAM request choice"),
        Action::WriteMamRequestMode { mode } => {
            format!("Save MAM request choice: {}", request_mode(*mode))
        }
        Action::DeleteMamRequestMode => String::from("Remove the saved MAM request choice"),
        Action::ReadSettings => String::from("Read saved settings"),
        Action::WriteHistoryLimit { limit } => format!("Save history limit: {limit} entries"),
        Action::WriteApiCookie { cookie: Some(_) } => {
            String::from("Save API cookie (value not recorded)")
        }
        Action::WriteApiCookie { cookie: None } => String::from("Remove the saved API cookie"),
        Action::OpenHistory => String::from("Open history storage"),
        Action::TrimHistory { keep } => {
            format!("Remove oldest history, keeping the newest {}", keep.get())
        }
        Action::StartTimer { name, deadline, .. } => format!(
            "Start timer: {} in {:.1} seconds",
            timer(*name),
            deadline.after.as_secs_f64()
        ),
        Action::RequestPortMapping { lifetime } => {
            format!("Request forwarded ports for {} seconds", lifetime.as_secs())
        }
        Action::ConnectQbittorrent => String::from("Connect to qBittorrent"),
        Action::SyncQbittorrent { position } => {
            format!("Read qBittorrent changes since update {}", position.0)
        }
        Action::ReadQbittorrentPreferences => String::from("Read qBittorrent preferences"),
        Action::SetQbittorrentListenPort { port } => {
            format!("Set qBittorrent's listen port to {}", port.get())
        }
        Action::QbittorrentCommand { command } => torrent_command(command),
        Action::ReadTorrentRegistry => String::from("Read the torrent registry"),
        Action::SaveTorrents { records } => {
            format!("Save {} torrent registry rows", records.len())
        }
        Action::CheckExitIp => String::from("Check the tunnel's exit IP"),
        Action::ReadLastVpnSwitch => String::from("Read the last VPN server switch"),
        Action::SaveVpnSwitch { switch } => {
            format!("Record a switch to VPN server {}", switch.server)
        }
        Action::SwitchVpnServer { server } => format!("Switch to VPN server {server}"),
        Action::WriteNtfyTopic { topic: Some(_) } => {
            String::from("Save notification topic (value not recorded)")
        }
        Action::WriteNtfyTopic { topic: None } => String::from("Remove the notification topic"),
        Action::SendNotification { push, .. } => {
            format!("Push: {}", super::notify::message(&push.notice).title)
        }
        Action::ReadNoticeLog => String::from("Read the notifications log"),
        Action::SaveNotice { entry, .. } => format!(
            "Save notice {}: {}",
            entry.id.get(),
            super::notify::message(&entry.notice).title
        ),
        Action::WritePointsReserve { reserve } => format!("Save points reserve: {reserve}"),
        Action::WriteTorrentLimits { limits } => format!(
            "Save torrent limits: {} downloading, {} active",
            limits.downloading(),
            limits.active()
        ),
        Action::SetQbittorrentQueue { queue } => {
            let limit = |limit: Option<u32>| {
                limit.map_or_else(|| String::from("no limit"), |limit| limit.to_string())
            };
            format!(
                "Set qBittorrent's queue: {} downloading, {} active, {} seeding, idle torrents {}",
                limit(queue.max_downloading),
                limit(queue.max_active),
                limit(queue.max_seeding),
                if queue.idle_not_counted {
                    "not counted"
                } else {
                    "counted"
                }
            )
        }
        Action::WriteRotatedApiCookie { .. } => {
            String::from("Save the new API cookie MAM issued (value not recorded)")
        }
        Action::ReadMamState => String::from("Read saved MAM state"),
        Action::SaveRegistrationAttempt { at } => {
            format!(
                "Record a seedbox registration attempt at {}",
                timestamp(*at)
            )
        }
        Action::SaveReminder { at } => {
            format!("Record the MAM visit reminder at {}", timestamp(*at))
        }
        Action::MamRequest { request, .. } => mam_request(request),
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use crate::core::{
        Action, ApiCookie, EntryCount, Event, HistoryEvent, HistoryLimit, MamPauseEvent,
        MamRequestMode, SettingsEvent, StartupEvent, StartupMode,
    };

    use super::{action, category, event};

    #[test]
    fn every_event_and_action_is_described_without_secrets() {
        let cookie = ApiCookie::parse("secret-value").ok();
        let limit = HistoryLimit::default();
        let events = [
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Resume,
            }),
            Event::Startup(StartupEvent::StorageOpened),
            Event::Startup(StartupEvent::StorageOpenFailed),
            Event::MamPause(MamPauseEvent::Set {
                mode: MamRequestMode::Enabled,
            }),
            Event::MamPause(MamPauseEvent::Delete),
            Event::MamPause(MamPauseEvent::Read {
                mode: Some(MamRequestMode::Paused),
            }),
            Event::MamPause(MamPauseEvent::Read {
                mode: Some(MamRequestMode::Enabled),
            }),
            Event::MamPause(MamPauseEvent::ReadFailed),
            Event::MamPause(MamPauseEvent::WriteFailed),
            Event::MamPause(MamPauseEvent::Deleted),
            Event::MamPause(MamPauseEvent::DeleteFailed),
            Event::Settings(SettingsEvent::Read {
                history_limit: Some(limit),
                api_cookie: cookie.clone(),
                ntfy_topic: None,
                points_reserve: None,
                torrent_limits: None,
            }),
            Event::Settings(SettingsEvent::ReadFailed),
            Event::Settings(SettingsEvent::HistoryLimitWritten),
            Event::Settings(SettingsEvent::HistoryLimitWriteFailed),
            Event::Settings(SettingsEvent::ClearApiCookie),
            Event::Settings(SettingsEvent::ApiCookieWriteFailed),
            Event::History(HistoryEvent::OpenFailed),
            Event::History(HistoryEvent::TrimFailed),
            Event::History(HistoryEvent::EntriesLost {
                count: EntryCount::new(2),
                from: SystemTime::UNIX_EPOCH,
                until: SystemTime::UNIX_EPOCH,
            }),
        ];
        let mut descriptions: Vec<String> = events.iter().map(event).collect();
        assert!(descriptions.contains(&String::from("Storage could not be opened")));
        assert!(
            descriptions
                .iter()
                .any(|text| text.contains("1970-01-01T00:00:00Z"))
        );
        assert_eq!(category(&events[0]), "startup");
        let actions = [
            Action::ReadMamRequestMode,
            Action::WriteMamRequestMode {
                mode: MamRequestMode::Paused,
            },
            Action::WriteMamRequestMode {
                mode: MamRequestMode::Enabled,
            },
            Action::DeleteMamRequestMode,
            Action::ReadSettings,
            Action::WriteHistoryLimit { limit },
            Action::WriteApiCookie { cookie },
            Action::WriteApiCookie { cookie: None },
            Action::OpenHistory,
        ];
        descriptions.extend(actions.iter().map(action));
        for description in &descriptions {
            assert!(!description.is_empty());
            assert!(!description.contains("secret-value"), "{description}");
        }
        let far_future = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(u64::MAX / 4);
        assert_eq!(super::timestamp(far_future), "");
        let unique: std::collections::BTreeSet<_> = descriptions.iter().collect();
        assert_eq!(
            unique.len(),
            descriptions.len(),
            "descriptions are distinct"
        );
    }

    const PORT: crate::core::Port = match crate::core::Port::new(41000) {
        Some(port) => port,
        None => unreachable!(),
    };

    /// Schedule timers through the core, for their identities.
    fn timer_actions() -> Vec<Action> {
        use std::time::Instant;

        use crate::core::{Core, MappingFailure, PortForwardingEvent};

        let mut core = Core::new();
        let clock = crate::core::ClockReading {
            monotonic: Instant::now(),
            wall: SystemTime::UNIX_EPOCH,
        };
        let mut actions = core.handle(clock, Event::PortForwarding(PortForwardingEvent::Enable));
        actions.extend(core.handle(
            clock,
            Event::PortForwarding(PortForwardingEvent::MappingFailed {
                reason: MappingFailure::NoResponse,
            }),
        ));
        actions
    }

    fn check(entries: &[(Event, &str, bool)]) {
        for (entry, expected_category, routine) in entries {
            assert!(!event(entry).is_empty());
            assert_eq!(category(entry), *expected_category, "{entry:?}");
            assert_eq!(super::is_routine(entry), *routine, "{entry:?}");
        }
    }

    #[test]
    fn vpn_events_are_described_and_classified() -> Result<(), &'static str> {
        use std::time::Duration;

        use crate::core::{MappingFailure, PortForwardingEvent, TimerName};

        let id = timer_actions()
            .iter()
            .find_map(|action| match action {
                Action::StartTimer { id, .. } => Some(*id),
                _ => None,
            })
            .ok_or("no timer")?;
        let mut entries = vec![
            (
                Event::TimerExpired {
                    name: TimerName::PortLease,
                    id,
                },
                "vpn",
                true,
            ),
            (
                Event::TimerExpired {
                    name: TimerName::QbittorrentPoll,
                    id,
                },
                "qbittorrent",
                true,
            ),
            (
                Event::PortForwarding(PortForwardingEvent::Enable),
                "vpn",
                false,
            ),
            (
                Event::PortForwarding(PortForwardingEvent::Mapped {
                    tcp: PORT,
                    udp: PORT,
                    lifetime: Duration::from_mins(1),
                }),
                "vpn",
                true,
            ),
        ];
        for reason in [
            MappingFailure::NoResponse,
            MappingFailure::Refused { result_code: 2 },
            MappingFailure::Malformed,
        ] {
            entries.push((
                Event::PortForwarding(PortForwardingEvent::MappingFailed { reason }),
                "vpn",
                false,
            ));
        }
        check(&entries);
        Ok(())
    }

    #[test]
    fn qbittorrent_events_are_described_and_classified() {
        use crate::core::{
            QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences, SyncUpdate,
        };

        let qbittorrent = |event, routine| (Event::Qbittorrent(event), "qbittorrent", routine);
        let mut entries = vec![
            qbittorrent(QbittorrentEvent::Enable, false),
            qbittorrent(
                QbittorrentEvent::Connected {
                    version: String::from("v5.2.2"),
                    preferences: QbittorrentPreferences::default(),
                },
                false,
            ),
            qbittorrent(QbittorrentEvent::Synced(SyncUpdate::default()), true),
            qbittorrent(
                QbittorrentEvent::Synced(SyncUpdate {
                    full: true,
                    ..SyncUpdate::default()
                }),
                false,
            ),
            qbittorrent(
                QbittorrentEvent::PreferencesRead(QbittorrentPreferences {
                    listen_port: Some(PORT),
                    interface: Some(String::from("wg0")),
                    queue: None,
                }),
                true,
            ),
            qbittorrent(
                QbittorrentEvent::PreferencesRead(QbittorrentPreferences::default()),
                true,
            ),
            qbittorrent(QbittorrentEvent::ListenPortSet, false),
            qbittorrent(QbittorrentEvent::QueueSet, false),
        ];
        for reason in [
            QbittorrentFailure::Unreachable,
            QbittorrentFailure::Rejected { status: 403 },
            QbittorrentFailure::Malformed,
        ] {
            for failure in [
                QbittorrentEvent::ConnectFailed { reason },
                QbittorrentEvent::SyncFailed { reason },
                QbittorrentEvent::PreferencesReadFailed { reason },
                QbittorrentEvent::ListenPortSetFailed { reason },
                QbittorrentEvent::QueueSetFailed { reason },
                QbittorrentEvent::CommandFailed { reason },
            ] {
                entries.push(qbittorrent(failure, false));
            }
        }
        entries.push(qbittorrent(QbittorrentEvent::CommandDone, false));
        check(&entries);
    }

    #[test]
    fn every_action_is_described() -> Result<(), Box<dyn std::error::Error>> {
        use std::time::SystemTime;

        use crate::core::{
            MamRequestMode, StartupMode, SyncPosition, TorrentCommand, VpnServer, VpnSwitch,
        };

        let server = VpnServer::try_new("two")?;
        let mut actions = vec![
            Action::OpenStorage {
                mode: StartupMode::Initialise,
            },
            Action::ReadMamRequestMode,
            Action::WriteMamRequestMode {
                mode: MamRequestMode::Paused,
            },
            Action::DeleteMamRequestMode,
            Action::ReadSettings,
            Action::WriteHistoryLimit {
                limit: HistoryLimit::default(),
            },
            Action::WriteApiCookie { cookie: None },
            Action::OpenHistory,
            Action::TrimHistory {
                keep: EntryCount::new(1),
            },
            Action::RequestPortMapping {
                lifetime: std::time::Duration::from_mins(1),
            },
            Action::ConnectQbittorrent,
            Action::SyncQbittorrent {
                position: SyncPosition(1),
            },
            Action::ReadQbittorrentPreferences,
            Action::SetQbittorrentListenPort { port: PORT },
            Action::QbittorrentCommand {
                command: TorrentCommand::Start(Vec::new()),
            },
            Action::ReadTorrentRegistry,
            Action::SaveTorrents {
                records: Vec::new(),
            },
            Action::CheckExitIp,
            Action::ReadLastVpnSwitch,
            Action::SaveVpnSwitch {
                switch: VpnSwitch {
                    server: server.clone(),
                    at: SystemTime::UNIX_EPOCH,
                },
            },
            Action::SwitchVpnServer { server },
            Action::WriteNtfyTopic {
                topic: crate::core::NtfyTopic::parse("alerts").ok(),
            },
            Action::WriteNtfyTopic { topic: None },
        ];
        actions.extend(crate::core::NtfyTopic::parse("alerts").map(|topic| {
            Action::SendNotification {
                topic,
                push: crate::core::Push {
                    entry: None,
                    notice: crate::core::Notice::Test,
                },
            }
        }));
        actions.extend([
            Action::ReadNoticeLog,
            Action::SaveNotice {
                entry: crate::core::Logged {
                    id: crate::core::NoticeId::new(7),
                    at: SystemTime::UNIX_EPOCH,
                    notice: crate::core::Notice::Test,
                    reach: crate::core::Reach::Push,
                },
                forget_through: None,
            },
        ]);
        actions.extend(timer_actions());
        for described in &actions {
            assert!(!action(described).is_empty(), "{described:?}");
        }
        Ok(())
    }

    #[test]
    fn tunnel_events_and_actions_are_described() -> Result<(), Box<dyn std::error::Error>> {
        use std::time::SystemTime;

        use crate::core::{ExitIpFailure, TunnelEvent, VpnServer, VpnSwitch};

        let server = VpnServer::try_new("two")?;
        let switch = VpnSwitch {
            server: server.clone(),
            at: SystemTime::UNIX_EPOCH,
        };
        let vpn = |event, routine| (Event::Tunnel(event), "vpn", routine);
        let mut entries = vec![
            vpn(
                TunnelEvent::Enable {
                    servers: vec![server.clone()],
                    current: None,
                },
                false,
            ),
            vpn(
                TunnelEvent::Enable {
                    servers: vec![server.clone()],
                    current: Some(server.clone()),
                },
                false,
            ),
            vpn(
                TunnelEvent::ExitIpObserved {
                    ip: std::net::IpAddr::from([192, 0, 2, 7]),
                },
                true,
            ),
            vpn(TunnelEvent::LastSwitchRead { last_switch: None }, false),
            vpn(
                TunnelEvent::LastSwitchRead {
                    last_switch: Some(switch.clone()),
                },
                false,
            ),
            vpn(TunnelEvent::LastSwitchReadFailed, false),
            vpn(TunnelEvent::SwitchSaved, false),
            vpn(TunnelEvent::SwitchSaveFailed, false),
            vpn(TunnelEvent::Switched, false),
            vpn(TunnelEvent::SwitchFailed, false),
        ];
        for reason in [
            ExitIpFailure::Unreachable,
            ExitIpFailure::Rejected { status: 503 },
            ExitIpFailure::Malformed,
        ] {
            entries.push(vpn(TunnelEvent::ExitIpCheckFailed { reason }, false));
        }
        check(&entries);
        assert!(
            [
                Action::CheckExitIp,
                Action::ReadLastVpnSwitch,
                Action::SaveVpnSwitch { switch },
                Action::SwitchVpnServer { server },
            ]
            .iter()
            .map(action)
            .any(|description| description == "Switch to VPN server two")
        );
        let unique: std::collections::BTreeSet<_> =
            entries.iter().map(|entry| event(&entry.0)).collect();
        assert_eq!(unique.len(), entries.len(), "descriptions are distinct");
        Ok(())
    }

    #[test]
    fn notification_events_are_described_and_classified() {
        use crate::core::{DeliveryFailure, NotificationEvent, NtfyTopic, TimerId, TimerName};

        let entries: Vec<(Event, &str, bool)> = [
            NotificationEvent::SendTest,
            NotificationEvent::Delivered,
            NotificationEvent::DeliveryFailed {
                reason: DeliveryFailure::Unreachable,
            },
            NotificationEvent::DeliveryFailed {
                reason: DeliveryFailure::Rejected { status: 429 },
            },
            NotificationEvent::LogRead { last: None },
            NotificationEvent::LogRead {
                last: Some(crate::core::NoticeId::new(3)),
            },
            NotificationEvent::LogReadFailed,
            NotificationEvent::SaveFailed,
        ]
        .into_iter()
        .map(|event| (Event::Notification(event), "notifications", false))
        .chain([(
            Event::Notification(NotificationEvent::Saved),
            "notifications",
            true,
        )])
        .chain(
            [
                SettingsEvent::ClearNtfyTopic,
                SettingsEvent::NtfyTopicWritten,
                SettingsEvent::NtfyTopicWriteFailed,
            ]
            .into_iter()
            .chain(NtfyTopic::parse("alerts").map(|topic| SettingsEvent::SetNtfyTopic { topic }))
            .map(|event| (Event::Settings(event), "settings", false)),
        )
        .collect();
        check(&entries);
        let secret = Event::Settings(SettingsEvent::Read {
            history_limit: None,
            api_cookie: None,
            ntfy_topic: NtfyTopic::parse("alerts").ok(),
            points_reserve: None,
            torrent_limits: None,
        });
        assert!(event(&secret).contains("notification topic configured"));
        let id = timer_actions().into_iter().find_map(|action| match action {
            Action::StartTimer { id, .. } => Some(id),
            _ => None,
        });
        let timers: Vec<(Event, &str, bool)> = id
            .into_iter()
            .flat_map(|id: TimerId| {
                [TimerName::NotificationGrace, TimerName::NotificationRetry]
                    .map(|name| (Event::TimerExpired { name, id }, "notifications", true))
            })
            .collect();
        assert_eq!(timers.len(), 2);
        check(&timers);
    }

    #[test]
    fn registry_events_and_commands_are_described() -> Result<(), &'static str> {
        use crate::core::{RecordId, RegistryEvent, TorrentCommand, TorrentHash};

        let registry = |event, routine| (Event::Registry(event), "torrents", routine);
        let row = RecordId::new(3).ok_or("zero id")?;
        check(&[
            registry(RegistryEvent::Redownload(row), false),
            registry(RegistryEvent::Read(Vec::new()), false),
            registry(RegistryEvent::ReadFailed, false),
            registry(RegistryEvent::Saved, true),
            registry(RegistryEvent::SaveFailed, false),
        ]);
        let hash = TorrentHash::parse(&"a".repeat(40)).ok_or("hash")?;
        let descriptions: Vec<String> = [
            Action::ReadTorrentRegistry,
            Action::SaveTorrents {
                records: Vec::new(),
            },
            Action::QbittorrentCommand {
                command: TorrentCommand::Start(vec![hash.clone()]),
            },
            Action::QbittorrentCommand {
                command: TorrentCommand::Remove(vec![hash.clone()]),
            },
            Action::QbittorrentCommand {
                command: TorrentCommand::SelectAllFiles(hash),
            },
        ]
        .iter()
        .map(action)
        .collect();
        assert!(descriptions.contains(&String::from("Start 1 stopped torrents")));
        assert!(descriptions.contains(&String::from("Remove 1 torrents and their files")));
        Ok(())
    }

    #[test]
    fn integration_actions_are_described() {
        use std::time::Duration;

        use crate::core::SyncPosition;

        for entry in [
            Action::RequestPortMapping {
                lifetime: Duration::from_mins(1),
            },
            Action::ConnectQbittorrent,
            Action::SyncQbittorrent {
                position: SyncPosition(3),
            },
            Action::ReadQbittorrentPreferences,
            Action::SetQbittorrentListenPort { port: PORT },
        ]
        .iter()
        .chain(&timer_actions())
        {
            assert!(!action(entry).is_empty());
        }
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one list of every MAM event, failure, and action reads best together"
    )]
    fn mam_events_and_actions_are_described_and_classified()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{net::IpAddr, time::Duration};

        use crate::core::{
            Asn, BookmarkPage, ByteSize, ClientStanding, FetchedTorrent, Hold, IpObservation,
            MamEvent, MamFailure, MamReply, MamRequest, MamTorrentId, Points, PointsReserve,
            Purchase, Registration, SessionRejection, TimerName, TorrentCommand, TorrentHash,
            UploadCredit, UserData,
        };

        let id = timer_actions()
            .iter()
            .find_map(|action| match action {
                Action::StartTimer { id, .. } => Some(*id),
                _ => None,
            })
            .ok_or("no timer")?;
        let ip = IpAddr::from([192, 0, 2, 10]);
        let hash = TorrentHash::parse(&"c".repeat(40)).ok_or("hash")?;
        let torrent = MamTorrentId::new(9).ok_or("id")?;
        let data = UserData {
            uploaded: ByteSize(3 << 30),
            downloaded: ByteSize(1 << 30),
            points: Points(1),
            wedges: 2,
            class: String::from("VIP"),
            vip_until: None,
            summary: Some(crate::core::SnatchSummary::default()),
            notices: None,
            clients: None,
        };
        let mut without_summary = data.clone();
        without_summary.summary = None;
        let answered = |result| MamEvent::Answered {
            result,
            rotated: None,
        };
        let routine = |reply| (Event::Mam(answered(Ok(reply))), "mam", true);
        let mut entries = vec![
            (
                Event::TimerExpired {
                    name: TimerName::Mam,
                    id,
                },
                "mam",
                true,
            ),
            (Event::Mam(MamEvent::Enable), "mam", false),
            routine(MamReply::Ip(IpObservation {
                ip,
                asn: Asn(1),
                provider: String::from("Provider"),
            })),
            routine(MamReply::UserData(Box::new(data))),
            routine(MamReply::UserData(Box::new(without_summary))),
            routine(MamReply::Bookmarks(BookmarkPage {
                found: 0,
                bookmarks: Vec::new(),
            })),
            (Event::Mam(MamEvent::ReminderSaved), "mam", true),
        ];
        entries.extend(
            [
                answered(Ok(MamReply::Registered {
                    registration: Registration::Completed,
                    ip,
                })),
                answered(Ok(MamReply::Registered {
                    registration: Registration::NoChange,
                    ip,
                })),
                answered(Ok(MamReply::Fetched(FetchedTorrent {
                    hash: hash.clone(),
                    size: ByteSize(1 << 20),
                }))),
                answered(Ok(MamReply::Purchased { points: Points(2) })),
                answered(Ok(MamReply::WedgeHistory(Vec::new()))),
                answered(Ok(MamReply::Torrent(None))),
                answered(Ok(MamReply::Torrent(Some(Box::new(
                    crate::core::Bookmark {
                        id: torrent,
                        title: String::from("Synthetic"),
                        vip: true,
                        free: true,
                        personal: crate::core::PersonalFreeleech::ThroughWedge,
                        snatched: true,
                        vip_expires: None,
                        bookmarked_at: None,
                    },
                ))))),
                answered(Ok(MamReply::Torrent(Some(Box::new(
                    crate::core::Bookmark {
                        id: torrent,
                        title: String::from("Synthetic"),
                        vip: false,
                        free: false,
                        personal: crate::core::PersonalFreeleech::ThroughVip,
                        snatched: false,
                        vip_expires: None,
                        bookmarked_at: None,
                    },
                ))))),
                answered(Ok(MamReply::Torrent(Some(Box::new(
                    crate::core::Bookmark {
                        id: torrent,
                        title: String::from("Synthetic"),
                        vip: false,
                        free: false,
                        personal: crate::core::PersonalFreeleech::No,
                        snatched: false,
                        vip_expires: None,
                        bookmarked_at: None,
                    },
                ))))),
                MamEvent::PurchaseChecked { made: true },
                MamEvent::PurchaseChecked { made: false },
                MamEvent::Answered {
                    result: Err(MamFailure::Unreachable),
                    rotated: ApiCookie::parse("rotated").ok(),
                },
                MamEvent::StateRead {
                    last_registration: None,
                    last_reminder: Some(SystemTime::UNIX_EPOCH),
                },
                MamEvent::StateReadFailed,
                MamEvent::RegistrationAttemptSaved,
                MamEvent::RegistrationAttemptSaveFailed,
                MamEvent::ReminderSaveFailed,
            ]
            .map(|event| (Event::Mam(event), "mam", false)),
        );
        entries.extend(
            [
                MamFailure::TooSoon { retry_after: None },
                MamFailure::TooSoon {
                    retry_after: Some(Duration::from_mins(1)),
                },
                MamFailure::Refused { message: None },
                MamFailure::Refused {
                    message: Some(String::from("No")),
                },
                MamFailure::NotKept,
                MamFailure::Rejected { status: 500 },
                MamFailure::Malformed,
            ]
            .into_iter()
            .chain(
                [
                    SessionRejection::InvalidCookie,
                    SessionRejection::IpMismatch,
                    SessionRejection::AsnMismatch,
                    SessionRejection::NotAllowed,
                    SessionRejection::Other,
                ]
                .map(MamFailure::Session),
            )
            .map(|failure| (Event::Mam(answered(Err(failure))), "mam", false)),
        );
        entries.extend(
            [
                SettingsEvent::SetPointsReserve {
                    reserve: PointsReserve::default(),
                },
                SettingsEvent::PointsReserveWritten,
                SettingsEvent::PointsReserveWriteFailed,
                SettingsEvent::SetTorrentLimits {
                    limits: crate::core::TorrentLimits::default(),
                },
                SettingsEvent::TorrentLimitsWritten,
                SettingsEvent::TorrentLimitsWriteFailed,
                SettingsEvent::RotatedApiCookieWritten,
                SettingsEvent::RotatedApiCookieWriteFailed,
            ]
            .map(|event| (Event::Settings(event), "settings", false)),
        );
        check(&entries);
        let rotated = event(&Event::Mam(MamEvent::Answered {
            result: Err(MamFailure::Unreachable),
            rotated: ApiCookie::parse("rotated").ok(),
        }));
        assert!(rotated.contains("new cookie") && !rotated.contains("rotated"));

        let upload = Purchase::Upload(UploadCredit::try_new(50)?);
        for entry in [
            Action::WritePointsReserve {
                reserve: PointsReserve::default(),
            },
            Action::WriteRotatedApiCookie {
                cookie: ApiCookie::parse("secret-rotated")?,
            },
            Action::ReadMamState,
            Action::SaveRegistrationAttempt {
                at: SystemTime::UNIX_EPOCH,
            },
            Action::SaveReminder {
                at: SystemTime::UNIX_EPOCH,
            },
            Action::QbittorrentCommand {
                command: TorrentCommand::Stop(vec![hash.clone()]),
            },
            Action::QbittorrentCommand {
                command: TorrentCommand::ForceStart(vec![hash.clone()]),
            },
            Action::QbittorrentCommand {
                command: TorrentCommand::EndForceStart(vec![hash.clone()]),
            },
            Action::WriteTorrentLimits {
                limits: crate::core::TorrentLimits::default(),
            },
            Action::SetQbittorrentQueue {
                queue: crate::core::QueuePreferences::for_limits(
                    crate::core::TorrentLimits::default(),
                ),
            },
            Action::SetQbittorrentQueue {
                queue: crate::core::QueuePreferences {
                    queueing: false,
                    max_downloading: None,
                    max_active: None,
                    max_seeding: None,
                    idle_not_counted: false,
                },
            },
            Action::QbittorrentCommand {
                command: TorrentCommand::Add { mam: torrent, hash },
            },
        ]
        .into_iter()
        .chain(
            [
                MamRequest::LookUpIp,
                MamRequest::RegisterSeedbox,
                MamRequest::LoadUserData { client_stats: true },
                MamRequest::LoadUserData {
                    client_stats: false,
                },
                MamRequest::ListBookmarks { start: 100 },
                MamRequest::FetchTorrent {
                    id: torrent,
                    wedge: true,
                },
                MamRequest::FetchTorrent {
                    id: torrent,
                    wedge: false,
                },
                MamRequest::Buy(upload),
                MamRequest::Buy(Purchase::Vip),
                MamRequest::WedgeHistory,
                MamRequest::LookUpTorrent { id: torrent },
            ]
            .map(|request| Action::MamRequest {
                request,
                cookie: ApiCookie::parse("secret-cookie").ok(),
            }),
        ) {
            let described = action(&entry);
            assert!(
                !described.is_empty() && !described.contains("secret"),
                "{entry:?}"
            );
        }
        for hold in [
            Hold::MamUnavailable,
            Hold::SeedboxUnconfirmed,
            Hold::DownloadsRefused,
            Hold::ClientUnavailable,
            Hold::AccountUnknown,
            Hold::RatioLow,
            Hold::UnsatisfiedLimit { count: 1, limit: 2 },
            Hold::DownloadingLimit {
                downloading: 5,
                limit: crate::core::DownloadingLimit::default(),
            },
            Hold::NoWedge,
            Hold::NotBookmarked,
        ]
        .into_iter()
        .chain(
            [
                ClientStanding::Unknown,
                ClientStanding::NoneListed,
                ClientStanding::Listed,
                ClientStanding::NotListed,
                ClientStanding::Refused,
                ClientStanding::NotUnderstood,
            ]
            .map(Hold::MamClientList),
        ) {
            assert!(!super::hold(hold).is_empty());
        }
        assert_eq!(super::gib(ByteSize(3 << 29)), "1.5 GiB");

        let port = crate::core::Port::new(41000).ok_or("zero port")?;
        let other = crate::core::Port::new(6881).ok_or("zero port")?;
        let mam = IpAddr::from([192, 0, 2, 10]);
        let tunnel = IpAddr::from([192, 0, 2, 11]);
        for (listen_port, exit_ip, text) in [
            (
                crate::core::PortFinding::Matches(port),
                crate::core::IpFinding::Matches(mam),
                "listens on the forwarded port 41000",
            ),
            (
                crate::core::PortFinding::Differs {
                    listening: Some(other),
                    forwarded: port,
                },
                crate::core::IpFinding::Differs { mam, tunnel },
                "listened on 6881, not the forwarded port 41000",
            ),
            (
                crate::core::PortFinding::Differs {
                    listening: None,
                    forwarded: port,
                },
                crate::core::IpFinding::Unknown,
                "listen port is unknown",
            ),
        ] {
            let found = crate::core::Findings {
                listen_port,
                lease_renewed: true,
                exit_ip,
                registered: true,
                interface: Some(String::from("wg0")),
            };
            let described = super::findings(&found);
            assert!(described.contains(text), "{described}");
            assert!(described.contains("bound to wg0"), "{described}");
        }
        Ok(())
    }
}
