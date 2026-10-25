//! The dashboard: server-rendered pages, form commands, and live status.
//!
//! Handlers validate requests, translate them into core events, and render the
//! core's read-only state. They make no decisions of their own.

use std::{
    convert::Infallible, future::Future, net::SocketAddr, path::PathBuf, sync::Arc,
    time::SystemTime,
};

use axum::{
    Form, Router,
    extract::{Query, Request, State as AppState},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Redirect, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use maud::Markup;
use serde::Deserialize;
use time::{
    OffsetDateTime, PrimitiveDateTime, format_description::well_known::Rfc3339,
    macros::format_description,
};
use tokio::{net::TcpListener, sync::watch};
use tokio_stream::{Stream, StreamExt, wrappers::WatchStream};

use crate::{
    core::{
        ActiveLimit, ApiCookie, DownloadingLimit, Event, HistoryLimit, HistoryState, MamEvent,
        MamPauseEvent, MamRequestMode, NotificationEvent, NtfyTopic, PointsReserve, RecordId,
        RegistryEvent, SettingsEvent, StartupEvent, StartupMode, State, TorrentLimits,
    },
    runner::{Observation, Runner},
    shell::{
        EventSender, Integrations, ShellError, ShellResult,
        describe::CATEGORIES,
        history::{self, HistoryQuery, Page},
        storage,
    },
};

mod guard;
mod mam_pages;
mod pages;

use pages::{Location, SettingsForm};

const STYLESHEET: &str = include_str!("static/app.css");
const SCRIPT: &str = include_str!("static/app.js");

/// Deployment facts supplied by the NixOS module. The dashboard shows them but
/// cannot change them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    /// Addresses the dashboard listens on.
    pub listen: Vec<SocketAddr>,
    /// Host names, such as a Tailscale name, accepted in addition to the
    /// listen addresses.
    pub allowed_hosts: Vec<String>,
    /// The `SQLite` database holding Windlass's durable state and history.
    pub database: PathBuf,
    /// Where notifications are published, such as `https://ntfy.sh/`.
    pub ntfy_server: String,
    /// The relay that reaches the ntfy server while the VPN is down, if any.
    pub ntfy_relay: Option<SocketAddr>,
    /// The dashboard address notifications link to, if configured.
    pub dashboard_url: Option<String>,
}

impl Deployment {
    fn accepted_hosts(&self) -> Vec<String> {
        self.listen
            .iter()
            .map(|address| address.ip().to_string())
            .chain(self.allowed_hosts.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone)]
struct App {
    events: EventSender,
    observations: watch::Receiver<Observation>,
    deployment: Arc<Deployment>,
    accepted_hosts: Arc<[String]>,
}

impl App {
    fn observation(&self) -> Observation {
        self.observations.borrow().clone()
    }

    async fn send(&self, event: Event) -> Result<(), Response> {
        self.events.send(event).await.map_err(|_| {
            let page = pages::message(
                &self.observation(),
                "Windlass is shutting down",
                "The request was not accepted.",
            );
            (StatusCode::SERVICE_UNAVAILABLE, page).into_response()
        })
    }
}

/// Build the dashboard routes for a running core.
pub fn router(
    events: EventSender,
    observations: watch::Receiver<Observation>,
    deployment: Deployment,
) -> Router {
    let app = App {
        events,
        observations,
        accepted_hosts: deployment.accepted_hosts().into(),
        deployment: Arc::new(deployment),
    };
    Router::new()
        .route("/", get(status_page))
        .route("/notifications", get(notifications_page))
        .route("/history", get(history_page))
        .route("/settings", get(settings_page))
        .route("/live", get(live))
        .route("/mam/pause", post(pause))
        .route("/mam/resume", post(resume))
        .route("/settings/history-limit", post(set_history_limit))
        .route("/settings/api-cookie", post(set_api_cookie))
        .route("/settings/api-cookie/clear", post(clear_api_cookie))
        .route("/settings/ntfy-topic", post(set_ntfy_topic))
        .route("/settings/ntfy-topic/clear", post(clear_ntfy_topic))
        .route("/settings/points-reserve", post(set_points_reserve))
        .route("/settings/torrent-limits", post(set_torrent_limits))
        .route("/notifications/test", post(send_test_notification))
        .route("/torrents/redownload", post(redownload))
        .route("/mam/purchase/checked", post(purchase_checked))
        .route("/static/app.css", get(stylesheet))
        .route("/static/app.js", get(script))
        .route("/favicon.ico", get(StatusCode::NO_CONTENT))
        .layer(middleware::from_fn_with_state(app.clone(), protect))
        .with_state(app)
}

async fn protect(AppState(app): AppState<App>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let host = text(header::HOST.as_str());
    if !guard::host_allowed(host, &app.accepted_hosts) {
        return (StatusCode::MISDIRECTED_REQUEST, "Unexpected host name.").into_response();
    }
    let changes_state = !matches!(*request.method(), Method::GET | Method::HEAD);
    if changes_state
        && !guard::same_origin(
            host.unwrap_or_default(),
            text(header::ORIGIN.as_str()),
            text("sec-fetch-site"),
        )
    {
        return (StatusCode::FORBIDDEN, "Cross-site request refused.").into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        // "no-referrer" would make browsers send `Origin: null` on our own forms.
        (header::REFERRER_POLICY, "same-origin"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLESHEET,
    )
}

async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        SCRIPT,
    )
}

async fn status_page(AppState(app): AppState<App>) -> Markup {
    pages::status(&app.observation())
}

async fn settings_page(AppState(app): AppState<App>) -> Markup {
    pages::settings(
        &app.observation(),
        &app.deployment,
        &SettingsForm::default(),
    )
}

#[derive(Debug, Deserialize)]
struct LiveParameters {
    page: Option<String>,
}

async fn live(
    AppState(app): AppState<App>,
    Query(parameters): Query<LiveParameters>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let location = Location::from_path(parameters.page.as_deref().unwrap_or("/"));
    let updates = WatchStream::new(app.observations).map(move |observation| {
        Ok(SseEvent::default()
            .event("live")
            .data(pages::live(&observation, location).into_string()))
    });
    Sse::new(updates).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
struct ReturnTo {
    next: Option<String>,
}

fn back(next: Option<&str>) -> Redirect {
    Redirect::to(Location::from_path(next.unwrap_or("/")).path())
}

async fn set_mam_mode(app: &App, mode: MamRequestMode, next: Option<&str>) -> Response {
    match app.send(Event::MamPause(MamPauseEvent::Set { mode })).await {
        Ok(()) => back(next).into_response(),
        Err(response) => response,
    }
}

async fn pause(AppState(app): AppState<App>, Form(form): Form<ReturnTo>) -> Response {
    set_mam_mode(&app, MamRequestMode::Paused, form.next.as_deref()).await
}

async fn resume(AppState(app): AppState<App>, Form(form): Form<ReturnTo>) -> Response {
    set_mam_mode(&app, MamRequestMode::Enabled, form.next.as_deref()).await
}

fn rejected(app: &App, form: &SettingsForm) -> Response {
    let page = pages::settings(&app.observation(), &app.deployment, form);
    (StatusCode::UNPROCESSABLE_ENTITY, page).into_response()
}

async fn send_setting(app: &App, event: SettingsEvent) -> Response {
    match app.send(Event::Settings(event)).await {
        Ok(()) => back(Some(Location::Settings.path())).into_response(),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
struct LimitForm {
    limit: String,
}

async fn set_history_limit(AppState(app): AppState<App>, Form(form): Form<LimitForm>) -> Response {
    let limit = form
        .limit
        .trim()
        .replace([',', '_', ' '], "")
        .parse::<u32>()
        .ok()
        .and_then(|limit| HistoryLimit::try_new(limit).ok());
    match limit {
        Some(limit) => send_setting(&app, SettingsEvent::SetHistoryLimit { limit }).await,
        None => rejected(
            &app,
            &SettingsForm {
                history_limit: Some(form.limit),
                history_limit_error: Some(String::from(
                    "Enter a whole number from 100 to 1000000. The saved limit is still in use.",
                )),
                ..SettingsForm::default()
            },
        ),
    }
}

#[derive(Debug, Deserialize)]
struct ReserveForm {
    reserve: String,
}

async fn set_points_reserve(
    AppState(app): AppState<App>,
    Form(form): Form<ReserveForm>,
) -> Response {
    let reserve = form
        .reserve
        .trim()
        .replace([',', '_', ' '], "")
        .parse::<u64>()
        .ok()
        .and_then(|reserve| PointsReserve::try_new(reserve).ok());
    match reserve {
        Some(reserve) => send_setting(&app, SettingsEvent::SetPointsReserve { reserve }).await,
        None => rejected(
            &app,
            &SettingsForm {
                points_reserve: Some(form.reserve),
                points_reserve_error: Some(String::from(
                    "Enter a whole number of points up to 100000000. The saved reserve is still in use.",
                )),
                ..SettingsForm::default()
            },
        ),
    }
}

#[derive(Debug, Deserialize)]
struct LimitsForm {
    downloading: String,
    active: String,
}

async fn set_torrent_limits(
    AppState(app): AppState<App>,
    Form(form): Form<LimitsForm>,
) -> Response {
    let number = |value: &str| value.trim().parse::<u32>().ok();
    let downloading =
        number(&form.downloading).and_then(|limit| DownloadingLimit::try_new(limit).ok());
    let active = number(&form.active).and_then(|limit| ActiveLimit::try_new(limit).ok());
    let limits = match downloading.zip(active) {
        Some((downloading, active)) => {
            TorrentLimits::new(downloading, active).map_err(|error| error.to_string())
        }
        None => Err(String::from(
            "Enter whole numbers: 1 to 100 downloading, 1 to 1000 active.",
        )),
    };
    match limits {
        Ok(limits) => send_setting(&app, SettingsEvent::SetTorrentLimits { limits }).await,
        Err(error) => rejected(
            &app,
            &SettingsForm {
                max_downloading: Some(form.downloading),
                max_active: Some(form.active),
                torrent_limits_error: Some(format!("{error} The saved limits are still in use.")),
                ..SettingsForm::default()
            },
        ),
    }
}

#[derive(Debug, Deserialize)]
struct CookieForm {
    cookie: String,
}

async fn set_api_cookie(AppState(app): AppState<App>, Form(form): Form<CookieForm>) -> Response {
    match ApiCookie::parse(&form.cookie) {
        Ok(cookie) => send_setting(&app, SettingsEvent::SetApiCookie { cookie }).await,
        Err(error) => rejected(
            &app,
            &SettingsForm {
                api_cookie: Some(form.cookie),
                api_cookie_error: Some(error.to_string()),
                ..SettingsForm::default()
            },
        ),
    }
}

async fn clear_api_cookie(AppState(app): AppState<App>) -> Response {
    send_setting(&app, SettingsEvent::ClearApiCookie).await
}

#[derive(Debug, Deserialize)]
struct TopicForm {
    topic: String,
}

async fn set_ntfy_topic(AppState(app): AppState<App>, Form(form): Form<TopicForm>) -> Response {
    match NtfyTopic::parse(&form.topic) {
        Ok(topic) => send_setting(&app, SettingsEvent::SetNtfyTopic { topic }).await,
        Err(error) => rejected(
            &app,
            &SettingsForm {
                ntfy_topic: Some(form.topic),
                ntfy_topic_error: Some(error.to_string()),
                ..SettingsForm::default()
            },
        ),
    }
}

async fn clear_ntfy_topic(AppState(app): AppState<App>) -> Response {
    send_setting(&app, SettingsEvent::ClearNtfyTopic).await
}

async fn send_test_notification(AppState(app): AppState<App>) -> Response {
    match app
        .send(Event::Notification(NotificationEvent::SendTest))
        .await
    {
        Ok(()) => back(Some(Location::Settings.path())).into_response(),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
struct CheckedForm {
    made: String,
}

async fn purchase_checked(AppState(app): AppState<App>, Form(form): Form<CheckedForm>) -> Response {
    let made = match form.made.as_str() {
        "yes" => true,
        "no" => false,
        _ => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    match app
        .send(Event::Mam(MamEvent::PurchaseChecked { made }))
        .await
    {
        Ok(()) => back(Some(Location::Status.path())).into_response(),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
struct RedownloadForm {
    record: u64,
}

async fn redownload(AppState(app): AppState<App>, Form(form): Form<RedownloadForm>) -> Response {
    let Some(record) = RecordId::new(form.record) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    match app
        .send(Event::Registry(RegistryEvent::Redownload(record)))
        .await
    {
        Ok(()) => back(Some(Location::Status.path())).into_response(),
        Err(response) => response,
    }
}

#[derive(Debug, Default, Deserialize)]
struct HistoryParameters {
    kind: Option<String>,
    from: Option<String>,
    to: Option<String>,
    from_local: Option<String>,
    to_local: Option<String>,
    before: Option<i64>,
    after: Option<i64>,
    routine: Option<String>,
}

/// Parse a filter time: exact RFC 3339 from the page script, or an unconverted
/// `datetime-local` value, read as UTC, when scripts are unavailable.
fn filter_time(exact: Option<&str>, local: Option<&str>) -> Option<SystemTime> {
    fn present(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    if let Some(local) = present(local) {
        return PrimitiveDateTime::parse(
            local,
            format_description!("[year]-[month]-[day]T[hour]:[minute]"),
        )
        .ok()
        .map(|time| time.assume_utc().into());
    }
    present(exact)
        .and_then(|exact| OffsetDateTime::parse(exact, &Rfc3339).ok())
        .map(Into::into)
}

fn history_query(parameters: &HistoryParameters) -> HistoryQuery {
    HistoryQuery {
        category: CATEGORIES
            .iter()
            .map(|(value, _)| *value)
            .find(|value| parameters.kind.as_deref() == Some(*value)),
        from: filter_time(parameters.from.as_deref(), parameters.from_local.as_deref()),
        until: filter_time(parameters.to.as_deref(), parameters.to_local.as_deref()),
        routine: parameters.routine.is_some(),
        page: match (parameters.before, parameters.after) {
            (Some(before), _) => Page::Before(before),
            (None, Some(after)) => Page::After(after),
            (None, None) => Page::Newest,
        },
    }
}

async fn history_page(
    AppState(app): AppState<App>,
    Query(parameters): Query<HistoryParameters>,
) -> Markup {
    let query = history_query(&parameters);
    let database = app.deployment.database.clone();
    let reading = query.clone();
    let result = tokio::task::spawn_blocking(move || history::read_page(&database, &reading))
        .await
        .map_err(ShellError::from)
        .and_then(|read| read)
        .map_err(|error| error.to_string());
    let observation = app.observation();
    let retained = match observation.history {
        HistoryState::Open { retained, .. } => Some(retained),
        HistoryState::Closed | HistoryState::Opening(_) | HistoryState::Unavailable => None,
    };
    pages::history(&observation, &query, &result, retained)
}

/// How many notices the notifications page shows.
const NOTICES_SHOWN: u32 = 200;

async fn notifications_page(AppState(app): AppState<App>) -> Markup {
    let database = app.deployment.database.clone();
    let result =
        tokio::task::spawn_blocking(move || storage::read_notices(&database, NOTICES_SHOWN))
            .await
            .map_err(ShellError::from)
            .and_then(|read| read)
            .map_err(|error| error.to_string());
    pages::notifications(&app.observation(), &result)
}

/// What `windlass serve` runs.
#[derive(Debug)]
pub struct Serve {
    /// The `SQLite` database.
    pub database: PathBuf,
    /// Whether to create the database or resume an existing one.
    pub mode: StartupMode,
    /// Bound dashboard listeners.
    pub listeners: Vec<TcpListener>,
    /// Host names accepted in addition to the listen addresses.
    pub allowed_hosts: Vec<String>,
    /// External services to use.
    pub integrations: Integrations,
}

/// Run the core and serve the dashboard until `shutdown` completes.
///
/// Shutdown stops accepting commands, finishes outstanding storage work and
/// history, then closes connections.
///
/// # Errors
/// Returns an error if a listener cannot report its address, the core cannot
/// start, or a server or worker fails unexpectedly.
pub async fn serve(
    options: Serve,
    shutdown: impl Future<Output = ()> + Send,
) -> ShellResult<State> {
    let Serve {
        database,
        mode,
        listeners,
        allowed_hosts,
        integrations,
    } = options;
    let deployment = Deployment {
        listen: listeners
            .iter()
            .map(TcpListener::local_addr)
            .collect::<Result<_, _>>()?,
        allowed_hosts,
        database: database.clone(),
        ntfy_server: integrations
            .ntfy_server
            .as_ref()
            .map_or_else(|| String::from(super::notify::NTFY_SH), ToString::to_string),
        ntfy_relay: integrations.ntfy_relay,
        dashboard_url: integrations.dashboard_url.as_ref().map(ToString::to_string),
    };
    let enable = integrations.enable_events();
    let (runner, events, observations) = Runner::with_integrations(database, integrations);
    events
        .send(Event::Startup(StartupEvent::Start { mode }))
        .await?;
    for event in enable {
        events.send(event).await?;
    }
    let routes = router(events.clone(), observations, deployment);
    let (stop, stopped) = watch::channel(false);
    let servers: Vec<_> = listeners
        .into_iter()
        .map(|listener| {
            let mut stopped = stopped.clone();
            let routes = routes.clone();
            tokio::spawn(async move {
                axum::serve(listener, routes)
                    .with_graceful_shutdown(async move {
                        let _ = stopped.wait_for(|stop| *stop).await;
                    })
                    .await
            })
        })
        .collect();
    let runner = tokio::spawn(runner.run());
    shutdown.await;
    // The runner may already have stopped after a worker failure.
    let _ = events.shutdown().await;
    let state = runner.await??;
    stop.send_replace(true);
    for server in servers {
        server.await??;
    }
    Ok(state)
}
