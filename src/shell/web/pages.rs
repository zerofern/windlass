//! Server-rendered pages. Templates translate core state into words; the core
//! decides what that state is.

use std::time::SystemTime;

use maud::{DOCTYPE, Markup, html};

use crate::{
    core::{
        Activity, ByteSize, Desired, EntryCount, Failover, HistoryState, MamApproval,
        MamRequestsStatus, MappingFailure, Origin, POLL_INTERVAL, Port, PortForwarding, Protection,
        Qbittorrent, Ratio, Records, Registry, Session, Setting, SettingStatus, Settings,
        SettingsState, SwitchAllowance, Torrent, TorrentRecord, TorrentState, Trim, Tunnel,
    },
    runner::Observation,
    shell::{
        describe::{CATEGORIES, exit_ip_failure, qbittorrent_failure, timestamp},
        history::{HistoryPage, HistoryQuery, Page},
    },
};

use super::Deployment;

/// Which page is showing, for navigation and for returning after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Location {
    Status,
    History,
    Settings,
}

impl Location {
    pub(super) const fn path(self) -> &'static str {
        match self {
            Self::Status => "/",
            Self::History => "/history",
            Self::Settings => "/settings",
        }
    }

    pub(super) fn from_path(path: &str) -> Self {
        match path {
            "/history" => Self::History,
            "/settings" => Self::Settings,
            _ => Self::Status,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Status => "Status",
            Self::History => "History",
            Self::Settings => "Settings",
        }
    }
}

fn time(time: SystemTime, style: &str) -> Markup {
    let stamp = timestamp(time);
    html! { time datetime=(stamp) data-style=(style) { (stamp) } }
}

fn layout(location: Location, observation: &Observation, body: &Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (location.title()) " · Windlass" }
                link rel="stylesheet" href="/static/app.css";
                script src="/static/app.js" defer {}
            }
            body {
                header.top {
                    div.bar {
                        a.brand href="/" { "Windlass" }
                        nav aria-label="Main" {
                            @for page in [Location::Status, Location::History, Location::Settings] {
                                @if page == location {
                                    a href=(page.path()) aria-current="page" { (page.title()) }
                                } @else {
                                    a href=(page.path()) { (page.title()) }
                                }
                            }
                        }
                        div.controls {
                            span #connection .connection { "Not live: reload to refresh" }
                            (header_live(observation, location))
                        }
                    }
                    div #disconnected .disconnected-banner role="status" hidden {
                        "Lost connection to Windlass. Everything below is from before then. Reconnecting…"
                    }
                }
                main { (body) }
            }
        }
    }
}

/// The parts of every page that update live.
///
/// The header's command returns to the page given by `location`.
pub(super) fn live(observation: &Observation, location: Location) -> Markup {
    html! {
        (header_live(observation, location))
        (status_live(observation))
        (settings_live(observation))
    }
}

fn header_live(observation: &Observation, location: Location) -> Markup {
    let status = observation.state.mam_requests();
    let (label, tone) = match status {
        MamRequestsStatus::Starting => ("Starting…", "neutral"),
        MamRequestsStatus::StartupFailed => ("Blocked: startup failed", "error"),
        MamRequestsStatus::Running => ("Running", "neutral"),
        MamRequestsStatus::Paused => ("Paused", "warning"),
        MamRequestsStatus::PausePending => ("Blocked, saving pause…", "pending"),
        MamRequestsStatus::PauseNotSaved => ("Blocked, pause not saved", "error"),
        MamRequestsStatus::ResumePending => ("Paused, saving resume…", "pending"),
        MamRequestsStatus::ResumeNotSaved => ("Paused, resume not saved", "error"),
    };
    let button = match status {
        MamRequestsStatus::Starting | MamRequestsStatus::StartupFailed => None,
        MamRequestsStatus::Running => Some(("/mam/pause", "Pause MAM requests", "secondary")),
        MamRequestsStatus::Paused => Some(("/mam/resume", "Resume MAM requests", "primary")),
        MamRequestsStatus::PausePending | MamRequestsStatus::ResumePending => {
            Some(("", "Saving…", "secondary"))
        }
        MamRequestsStatus::PauseNotSaved => {
            Some(("/mam/pause", "Try saving the pause again", "primary"))
        }
        MamRequestsStatus::ResumeNotSaved => Some(("/mam/resume", "Try resuming again", "primary")),
    };
    html! {
        div.mam-control data-live="header" {
            span class={ "pill " (tone) } { "MAM requests: " strong { (label) } }
            @if let Some((action, text, style)) = button {
                @if action.is_empty() {
                    button class=(style) type="button" disabled { (text) }
                } @else {
                    form method="post" action=(action) {
                        input type="hidden" name="next" value=(location.path());
                        button class=(style) type="submit" { (text) }
                    }
                }
            }
        }
    }
}

const fn mam_requests_explanation(status: MamRequestsStatus) -> &'static str {
    match status {
        MamRequestsStatus::Starting => {
            "Windlass is loading its saved state. No MAM requests are made until it is loaded."
        }
        MamRequestsStatus::StartupFailed => {
            "Saved state could not be loaded, so MAM requests stay blocked. Check the storage error below."
        }
        MamRequestsStatus::Running => {
            "Windlass may contact MAM when its other conditions are met. Pausing does not stop qBittorrent's tracker traffic."
        }
        MamRequestsStatus::Paused => {
            "The pause is saved and survives a restart. Authentication is unverified and IP reporting is unconfirmed while paused; this does not mean MAM rejected anything."
        }
        MamRequestsStatus::PausePending => {
            "Requests are already blocked. The pause is confirmed once it is saved."
        }
        MamRequestsStatus::PauseNotSaved => {
            "Requests are blocked now, but the pause could not be saved, so a restart could resume them."
        }
        MamRequestsStatus::ResumePending => "Requests stay paused until resuming is saved.",
        MamRequestsStatus::ResumeNotSaved => {
            "Resuming could not be saved, so requests stay paused."
        }
    }
}

const fn cookie_status(settings: &SettingsState) -> (&'static str, &'static str) {
    match settings {
        SettingsState::Unread | SettingsState::Reading => ("Loading…", "neutral"),
        SettingsState::ReadFailed => ("Settings could not be read", "error"),
        SettingsState::Loaded(Settings { api_cookie, .. }) => match api_cookie.status() {
            SettingStatus::Saving => ("Saving…", "pending"),
            SettingStatus::NotSaved => ("Change not saved; no cookie is used", "error"),
            SettingStatus::Saved if api_cookie.saved.is_some() => ("Configured", "ok"),
            SettingStatus::Saved => ("Not configured", "warning"),
        },
    }
}

fn history_status(history: HistoryState, settings: &SettingsState) -> String {
    let limit = settings.history_limit();
    match history {
        HistoryState::Closed | HistoryState::Opening(_) => String::from("Opening…"),
        HistoryState::Unavailable => String::from("Unavailable: history could not be opened"),
        HistoryState::Open { retained, trim } => {
            let kept = limit.map_or_else(
                || format!("About {} entries kept", retained.get()),
                |limit| format!("About {} of {limit} entries kept", retained.get()),
            );
            match trim {
                Trim::Idle => kept,
                Trim::Pending(_) => format!("{kept}; removing the oldest…"),
                Trim::Failed => {
                    format!("{kept}; removing the oldest failed and resumes after a restart")
                }
            }
        }
    }
}

fn status_live(observation: &Observation) -> Markup {
    let status = observation.state.mam_requests();
    let (cookie, cookie_tone) = cookie_status(&observation.settings);
    let needs_cookie = observation
        .settings
        .loaded()
        .is_some_and(|settings| settings.api_cookie.saved.is_none());
    html! {
        div.stack data-live="status" {
            @if needs_cookie {
                section.attention aria-labelledby="setup" {
                    h2 #setup { "Set up MAM access" }
                    ol {
                        li { a href="/settings#api-cookie" { "Add your MAM API cookie" } "." }
                        li { "Resume MAM requests when you are ready. Windlass starts paused." }
                    }
                }
            }
            div.grid {
                section.panel aria-labelledby="mam-requests" {
                    div.panel-head { h2 #mam-requests { "MAM requests" } }
                    p { (mam_requests_explanation(status)) }
                }
                section.panel aria-labelledby="mam-access" {
                    div.panel-head {
                        h2 #mam-access { "MAM access" }
                        span class={ "tag " (cookie_tone) } { (cookie) }
                    }
                    dl {
                        dt { "API cookie" } dd { (cookie) }
                        dt { "Authentication" } dd { "Unverified" }
                        dt { "Seedbox registration" } dd { "Unconfirmed" }
                    }
                    p.note { "Windlass does not contact MAM yet, so these remain unverified." }
                }
                section.panel aria-labelledby="history-panel" {
                    div.panel-head { h2 #history-panel { "History" } a href="/history" { "View history" } }
                    p { (history_status(observation.history, &observation.settings)) }
                }
                (tunnel_panel(&observation.tunnel, SystemTime::now()))
                (port_forwarding_panel(&observation.port_forwarding))
                (qbittorrent_panel(&observation.qbittorrent, observation.port_forwarding.forwarded_port()))
            }
            (torrent_list(&observation.qbittorrent, &observation.registry))
            @if let Some(error) = &observation.storage_error {
                section.panel.error aria-labelledby="storage-error" {
                    h2 #storage-error { "Latest storage error" }
                    p.mono { (error) }
                }
            }
        }
    }
}

fn tunnel_panel(tunnel: &Tunnel, now: SystemTime) -> Markup {
    let Some(watch) = &tunnel.watch else {
        return html! {
            section.panel aria-labelledby="tunnel" {
                div.panel-head {
                    h2 #tunnel { "VPN tunnel" }
                    span.tag.neutral { "Not configured" }
                }
                p { "No exit-IP service is configured, so Windlass cannot tell whether the VPN carries traffic." }
            }
        };
    };
    let (tag, tone) = match (&watch.failover, watch.protection) {
        (Some(_), _) => ("Switching server…", "pending"),
        (None, Protection::Unknown) => ("Checking…", "pending"),
        (None, Protection::Available { .. }) => ("Protected", "ok"),
        (None, Protection::Unavailable { .. }) => ("Unprotected", "error"),
    };
    html! {
        section.panel aria-labelledby="tunnel" {
            div.panel-head {
                h2 #tunnel { "VPN tunnel" }
                span class={ "tag " (tone) } { (tag) }
            }
            dl {
                dt { "Server" }
                dd {
                    @match &watch.current {
                        Some(server) => span.mono { (server) },
                        None => span { "Unknown" },
                    }
                }
                @match watch.protection {
                    Protection::Available { exit_ip, checked_at } => {
                        dt { "Exit IP" } dd.mono { (exit_ip) }
                        dt { "Checked" } dd { (time(checked_at, "full")) }
                    }
                    Protection::Unavailable { since } => {
                        dt { "Exit IP" } dd { "Unknown" }
                        dt { "Failing since" } dd { (time(since, "full")) }
                    }
                    Protection::Unknown => { dt { "Exit IP" } dd { "Not checked yet" } }
                }
                @if let Some(reason) = watch.last_failure {
                    dt { "Failed checks" } dd { (watch.failures) }
                    dt { "Latest failure" } dd { (exit_ip_failure(reason)) }
                }
                dt { "Failover" }
                dd {
                    @match &watch.failover {
                        Some(Failover::Saving(switch)) => { "Recording a switch to " span.mono { (switch.server) } "…" }
                        Some(Failover::Switching(server)) => { "Switching to " span.mono { (server) } "…" }
                        None => (switch_allowance(tunnel.switch_allowance(now))),
                    }
                }
            }
        }
    }
}

fn switch_allowance(allowance: SwitchAllowance) -> Markup {
    match allowance {
        SwitchAllowance::Now => html! { "Allowed if checks keep failing" },
        SwitchAllowance::After(at) => {
            html! { "Not before " (time(at, "full")) ", as MAM allows one IP change an hour" }
        }
        SwitchAllowance::Unknown => html! { "Blocked: the last switch is not known" },
        SwitchAllowance::NoAlternative => html! { "No other server is configured" },
    }
}

fn port_forwarding_panel(forwarding: &PortForwarding) -> Markup {
    let (tag, tone) = match forwarding {
        PortForwarding::Disabled => ("Not configured", "neutral"),
        PortForwarding::Enabled { lease: None, .. } => ("No forwarded port", "error"),
        PortForwarding::Enabled { failures: 0, .. } => ("Forwarded", "ok"),
        PortForwarding::Enabled { .. } => ("Renewal failing", "warning"),
    };
    html! {
        section.panel aria-labelledby="port-forwarding" {
            div.panel-head {
                h2 #port-forwarding { "Port forwarding" }
                span class={ "tag " (tone) } { (tag) }
            }
            @match forwarding {
                PortForwarding::Disabled => {
                    p { "No ProtonVPN NAT-PMP gateway is configured, so peers cannot connect in." }
                }
                PortForwarding::Enabled { lease, failures, last_failure, .. } => {
                    dl {
                        @if let Some(lease) = lease {
                            dt { "Forwarded port" }
                            dd.mono { "TCP " (lease.tcp.get()) ", UDP " (lease.udp.get()) }
                            dt { "Renewed" } dd { (time(lease.renewed_at, "full")) }
                        } @else {
                            dt { "Forwarded port" } dd { "None" }
                        }
                        @if let Some(reason) = last_failure {
                            dt { "Failed attempts" } dd { (failures) }
                            dt { "Latest failure" } dd { (port_failure(*reason)) }
                        }
                    }
                }
            }
        }
    }
}

const fn port_failure(reason: MappingFailure) -> &'static str {
    match reason {
        MappingFailure::NoResponse => "The gateway did not answer",
        MappingFailure::Refused { .. } => "The gateway refused the request",
        MappingFailure::Malformed => "The gateway's answer was not valid",
    }
}

const fn approval(approval: MamApproval) -> (&'static str, &'static str) {
    match approval {
        MamApproval::Recommended => ("Recommended by MAM", "ok"),
        MamApproval::Allowed => ("Allowed by MAM", "ok"),
        MamApproval::Discouraged => ("Discouraged by MAM", "warning"),
        MamApproval::NotAllowed => ("Not allowed by MAM", "error"),
    }
}

fn qbittorrent_panel(qbittorrent: &Qbittorrent, forwarded: Option<Port>) -> Markup {
    let (tag, tone) = match qbittorrent {
        Qbittorrent::Disabled => ("Not configured", "neutral"),
        Qbittorrent::Connecting => ("Connecting…", "pending"),
        Qbittorrent::Unreachable { .. } => ("Unreachable", "error"),
        Qbittorrent::Connected(session) => approval(session.approval),
    };
    html! {
        section.panel aria-labelledby="qbittorrent" {
            div.panel-head {
                h2 #qbittorrent { "qBittorrent" }
                span class={ "tag " (tone) } { (tag) }
            }
            @match qbittorrent {
                Qbittorrent::Disabled => p { "No qBittorrent WebUI is configured." },
                Qbittorrent::Connecting => p { "Waiting for qBittorrent's WebUI to answer." },
                Qbittorrent::Unreachable { reason, .. } => {
                    p { (qbittorrent_failure(*reason)) ". Retrying every " (POLL_INTERVAL.as_secs()) " seconds." }
                }
                Qbittorrent::Connected(session) => {
                    dl {
                        dt { "Version" } dd.mono { (session.version) }
                        dt { "Network interface" }
                        dd {
                            @match &session.preferences.interface {
                                Some(interface) => span.mono { (interface) },
                                None => span { "Any interface; not bound to the VPN" },
                            }
                        }
                        dt { "Listen port" }
                        dd {
                            @match session.preferences.listen_port {
                                Some(port) => span.mono { (port.get()) },
                                None => span { "Unknown" },
                            }
                            @match (forwarded, session.activity) {
                                (_, Activity::SettingListenPort(_)) => " · updating to the forwarded port…",
                                (Some(port), _) if session.preferences.listen_port == Some(port) => " · matches the forwarded port",
                                (Some(_), _) => " · differs from the forwarded port",
                                (None, _) => "",
                            }
                        }
                        dt { "Torrents" } dd { (session.torrents.len()) }
                    }
                }
            }
        }
    }
}

fn size(bytes: ByteSize) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes.0;
    let mut unit = 0;
    let mut tenths = 0;
    while value >= 1024 && unit + 1 < UNITS.len() {
        tenths = (value % 1024) * 10 / 1024;
        value /= 1024;
        unit += 1;
    }
    let unit = UNITS.get(unit).copied().unwrap_or("B");
    if unit == "B" {
        format!("{value} {unit}")
    } else {
        format!("{value}.{tenths} {unit}")
    }
}

const fn torrent_state(state: TorrentState) -> &'static str {
    match state {
        TorrentState::FetchingMetadata => "Fetching metadata",
        TorrentState::Downloading => "Downloading",
        TorrentState::DownloadStalled => "Downloading, stalled",
        TorrentState::DownloadQueued => "Queued to download",
        TorrentState::Seeding => "Seeding",
        TorrentState::SeedStalled => "Seeding, no peers",
        TorrentState::SeedQueued => "Queued to seed",
        TorrentState::Stopped { complete: true } => "Stopped, complete",
        TorrentState::Stopped { complete: false } => "Stopped, incomplete",
        TorrentState::Checking => "Checking",
        TorrentState::Moving => "Moving",
        TorrentState::MissingFiles => "Files missing",
        TorrentState::Error => "Error",
        TorrentState::Unknown => "Unknown",
    }
}

fn progress(torrent: &Torrent) -> String {
    let permille = torrent.progress.permille();
    format!("{}.{}%", permille / 10, permille % 10)
}

fn ratio(ratio: Ratio) -> String {
    format!("{}.{:02}", ratio.0 / 1000, ratio.0 % 1000 / 10)
}

/// The torrent registry, with what qBittorrent reports now; while the
/// registry is unavailable, what qBittorrent reports.
fn torrent_list(qbittorrent: &Qbittorrent, registry: &Registry) -> Markup {
    let session = match qbittorrent {
        Qbittorrent::Connected(session) => Some(&**session),
        _ => None,
    };
    match registry {
        Registry::Loaded(records) => registry_table(records, session),
        Registry::ReadFailed => html! {
            section.panel.error aria-labelledby="registry-error" {
                h2 #registry-error { "Torrent registry unavailable" }
                p { "The torrent registry could not be read, so torrents are shown but not managed." }
            }
            (session_table(session))
        },
        Registry::Unread | Registry::Reading => session_table(session),
    }
}

fn session_table(session: Option<&Session>) -> Markup {
    let Some(session) = session.filter(|session| !session.torrents.is_empty()) else {
        return html! {};
    };
    let mut torrents: Vec<_> = session.torrents.values().collect();
    torrents.sort_by_key(|torrent| torrent.name.to_lowercase());
    html! {
        section.panel.table-box aria-labelledby="torrents" {
            h2 #torrents .table-title { "Torrents" }
            table.history {
                thead { tr {
                    th scope="col" { "Name" }
                    th scope="col" { "State" }
                    th scope="col" { "Progress" }
                    th scope="col" { "Size" }
                } }
                tbody {
                    @for torrent in torrents {
                        tr {
                            td { (torrent.name) }
                            td { (torrent_state(torrent.state)) }
                            td.mono { (progress(torrent)) }
                            td.mono { (size(torrent.size)) }
                        }
                    }
                }
            }
        }
    }
}

const fn wanted(record: &TorrentRecord) -> &'static str {
    match (record.desired, record.origin) {
        (Desired::Queued, _) => "Queued",
        (Desired::Active, Origin::Adopted) => "Active, adopted",
        (Desired::Active, Origin::Bookmark | Origin::Redownload) => "Active",
        (Desired::Removed, _) => "Removed",
    }
}

/// What qBittorrent reports for a row now, and whether it needs attention.
fn registry_state(
    record: &TorrentRecord,
    live: Option<&Torrent>,
    listed: bool,
) -> (&'static str, bool) {
    match (live, record.desired) {
        (Some(torrent), _) => (
            torrent_state(torrent.state),
            torrent.state == TorrentState::MissingFiles,
        ),
        (None, Desired::Active) if listed => ("Missing from qBittorrent", true),
        (None, Desired::Queued) => ("Waiting for a MAM slot", false),
        (None, Desired::Removed) => ("Not in qBittorrent", false),
        (None, Desired::Active) => ("Unknown", false),
    }
}

fn registry_table(records: &Records, session: Option<&Session>) -> Markup {
    type Row<'a> = (
        String,
        &'a TorrentRecord,
        Option<&'a Torrent>,
        Option<&'a Torrent>,
    );
    let mut rows: Vec<Row<'_>> = records
        .records()
        .map(|record| {
            let live = record
                .identity
                .hash()
                .and_then(|hash| session?.torrents.get(hash));
            // What qBittorrent reports now, or else what it reported last.
            let seen = live.or_else(|| record.observation.as_ref().map(|seen| &seen.torrent));
            let name = seen.map_or_else(
                || {
                    record
                        .identity
                        .mam_id()
                        .map_or_else(String::new, |id| format!("MAM torrent {}", id.get()))
                },
                |torrent| torrent.name.clone(),
            );
            (name, record, live, seen)
        })
        .collect();
    if rows.is_empty() {
        return html! {};
    }
    rows.sort_by_key(|(name, ..)| name.to_lowercase());
    let listed = session.is_some_and(Session::listed);
    html! {
        section.panel.table-box aria-labelledby="torrents" {
            h2 #torrents .table-title { "Torrents" }
            @if records.save_failed() {
                p.note { "The latest changes could not be saved; Windlass retries with the next update." }
            }
            table.history {
                thead { tr {
                    th scope="col" { "Name" }
                    th scope="col" { "Wanted" }
                    th scope="col" { "State" }
                    th scope="col" { "Progress" }
                    th scope="col" { "Size" }
                    th scope="col" { "Uploaded" }
                    th scope="col" { "Completed" }
                } }
                tbody {
                    @for (name, record, live, seen) in rows {
                        @let (state, attention) = registry_state(record, live, listed);
                        tr {
                            td { (name) }
                            td { (wanted(record)) }
                            td class=[attention.then_some("attention")] { (state) }
                            @match seen {
                                Some(torrent) => {
                                    td.mono { (progress(torrent)) }
                                    td.mono { (size(torrent.size)) }
                                    td.mono { (size(torrent.uploaded)) " · ratio " (ratio(torrent.ratio)) }
                                    td {
                                        @match torrent.completed_at {
                                            Some(at) => (time(at, "full")),
                                            None => "Not yet",
                                        }
                                    }
                                }
                                None => { td {} td {} td {} td {} }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The status page.
pub(super) fn status(observation: &Observation) -> Markup {
    let body = html! {
        div.page {
            h1.visually-hidden { "Status" }
            (status_live(observation))
        }
    };
    layout(Location::Status, observation, &body)
}

const fn setting_label<T: Clone + PartialEq>(setting: &Setting<T>) -> (&'static str, &'static str) {
    match setting.status() {
        SettingStatus::Saved => ("Saved", "ok"),
        SettingStatus::Saving => ("Saving…", "pending"),
        SettingStatus::NotSaved => ("Not saved", "error"),
    }
}

fn settings_live(observation: &Observation) -> Markup {
    let (cookie, cookie_tone) = cookie_status(&observation.settings);
    let (limit, limit_tone) = observation
        .settings
        .loaded()
        .map_or(("Loading…", "neutral"), |settings| {
            setting_label(&settings.history_limit)
        });
    html! {
        span class={ "tag " (cookie_tone) } data-live="api-cookie-status" { (cookie) }
        span class={ "tag " (limit_tone) } data-live="history-limit-status" { (limit) }
    }
}

/// Errors and submitted values to show again after a rejected settings form.
#[derive(Debug, Default)]
pub(super) struct SettingsForm {
    pub(super) history_limit: Option<String>,
    pub(super) history_limit_error: Option<String>,
    pub(super) api_cookie: Option<String>,
    pub(super) api_cookie_error: Option<String>,
}

/// The settings page.
pub(super) fn settings(
    observation: &Observation,
    deployment: &Deployment,
    form: &SettingsForm,
) -> Markup {
    let (cookie, cookie_tone) = cookie_status(&observation.settings);
    let loaded = observation.settings.loaded();
    let body = html! {
        div.page.narrow {
            h1 { "Settings" }
            section.panel.flush aria-labelledby="windlass-settings" {
                div.section-head {
                    h2 #windlass-settings { "Windlass settings" }
                    p { "Saved by Windlass. Each change is confirmed only once it is saved." }
                }
                @if let Some(settings) = loaded {
                    form.setting #api-cookie method="post" action="/settings/api-cookie" {
                        div.setting-head {
                            label for="cookie" { "MAM API cookie" }
                            span class={ "tag " (cookie_tone) } data-live="api-cookie-status" { (cookie) }
                        }
                        p.hint { "Paste the mam_id value of an IP- or ASN-locked session with seedbox permission." }
                        div.row {
                            input #cookie type="text" name="cookie" autocomplete="off" spellcheck="false"
                                placeholder="mam_id value"
                                value=[form.api_cookie.clone().or_else(|| {
                                    settings.api_cookie.requested.as_ref().map(|cookie| cookie.expose_secret().to_owned())
                                })]
                                aria-invalid=[form.api_cookie_error.as_ref().map(|_| "true")]
                                aria-describedby=[form.api_cookie_error.as_ref().map(|_| "cookie-error")];
                            button.primary type="submit" {
                                @if settings.api_cookie.saved.is_some() { "Replace" } @else { "Save" }
                            }
                            @if settings.api_cookie.saved.is_some() || settings.api_cookie.requested.is_some() {
                                button.secondary type="submit" formaction="/settings/api-cookie/clear" { "Clear" }
                            }
                        }
                        @if let Some(error) = &form.api_cookie_error {
                            p.field-error #cookie-error { (error) }
                        }
                    }
                    form.setting method="post" action="/settings/history-limit" {
                        div.setting-head {
                            label for="history-limit" { "History size limit" }
                            @let (label, tone) = setting_label(&settings.history_limit);
                            span class={ "tag " (tone) } data-live="history-limit-status" { (label) }
                        }
                        p.hint { "When history grows past this many entries, the oldest are removed. Default 10000." }
                        div.row {
                            input #history-limit type="text" inputmode="numeric" name="limit"
                                value=(form.history_limit.clone().unwrap_or_else(|| settings.history_limit.requested.to_string()))
                                aria-invalid=[form.history_limit_error.as_ref().map(|_| "true")]
                                aria-describedby=[form.history_limit_error.as_ref().map(|_| "limit-error")];
                            span { "entries" }
                            button.primary type="submit" { "Save" }
                        }
                        @if let Some(error) = &form.history_limit_error {
                            p.field-error #limit-error { (error) }
                        }
                    }
                } @else {
                    p.setting { (cookie) }
                }
            }
            section.panel.flush aria-labelledby="deployment" {
                div.section-head {
                    h2 #deployment { "Deployment settings" }
                    p { "Set in your NixOS configuration. Change them there and rebuild." }
                }
                dl.setting {
                    dt { "Listen addresses" }
                    dd.mono { (deployment.listen.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")) }
                    dt { "Allowed host names" }
                    @if deployment.allowed_hosts.is_empty() {
                        dd { "None; only the listen addresses" }
                    } @else {
                        dd.mono { (deployment.allowed_hosts.join(", ")) }
                    }
                    dt { "Database" }
                    dd.mono { (deployment.database.display()) }
                }
            }
        }
    };
    layout(Location::Settings, observation, &body)
}

fn delay(occurred: SystemTime, processed: SystemTime) -> String {
    processed.duration_since(occurred).map_or_else(
        |_| String::from("clock changed"),
        |delay| format!("+{} ms", delay.as_millis()),
    )
}

fn page_link(query: &HistoryQuery, page: Page) -> String {
    let mut parameters = Vec::new();
    if let Some(category) = query.category {
        parameters.push(format!("kind={category}"));
    }
    if query.routine {
        parameters.push(String::from("routine=on"));
    }
    for (name, value) in [("from", query.from), ("to", query.until)] {
        if let Some(value) = value {
            parameters.push(format!("{name}={}", timestamp(value).replace('+', "%2B")));
        }
    }
    match page {
        Page::Newest => {}
        Page::Before(id) => parameters.push(format!("before={id}")),
        Page::After(id) => parameters.push(format!("after={id}")),
    }
    format!("/history?{}", parameters.join("&"))
}

/// The history page.
pub(super) fn history(
    observation: &Observation,
    query: &HistoryQuery,
    result: &Result<HistoryPage, String>,
    retained: Option<EntryCount>,
) -> Markup {
    let body = html! {
        div.page {
            div.title-row {
                h1 { "History" }
                @if let Some(retained) = retained {
                    p.muted { "About " (retained.get()) " entries kept" }
                }
            }
            form.panel.filters #history-filter method="get" action="/history" {
                div.field {
                    label for="kind" { "Kind" }
                    select #kind name="kind" {
                        option value="" { "All kinds" }
                        @for (value, label) in CATEGORIES {
                            option value=(value) selected[query.category == Some(value)] { (label) }
                        }
                    }
                }
                div.field {
                    label for="from_local" { "From" }
                    input #from_local type="datetime-local" name="from_local";
                    input type="hidden" name="from" value=[query.from.map(timestamp)];
                }
                div.field {
                    label for="to_local" { "To" }
                    input #to_local type="datetime-local" name="to_local";
                    input type="hidden" name="to" value=[query.until.map(timestamp)];
                }
                label.check {
                    input type="checkbox" name="routine" checked[query.routine];
                    "Show routine entries"
                }
                button.primary type="submit" { "Apply" }
                @if query.from.is_some() || query.until.is_some() {
                    p.muted.filter-summary {
                        "Showing entries processed"
                        @if let Some(from) = query.from { " from " (time(from, "full")) }
                        @if let Some(until) = query.until { " until " (time(until, "full")) }
                        ". " a href="/history" { "Clear filters" }
                    }
                }
            }
            @match result {
                Err(error) => {
                    section.panel.error { h2 { "History could not be read" } p.mono { (error) } }
                }
                Ok(page) => {
                    div.panel.table-box {
                        table.history {
                            caption.visually-hidden { "History entries, newest first. Actions are listed under the event whose processing produced them." }
                            thead { tr {
                                th scope="col" { "#" }
                                th scope="col" { "Happened" }
                                th scope="col" { "Processed" }
                                th scope="col" { "Entry" }
                            } }
                            tbody {
                                @if page.rows.is_empty() {
                                    tr { td colspan="4" { "No entries match." } }
                                }
                                @for row in &page.rows {
                                    tr class={ "event " (row.category) } {
                                        td.mono.muted { (row.id) }
                                        td.mono { (time(row.occurred_at, "clock")) }
                                        td.mono { (time(row.processed_at, "clock")) " " span.muted { (delay(row.occurred_at, row.processed_at)) } }
                                        td {
                                            span.kind { "EVENT" }
                                            (row.summary)
                                            details { summary { "Data" } pre { (row.event) } }
                                            @if !row.actions.is_empty() {
                                                ul.actions {
                                                    @for action in &row.actions {
                                                        li {
                                                            span.kind.action { "ACTION" }
                                                            (action.summary)
                                                            details { summary { "Data" } pre { (action.detail) } }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    nav.pager aria-label="History pages" {
                        @if let (true, Some(first)) = (page.has_newer, page.rows.first()) {
                            a.button href=(page_link(query, Page::After(first.id))) { "Newer" }
                        } @else { span {} }
                        @if let (true, Some(last)) = (page.has_older, page.rows.last()) {
                            a.button href=(page_link(query, Page::Before(last.id))) { "Older" }
                        }
                    }
                }
            }
        }
    };
    layout(Location::History, observation, &body)
}

/// A plain page for refused or failed requests.
pub(super) fn message(observation: &Observation, title: &str, text: &str) -> Markup {
    let body = html! {
        div.page.narrow {
            h1 { (title) }
            p { (text) }
            p { a href="/" { "Back to status" } }
        }
    };
    layout(Location::Status, observation, &body)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use crate::{
        core::{
            ApiCookie, EntryCount, HistoryLimit, HistoryState, MamPreferencePersistence,
            MamPreferenceState, MamPreferenceUpdate, MamRequestMode, PortForwarding, Qbittorrent,
            Setting, SettingPersistence, Settings, SettingsState, State, Step, Trim,
        },
        runner::Observation,
        shell::history::{HistoryQuery, Page},
    };

    use super::{Location, delay, live, page_link};

    fn observation(state: State, settings: SettingsState, history: HistoryState) -> Observation {
        Observation {
            state,
            settings,
            history,
            port_forwarding: PortForwarding::Disabled,
            qbittorrent: Qbittorrent::Disabled,
            registry: crate::core::Registry::Unread,
            tunnel: crate::core::Tunnel::default(),
            last_event: None,
            storage_error: None,
        }
    }

    fn preference(requested: MamRequestMode, persistence: MamPreferencePersistence) -> State {
        State::Ready(MamPreferenceState {
            saved: None,
            requested: MamPreferenceUpdate::Set(requested),
            effective: MamRequestMode::Paused,
            persistence,
        })
    }

    fn settings(
        api_cookie: Option<ApiCookie>,
        persistence: SettingPersistence<Option<ApiCookie>>,
    ) -> SettingsState {
        SettingsState::Loaded(Settings {
            history_limit: Setting {
                saved: HistoryLimit::default(),
                requested: HistoryLimit::default(),
                persistence: SettingPersistence::Pending(HistoryLimit::default()),
            },
            api_cookie: Setting {
                saved: api_cookie.clone(),
                requested: api_cookie,
                persistence,
            },
        })
    }

    #[test]
    fn every_mam_request_state_has_words_and_the_right_control() {
        let pending =
            MamPreferencePersistence::Pending(MamPreferenceUpdate::Set(MamRequestMode::Paused));
        for (state, label, control) in [
            (State::NotStarted, "Starting…", None),
            (
                preference(MamRequestMode::Paused, pending),
                "Blocked, saving pause…",
                Some("Saving…"),
            ),
            (
                preference(MamRequestMode::Paused, MamPreferencePersistence::Failed),
                "Blocked, pause not saved",
                Some("Try saving the pause again"),
            ),
            (
                preference(MamRequestMode::Enabled, pending),
                "Paused, saving resume…",
                Some("Saving…"),
            ),
        ] {
            let html = live(
                &observation(state, SettingsState::Reading, HistoryState::Closed),
                Location::Settings,
            )
            .into_string();
            assert!(html.contains(label), "{html}");
            if let Some(control) = control {
                assert!(html.contains(control), "{html}");
            }
            assert!(html.contains("Loading…"));
            assert!(html.contains("Opening…"));
        }
    }

    #[test]
    fn setting_and_history_states_have_words() {
        let cookie = ApiCookie::parse("value").ok();
        for (settings, expected) in [
            (
                settings(cookie.clone(), SettingPersistence::Pending(cookie.clone())),
                "Saving…",
            ),
            (
                settings(cookie.clone(), SettingPersistence::Failed),
                "Change not saved; no cookie is used",
            ),
            (settings(cookie, SettingPersistence::Idle), "Configured"),
        ] {
            let html = live(
                &observation(
                    State::NotStarted,
                    settings,
                    HistoryState::Open {
                        retained: EntryCount::new(5),
                        trim: Trim::Pending(Step::default()),
                    },
                ),
                Location::Status,
            )
            .into_string();
            assert!(html.contains(expected), "{html}");
            assert!(html.contains("removing the oldest…"));
        }
        let html = live(
            &observation(
                State::Failed,
                SettingsState::ReadFailed,
                HistoryState::Open {
                    retained: EntryCount::new(5),
                    trim: Trim::Failed,
                },
            ),
            Location::History,
        )
        .into_string();
        assert!(html.contains("About 5 entries kept; removing the oldest failed"));
        assert!(html.contains("Settings could not be read"));
        assert!(html.contains("Blocked: startup failed"));
    }

    #[test]
    fn delays_and_links_are_described() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        assert_eq!(delay(now, now + Duration::from_millis(3)), "+3 ms");
        assert_eq!(delay(now, now - Duration::from_secs(1)), "clock changed");
        let query = HistoryQuery {
            category: Some("mam"),
            routine: true,
            from: Some(now),
            until: Some(now),
            page: Page::Newest,
        };
        assert_eq!(
            page_link(&query, Page::Before(7)),
            "/history?kind=mam&routine=on&from=1970-01-01T00:00:10Z&to=1970-01-01T00:00:10Z&before=7"
        );
        assert_eq!(
            page_link(&HistoryQuery::default(), Page::Newest),
            "/history?"
        );
    }

    fn drive(events: Vec<crate::core::Event>) -> crate::core::Core {
        let clock = crate::core::ClockReading {
            monotonic: std::time::Instant::now(),
            wall: SystemTime::UNIX_EPOCH,
        };
        let mut core = crate::core::Core::new();
        for event in events {
            let _ = core.handle(clock, event);
        }
        core
    }

    const FORWARDED: crate::core::Port = match crate::core::Port::new(41000) {
        Some(port) => port,
        None => unreachable!(),
    };

    fn lease(port: crate::core::Port) -> crate::core::Event {
        crate::core::Event::PortForwarding(crate::core::PortForwardingEvent::Mapped {
            tcp: port,
            udp: port,
            lifetime: crate::core::LEASE_LIFETIME,
        })
    }

    #[test]
    fn port_forwarding_states_have_words() {
        use crate::core::{Event, LeaseRequest, MappingFailure, PortForwardingEvent};

        use super::port_forwarding_panel;

        let render = |forwarding| port_forwarding_panel(&forwarding).into_string();
        assert!(render(PortForwarding::Disabled).contains("Not configured"));
        for (reason, text) in [
            (MappingFailure::NoResponse, "did not answer"),
            (MappingFailure::Refused { result_code: 2 }, "refused"),
            (MappingFailure::Malformed, "not valid"),
        ] {
            let html = render(PortForwarding::Enabled {
                lease: None,
                request: LeaseRequest::Outstanding,
                failures: 2,
                last_failure: Some(reason),
            });
            assert!(
                html.contains("No forwarded port") && html.contains(text),
                "{html}"
            );
        }
        let core = drive(vec![
            Event::PortForwarding(PortForwardingEvent::Enable),
            lease(FORWARDED),
        ]);
        let html = render(*core.port_forwarding());
        assert!(
            html.contains("Forwarded") && html.contains("TCP 41000, UDP 41000"),
            "{html}"
        );
    }

    #[test]
    fn qbittorrent_states_have_words() -> Result<(), &'static str> {
        use crate::core::{
            ByteSize, Event, Port, PortForwardingEvent, Progress, QbittorrentEvent,
            QbittorrentFailure, QbittorrentPreferences, SyncUpdate, TorrentHash, TorrentState,
            TorrentUpdate,
        };

        use super::{qbittorrent_panel, size, torrent_list};

        let render = |markup: maud::Markup| markup.into_string();
        assert!(render(qbittorrent_panel(&Qbittorrent::Disabled, None)).contains("Not configured"));
        assert!(render(qbittorrent_panel(&Qbittorrent::Connecting, None)).contains("Connecting"));
        let unreachable = drive(vec![
            Event::Qbittorrent(QbittorrentEvent::Enable),
            Event::Qbittorrent(QbittorrentEvent::ConnectFailed {
                reason: QbittorrentFailure::Rejected { status: 403 },
            }),
        ]);
        assert!(
            render(qbittorrent_panel(unreachable.qbittorrent(), None)).contains("HTTP status 403")
        );
        let hash = TorrentHash::parse(&"a".repeat(40)).ok_or("invalid hash")?;
        let second = TorrentHash::parse(&"b".repeat(40)).ok_or("invalid hash")?;
        let torrents = SyncUpdate {
            full: true,
            torrents: vec![
                (
                    hash,
                    TorrentUpdate {
                        name: Some(String::from("A Synthetic Audiobook")),
                        state: Some(TorrentState::Stopped { complete: false }),
                        progress: Some(Progress::from_fraction(0.5)),
                        size: Some(ByteSize(1536)),
                        ..TorrentUpdate::default()
                    },
                ),
                (
                    second,
                    TorrentUpdate {
                        name: Some(String::from("Another Synthetic Book")),
                        ..TorrentUpdate::default()
                    },
                ),
            ],
            ..SyncUpdate::default()
        };
        for (version, listen, interface, expected) in [
            (
                "v5.1.0",
                41000,
                Some("wg0"),
                ["Allowed by MAM", "matches the forwarded port"],
            ),
            (
                "v4.6.0",
                1,
                None,
                ["Discouraged by MAM", "updating to the forwarded port"],
            ),
            (
                "v5.2.2-dev",
                1,
                None,
                ["Not allowed by MAM", "Any interface"],
            ),
        ] {
            let core = drive(vec![
                Event::Qbittorrent(QbittorrentEvent::Enable),
                Event::Qbittorrent(QbittorrentEvent::Connected {
                    version: String::from(version),
                    preferences: QbittorrentPreferences {
                        listen_port: Port::new(listen),
                        interface: interface.map(str::to_owned),
                    },
                }),
                Event::Qbittorrent(QbittorrentEvent::Synced(torrents.clone())),
                Event::PortForwarding(PortForwardingEvent::Enable),
                lease(FORWARDED),
            ]);
            let forwarded = core.port_forwarding().forwarded_port();
            let html = render(qbittorrent_panel(core.qbittorrent(), forwarded));
            for text in expected {
                assert!(html.contains(text), "{text}: {html}");
            }
            let list = render(torrent_list(core.qbittorrent(), core.registry()));
            assert!(
                list.contains("A Synthetic Audiobook") && list.contains("50.0%"),
                "{list}"
            );
        }
        let reading = crate::core::Registry::Reading;
        assert!(render(torrent_list(&Qbittorrent::Disabled, &reading)).is_empty());
        assert_eq!(size(ByteSize(512)), "512 B");
        assert_eq!(size(ByteSize(1536)), "1.5 KiB");
        assert_eq!(size(ByteSize(5 * 1024 * 1024 * 1024)), "5.0 GiB");
        Ok(())
    }

    #[test]
    fn every_torrent_state_has_words() {
        use crate::core::TorrentState;

        use super::torrent_state;

        let states = [
            TorrentState::FetchingMetadata,
            TorrentState::Downloading,
            TorrentState::DownloadStalled,
            TorrentState::DownloadQueued,
            TorrentState::Seeding,
            TorrentState::SeedStalled,
            TorrentState::SeedQueued,
            TorrentState::Stopped { complete: true },
            TorrentState::Stopped { complete: false },
            TorrentState::Checking,
            TorrentState::Moving,
            TorrentState::MissingFiles,
            TorrentState::Error,
            TorrentState::Unknown,
        ];
        let words: std::collections::BTreeSet<_> = states.into_iter().map(torrent_state).collect();
        assert_eq!(words.len(), states.len());
    }

    #[test]
    fn renewal_failures_and_recommended_versions_have_words() {
        use crate::core::{Event, PortForwardingEvent, QbittorrentEvent, QbittorrentPreferences};

        use super::{port_forwarding_panel, qbittorrent_panel};

        let core = drive(vec![
            Event::PortForwarding(PortForwardingEvent::Enable),
            lease(FORWARDED),
        ]);
        // A failed renewal while the lease is still valid.
        let mut forwarding = *core.port_forwarding();
        if let PortForwarding::Enabled {
            failures, request, ..
        } = &mut forwarding
        {
            *failures = 1;
            *request = crate::core::LeaseRequest::Outstanding;
        }
        assert!(
            port_forwarding_panel(&forwarding)
                .into_string()
                .contains("Renewal failing")
        );
        let core = drive(vec![
            Event::Qbittorrent(QbittorrentEvent::Enable),
            Event::Qbittorrent(QbittorrentEvent::Connected {
                version: String::from("v5.2.2"),
                preferences: QbittorrentPreferences::default(),
            }),
        ]);
        let html = qbittorrent_panel(core.qbittorrent(), None).into_string();
        assert!(
            html.contains("Recommended by MAM") && html.contains("Unknown"),
            "{html}"
        );
        // Connected, but with no torrents yet: no list.
        assert!(
            super::torrent_list(core.qbittorrent(), core.registry())
                .into_string()
                .is_empty()
        );
    }

    /// A core driven with a clock that moves only when told to.
    struct Clocked {
        core: crate::core::Core,
        start: std::time::Instant,
        elapsed: std::time::Duration,
    }

    impl Clocked {
        fn new() -> Self {
            Self {
                core: crate::core::Core::new(),
                start: std::time::Instant::now(),
                elapsed: std::time::Duration::from_hours(2),
            }
        }

        fn send(&mut self, event: crate::core::Event) -> Vec<crate::core::Action> {
            let clock = crate::core::ClockReading {
                monotonic: self.start + self.elapsed,
                wall: SystemTime::UNIX_EPOCH + self.elapsed,
            };
            self.core.handle(clock, event)
        }

        /// Fail the outstanding tunnel check, then start the next one.
        fn fail_check(&mut self) {
            use crate::core::{Check, Event, ExitIpFailure, TimerName, TunnelEvent};

            let _ = self.send(Event::Tunnel(TunnelEvent::ExitIpCheckFailed {
                reason: ExitIpFailure::Rejected { status: 503 },
            }));
            if let Some(Check::Waiting(id)) =
                self.core.tunnel().watch.as_ref().map(|watch| watch.check)
            {
                let _ = self.send(Event::TimerExpired {
                    name: TimerName::TunnelCheck,
                    id,
                });
            }
        }
    }

    #[test]
    fn tunnel_states_have_words() -> Result<(), crate::core::VpnServerError> {
        use crate::core::{
            Event, StartupEvent, StartupMode, Tunnel, TunnelEvent, VpnServer, VpnSwitch,
        };

        use super::tunnel_panel;

        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_hours(2);
        let render = |tunnel: &Tunnel| tunnel_panel(tunnel, now).into_string();
        assert!(render(&Tunnel::default()).contains("Not configured"));
        let one = VpnServer::try_new("one")?;
        let two = VpnServer::try_new("two")?;
        let mut clocked = Clocked::new();
        let _ = clocked.send(Event::Tunnel(TunnelEvent::Enable {
            servers: vec![one.clone()],
            current: Some(one.clone()),
        }));
        let html = render(clocked.core.tunnel());
        for text in ["Checking…", ">one<", "Not checked yet", "No other server"] {
            assert!(html.contains(text), "{text}: {html}");
        }
        let _ = clocked.send(Event::Tunnel(TunnelEvent::ExitIpObserved {
            ip: std::net::IpAddr::from([192, 0, 2, 7]),
        }));
        let html = render(clocked.core.tunnel());
        assert!(
            html.contains("Protected") && html.contains("192.0.2.7"),
            "{html}"
        );

        let mut clocked = Clocked::new();
        let _ = clocked.send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }));
        let _ = clocked.send(Event::Startup(StartupEvent::StorageOpened));
        let _ = clocked.send(Event::Tunnel(TunnelEvent::Enable {
            servers: vec![one, two.clone()],
            current: None,
        }));
        let html = render(clocked.core.tunnel());
        assert!(
            html.contains("the last switch is not known") && html.contains("Unknown"),
            "{html}"
        );
        let _ = clocked.send(Event::Tunnel(TunnelEvent::LastSwitchRead {
            last_switch: Some(VpnSwitch {
                server: two,
                at: SystemTime::UNIX_EPOCH,
            }),
        }));
        clocked.fail_check();
        let html = render(clocked.core.tunnel());
        for text in [
            "Unprotected",
            "Failing since",
            "HTTP status 503",
            "Allowed if checks",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        clocked.fail_check();
        clocked.fail_check();
        let html = render(clocked.core.tunnel());
        assert!(html.contains("Recording a switch to"), "{html}");
        let _ = clocked.send(Event::Tunnel(TunnelEvent::SwitchSaved));
        let html = render(clocked.core.tunnel());
        assert!(
            html.contains("Switching server…") && html.contains("Switching to"),
            "{html}"
        );
        let _ = clocked.send(Event::Tunnel(TunnelEvent::Switched));
        let html = render(clocked.core.tunnel());
        assert!(
            html.contains("Not before <time datetime=\"1970-01-01T03:00:00Z\""),
            "{html}"
        );
        Ok(())
    }

    fn registry_rows() -> Result<Vec<crate::core::TorrentRecord>, &'static str> {
        use crate::core::{
            Desired, Identity, MamTorrentId, Origin, RecordId, TorrentHash, TorrentRecord,
        };

        let hash = |fill: char| TorrentHash::parse(&fill.to_string().repeat(40));
        let (Some(missing), Some(gone)) = (hash('a'), hash('d')) else {
            return Err("invalid hash");
        };
        let mam = MamTorrentId::new(77).ok_or("zero id")?;
        let row = |id, identity, desired| -> Result<TorrentRecord, &'static str> {
            Ok(TorrentRecord {
                id: RecordId::new(id).ok_or("zero id")?,
                identity,
                origin: Origin::Bookmark,
                desired,
                save_path: None,
                deletion_allowed: false,
                observation: None,
            })
        };
        Ok(vec![
            row(1, Identity::Client(missing), Desired::Active)?,
            row(2, Identity::Mam(mam), Desired::Queued)?,
            row(3, Identity::Client(gone), Desired::Removed)?,
        ])
    }

    /// Start a core whose registry holds these rows.
    fn with_registry(rows: Vec<crate::core::TorrentRecord>) -> Clocked {
        use crate::core::{Event, RegistryEvent, StartupEvent, StartupMode};

        let mut clocked = Clocked::new();
        for event in [
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Resume,
            }),
            Event::Startup(StartupEvent::StorageOpened),
            Event::Registry(RegistryEvent::Read(rows)),
        ] {
            let _ = clocked.send(event);
        }
        clocked
    }

    #[test]
    fn registry_rows_have_words() -> Result<(), &'static str> {
        use crate::core::{
            ByteSize, Event, Progress, QbittorrentEvent, QbittorrentPreferences, Ratio, Registry,
            RegistryEvent, SyncPosition, SyncUpdate, TorrentHash, TorrentState, TorrentUpdate,
        };

        use super::torrent_list;

        let hash = |fill: char| TorrentHash::parse(&fill.to_string().repeat(40));
        let (Some(present), Some(damaged)) = (hash('b'), hash('c')) else {
            return Err("invalid hash");
        };
        let update = |name: &str, state| TorrentUpdate {
            name: Some(name.to_owned()),
            state: Some(state),
            progress: Some(Progress::from_fraction(1.0)),
            size: Some(ByteSize(2048)),
            total_size: Some(ByteSize(2048)),
            uploaded: Some(ByteSize(4096)),
            ratio: Some(Ratio(1500)),
            completed_at: Some(Some(SystemTime::UNIX_EPOCH)),
            ..TorrentUpdate::default()
        };
        let mut clocked = with_registry(registry_rows()?);
        for event in [
            Event::Qbittorrent(QbittorrentEvent::Enable),
            Event::Qbittorrent(QbittorrentEvent::Connected {
                version: String::from("v5.2.2"),
                preferences: QbittorrentPreferences::default(),
            }),
            Event::Qbittorrent(QbittorrentEvent::Synced(SyncUpdate {
                position: SyncPosition(1),
                full: true,
                torrents: vec![
                    (present, update("An Adopted Book", TorrentState::Seeding)),
                    (
                        damaged,
                        update("A Damaged Book", TorrentState::MissingFiles),
                    ),
                ],
                removed: Vec::new(),
            })),
            Event::Registry(RegistryEvent::SaveFailed),
        ] {
            let _ = clocked.send(event);
        }
        let html = torrent_list(clocked.core.qbittorrent(), clocked.core.registry()).into_string();
        for text in [
            "Active, adopted",
            "Missing from qBittorrent",
            "MAM torrent 77",
            "Waiting for a MAM slot",
            "Not in qBittorrent",
            "class=\"attention\">Files missing",
            "4.0 KiB · ratio 1.50",
            "1970-01-01T00:00:00Z",
            "could not be saved",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }

        let html = torrent_list(clocked.core.qbittorrent(), &Registry::ReadFailed).into_string();
        assert!(
            html.contains("Torrent registry unavailable") && html.contains("A Damaged Book"),
            "{html}"
        );
        Ok(())
    }

    #[test]
    fn registry_rows_without_qbittorrent_show_what_was_last_seen() -> Result<(), &'static str> {
        use crate::core::{Observation, Torrent};

        use super::torrent_list;

        let mut rows = registry_rows()?;
        if let Some(first) = rows.first_mut() {
            first.observation = Some(Observation {
                torrent: Torrent {
                    name: String::from("A Remembered Book"),
                    ..Torrent::default()
                },
                last_seen: SystemTime::UNIX_EPOCH,
            });
        }
        let alone = with_registry(rows);
        let html = torrent_list(&Qbittorrent::Disabled, alone.core.registry()).into_string();
        for text in ["A Remembered Book", "Unknown", "Not yet", "<td></td>"] {
            assert!(html.contains(text), "{text}: {html}");
        }
        let empty = with_registry(Vec::new());
        assert!(
            torrent_list(&Qbittorrent::Disabled, empty.core.registry())
                .into_string()
                .is_empty()
        );
        Ok(())
    }
}
