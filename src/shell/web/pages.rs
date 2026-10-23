//! Server-rendered pages. Templates translate core state into words; the core
//! decides what that state is.

use std::time::SystemTime;

use maud::{DOCTYPE, Markup, html};

use crate::{
    core::{
        EntryCount, HistoryState, MamRequestsStatus, Setting, SettingStatus, Settings,
        SettingsState, Trim,
    },
    runner::Observation,
    shell::{
        describe::{CATEGORIES, timestamp},
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
            }
            @if let Some(error) = &observation.storage_error {
                section.panel.error aria-labelledby="storage-error" {
                    h2 #storage-error { "Latest storage error" }
                    p.mono { (error) }
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
            MamPreferenceState, MamPreferenceUpdate, MamRequestMode, Setting, SettingPersistence,
            Settings, SettingsState, State, Step, Trim,
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
            from: Some(now),
            until: Some(now),
            page: Page::Newest,
        };
        assert_eq!(
            page_link(&query, Page::Before(7)),
            "/history?kind=mam&from=1970-01-01T00:00:10Z&to=1970-01-01T00:00:10Z&before=7"
        );
        assert_eq!(
            page_link(&HistoryQuery::default(), Page::Newest),
            "/history?"
        );
    }
}
