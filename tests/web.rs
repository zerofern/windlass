//! Exercise the dashboard routes, history storage, and server lifecycle with
//! real `SQLite` files and a running core.

use std::{
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rusqlite::Connection;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{oneshot, watch},
    task::JoinHandle,
};
use tower::ServiceExt;
use windlass::{
    core::{
        Event, HistoryState, MamPauseEvent, MamRequestMode, MamRequestsStatus, SettingsState,
        StartupEvent, StartupMode, State, Trim,
    },
    runner::{Observation, Runner},
    shell::{
        EventSender,
        web::{self, Deployment},
    },
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const HOST: &str = "127.0.0.1:8740";

struct Dashboard {
    router: Router,
    events: EventSender,
    observations: watch::Receiver<Observation>,
    runner: JoinHandle<TestResult<State>>,
    path: PathBuf,
    _directory: Option<tempfile::TempDir>,
}

impl Dashboard {
    async fn start(mode: StartupMode) -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("windlass.sqlite");
        Self::start_at(Some(directory), path, mode).await
    }

    /// Start on `path`, deleting `directory` when the dashboard is dropped.
    async fn start_at(
        directory: Option<tempfile::TempDir>,
        path: PathBuf,
        mode: StartupMode,
    ) -> TestResult<Self> {
        let (runner, events, observations) = Runner::new(path.clone());
        let runner = tokio::spawn(runner.run());
        events
            .send(Event::Startup(StartupEvent::Start { mode }))
            .await?;
        let router = web::router(
            events.clone(),
            observations.clone(),
            Deployment {
                listen: vec![HOST.parse::<SocketAddr>()?],
                allowed_hosts: vec![String::from("windlass.example.ts.net")],
                database: path.clone(),
            },
        );
        let dashboard = Self {
            router,
            events,
            observations,
            runner,
            path,
            _directory: directory,
        };
        dashboard
            .wait(|observation| {
                observation.state == State::Failed
                    || !matches!(
                        observation.settings,
                        SettingsState::Unread | SettingsState::Reading
                    ) && !matches!(
                        observation.history,
                        HistoryState::Closed | HistoryState::Opening(_)
                    ) && observation.state.mam_requests() != MamRequestsStatus::Starting
            })
            .await?;
        Ok(dashboard)
    }

    async fn wait(&self, predicate: impl FnMut(&Observation) -> bool) -> TestResult<Observation> {
        let mut observations = self.observations.clone();
        let observation =
            tokio::time::timeout(Duration::from_secs(10), observations.wait_for(predicate))
                .await??;
        Ok(observation.clone())
    }

    async fn request(&self, request: Request<Body>) -> TestResult<(StatusCode, String, String)> {
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = response.into_body().collect().await?.to_bytes();
        Ok((status, location, String::from_utf8(body.to_vec())?))
    }

    async fn get(&self, uri: &str) -> TestResult<(StatusCode, String)> {
        let request = Request::get(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())?;
        let (status, _, body) = self.request(request).await?;
        Ok((status, body))
    }

    async fn post(&self, uri: &str, form: &str) -> TestResult<(StatusCode, String, String)> {
        let request = Request::post(uri)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, format!("http://{HOST}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form.to_owned()))?;
        self.request(request).await
    }

    async fn history_count(&self) -> TestResult<i64> {
        let path = self.path.clone();
        Ok(tokio::task::spawn_blocking(move || {
            Connection::open(path)?.query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))
        })
        .await??)
    }

    /// Wait until the history writer has stored at least `count` entries.
    async fn wait_for_rows(&self, count: i64) -> TestResult {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.history_count().await? < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            TestResult::Ok(())
        })
        .await?
    }

    async fn stop(self) -> TestResult<State> {
        self.events.shutdown().await?;
        tokio::time::timeout(Duration::from_secs(10), self.runner).await??
    }
}

fn settings_value(path: &Path, column: &str) -> TestResult<Option<String>> {
    let connection = Connection::open(path)?;
    Ok(connection.query_row(
        &format!("SELECT CAST({column} AS TEXT) FROM settings WHERE singleton = 1"),
        [],
        |row| row.get(0),
    )?)
}

#[tokio::test]
async fn a_fresh_instance_starts_paused_and_asks_for_setup() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    let (status, body) = dashboard.get("/").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("MAM requests: <strong>Paused</strong>"),
        "{body}"
    );
    assert!(body.contains("Resume MAM requests"));
    assert!(body.contains("Set up MAM access"));
    assert!(body.contains("Not configured"));
    let response = dashboard
        .router
        .clone()
        .oneshot(
            Request::get("/static/app.css")
                .header(header::HOST, HOST)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/css; charset=utf-8"
    );
    assert!(
        response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()?
            .contains("default-src 'self'")
    );
    assert_eq!(response.headers()[header::REFERRER_POLICY], "same-origin");
    assert_eq!(
        dashboard.get("/favicon.ico").await?.0,
        StatusCode::NO_CONTENT
    );
    let (status, script) = dashboard.get("/static/app.js").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(script.contains("EventSource"));
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn requests_from_other_hosts_and_sites_are_refused() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    for host in ["attacker.example", "127.0.0.1.attacker.example:8740"] {
        let request = Request::get("/")
            .header(header::HOST, host)
            .body(Body::empty())?;
        assert_eq!(
            dashboard.request(request).await?.0,
            StatusCode::MISDIRECTED_REQUEST
        );
    }
    let request = Request::get("/").body(Body::empty())?;
    assert_eq!(
        dashboard.request(request).await?.0,
        StatusCode::MISDIRECTED_REQUEST
    );
    let request = Request::get("/")
        .header(header::HOST, "windlass.example.ts.net:8740")
        .body(Body::empty())?;
    assert_eq!(dashboard.request(request).await?.0, StatusCode::OK);
    let request = Request::post("/mam/resume")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "http://attacker.example")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("next=/"))?;
    assert_eq!(dashboard.request(request).await?.0, StatusCode::FORBIDDEN);
    let request = Request::post("/mam/resume")
        .header(header::HOST, HOST)
        .header("sec-fetch-site", "cross-site")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("next=/"))?;
    assert_eq!(dashboard.request(request).await?.0, StatusCode::FORBIDDEN);
    let state = dashboard.stop().await?;
    assert_eq!(state.mam_requests(), MamRequestsStatus::Paused);
    Ok(())
}

#[tokio::test]
async fn pause_and_resume_are_confirmed_after_saving() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    let (status, location, _) = dashboard.post("/mam/resume", "next=/settings").await?;
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, "/settings")
    );
    dashboard
        .wait(|observation| observation.state.mam_requests() == MamRequestsStatus::Running)
        .await?;
    let (_, body) = dashboard.get("/").await?;
    assert!(body.contains("MAM requests: <strong>Running</strong>"));
    assert!(body.contains("Pause MAM requests"));
    let (_, location, _) = dashboard
        .post("/mam/pause", "next=//attacker.example")
        .await?;
    assert_eq!(
        location, "/",
        "only dashboard pages are valid return targets"
    );
    dashboard
        .wait(|observation| observation.state.mam_requests() == MamRequestsStatus::Paused)
        .await?;
    let state = dashboard.stop().await?;
    assert_eq!(state.mam_requests(), MamRequestsStatus::Paused);
    Ok(())
}

#[tokio::test]
async fn header_states_cover_pending_and_failed_saves() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    let path = dashboard.path.clone();
    tokio::task::spawn_blocking(move || {
        Connection::open(path)?.execute_batch(
            "CREATE TRIGGER reject_write BEFORE INSERT ON mam_configuration
             BEGIN SELECT RAISE(ABORT, 'write rejected'); END;",
        )
    })
    .await??;
    dashboard.post("/mam/resume", "next=/").await?;
    dashboard
        .wait(|observation| observation.state.mam_requests() == MamRequestsStatus::ResumeNotSaved)
        .await?;
    let (_, body) = dashboard.get("/").await?;
    assert!(body.contains("Paused, resume not saved"), "{body}");
    assert!(body.contains("Try resuming again"));
    assert!(body.contains("Latest storage error"));
    assert!(body.contains("write rejected"));
    Connection::open(&dashboard.path)?.execute_batch("DROP TRIGGER reject_write;")?;
    dashboard.post("/mam/pause", "next=/").await?;
    dashboard
        .wait(|observation| observation.state.mam_requests() == MamRequestsStatus::Paused)
        .await?;
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn settings_are_validated_saved_and_secrets_never_shown() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    let (status, _, body) = dashboard
        .post("/settings/history-limit", "limit=50")
        .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("Enter a whole number from 100 to 1000000"));
    assert!(
        body.contains("value=\"50\""),
        "the rejected value is shown again"
    );
    let (status, location, _) = dashboard
        .post("/settings/history-limit", "limit=20%2C000")
        .await?;
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, "/settings")
    );
    dashboard
        .wait(|observation| {
            observation
                .settings
                .history_limit()
                .is_some_and(|limit| limit.into_inner() == 20_000)
        })
        .await?;
    assert_eq!(
        settings_value(&dashboard.path, "history_limit")?,
        Some(String::from("20000"))
    );

    let (status, _, body) = dashboard
        .post("/settings/api-cookie", "cookie=has+space")
        .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("cookie-error"));
    assert!(!body.contains("has space"));
    let (status, _, _) = dashboard
        .post("/settings/api-cookie", "cookie=secret-value%3D")
        .await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    dashboard
        .wait(|observation| {
            observation
                .settings
                .loaded()
                .is_some_and(|settings| settings.usable_api_cookie().is_some())
        })
        .await?;
    assert_eq!(
        settings_value(&dashboard.path, "api_cookie")?,
        Some(String::from("secret-value="))
    );
    let (_, page) = dashboard.get("/settings").await?;
    assert!(page.contains("Configured"));
    assert!(page.contains("Replace"));
    assert!(page.contains("Clear"));
    assert!(page.contains("windlass.example.ts.net"));
    assert!(!page.contains("secret-value"));
    let (_, status_page) = dashboard.get("/").await?;
    assert!(!status_page.contains("Set up MAM access"));
    let (_, history) = dashboard.get("/history?kind=settings").await?;
    assert!(history.contains("API cookie replaced from the dashboard (value not recorded)"));
    assert!(!history.contains("secret-value"));
    let stored: String = Connection::open(&dashboard.path)?.query_row(
        "SELECT group_concat(event || actions) FROM history",
        [],
        |row| row.get(0),
    )?;
    assert!(!stored.contains("secret-value"));

    let (status, _, _) = dashboard.post("/settings/api-cookie/clear", "").await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    dashboard
        .wait(|observation| {
            observation
                .settings
                .loaded()
                .is_some_and(|settings| settings.api_cookie.saved.is_none())
        })
        .await?;
    assert_eq!(settings_value(&dashboard.path, "api_cookie")?, None);
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn history_records_every_step_and_pages_through_it() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    let pause = Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    });
    for _ in 0..60 {
        dashboard.events.send(pause.clone()).await?;
    }
    dashboard.wait_for_rows(65).await?;
    let (status, body) = dashboard.get("/history").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Pause of MAM requests requested"), "{body}");
    assert!(body.contains("Older"));
    assert!(!body.contains(">Newer<"));
    let older = body
        .split("href=\"/history?before=")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .ok_or("missing older link")?;
    let (_, older_page) = dashboard.get(&format!("/history?before={older}")).await?;
    assert!(older_page.contains("Startup requested (initialise new storage)"));
    assert!(older_page.contains("ACTION</span>Open storage (initialise new storage)"));
    assert!(older_page.contains("Newer"));
    let (_, newer_page) = dashboard.get("/history?after=1").await?;
    assert!(newer_page.contains("Storage opened"));

    let (_, startup) = dashboard.get("/history?kind=startup").await?;
    assert!(startup.contains("Storage opened"));
    assert!(!startup.contains("Pause of MAM requests requested"));
    let (_, future) = dashboard
        .get("/history?from=2999-01-01T00%3A00%3A00Z&kind=mam")
        .await?;
    assert!(future.contains("No entries match."));
    assert!(future.contains("Clear filters"));
    let (_, local) = dashboard
        .get("/history?kind=startup&from_local=2000-01-01T00%3A00&to_local=2999-01-01T00%3A00&from=ignored")
        .await?;
    assert!(local.contains("Storage opened"));
    let (_, paged) = dashboard
        .get("/history?kind=mam&from=2000-01-01T00%3A00%3A00%2B01%3A00&to=2999-01-01T00%3A00%3A00Z&before=1000")
        .await?;
    assert!(paged.contains("kind=mam&amp;from=") || paged.contains("No entries match."));
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn history_is_trimmed_to_the_configured_limit() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    let dashboard = Dashboard::start_at(None, path.clone(), StartupMode::Initialise).await?;
    dashboard
        .post("/settings/history-limit", "limit=100")
        .await?;
    dashboard
        .wait(|observation| {
            observation
                .settings
                .history_limit()
                .is_some_and(|limit| limit.into_inner() == 100)
        })
        .await?;
    let pause = Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    });
    for _ in 0..150 {
        dashboard.events.send(pause.clone()).await?;
    }
    dashboard
        .wait(|observation| {
            matches!(
                observation.history,
                HistoryState::Open { retained, trim: Trim::Idle } if retained.get() <= 100
            ) && observation
                .last_event
                .as_ref()
                .is_some_and(|event| matches!(event.event, Event::History(_)))
        })
        .await?;
    let (_, body) = dashboard.get("/history?kind=history").await?;
    assert!(
        body.contains("Oldest history removed; 90 entries kept"),
        "{body}"
    );
    let (_, status) = dashboard.get("/").await?;
    assert!(status.contains("of 100 entries kept"));
    let observations = dashboard.observations.clone();
    dashboard.stop().await?;
    let stored: i64 =
        Connection::open(&path)?.query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))?;
    let HistoryState::Open { retained, .. } = observations.borrow().history else {
        return Err("history is not open".into());
    };
    assert!(stored <= 100, "{stored} entries stored");
    assert_eq!(
        u64::try_from(stored)?,
        retained.get(),
        "the core's count matches storage"
    );
    Ok(())
}

#[tokio::test]
async fn gap_entries_name_the_missing_count() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    Connection::open(&dashboard.path)?.execute_batch(
        "CREATE TRIGGER reject_history BEFORE INSERT ON history
         WHEN NEW.summary LIKE 'Pause of MAM requests%'
         BEGIN SELECT RAISE(ABORT, 'history rejected'); END;",
    )?;
    let pause = Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    });
    for _ in 0..3 {
        dashboard.events.send(pause.clone()).await?;
    }
    dashboard
        .post("/settings/history-limit", "limit=500")
        .await?;
    let (_, body) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = dashboard.get("/history?kind=history").await?;
            if page.1.contains("History could not be saved") {
                return TestResult::Ok(page);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(
        body.contains("entries are missing, processed from"),
        "{body}"
    );
    let (_, all) = dashboard.get("/history?kind=mam").await?;
    assert!(!all.contains("Pause of MAM requests requested"));
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn missing_history_storage_is_shown_as_unavailable() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    Dashboard::start_at(None, path.clone(), StartupMode::Initialise)
        .await?
        .stop()
        .await?;
    Connection::open(&path)?.execute_batch("DROP TABLE history;")?;
    let dashboard = Dashboard::start_at(None, path, StartupMode::Resume).await?;
    assert_eq!(
        dashboard.observations.borrow().history,
        HistoryState::Unavailable
    );
    let (_, body) = dashboard.get("/").await?;
    assert!(body.contains("Unavailable: history could not be opened"));
    let (_, history) = dashboard.get("/history").await?;
    assert!(history.contains("History could not be read"));
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn older_storage_is_upgraded_and_newer_storage_is_refused() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    Connection::open(&path)?.execute_batch(
        "CREATE TABLE mam_configuration (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            request_mode TEXT NOT NULL CHECK (request_mode IN ('paused', 'enabled'))
         );
         INSERT INTO mam_configuration VALUES (1, 'enabled');",
    )?;
    let dashboard = Dashboard::start_at(None, path.clone(), StartupMode::Resume).await?;
    let observation = dashboard.observations.borrow().clone();
    assert_eq!(observation.state.mam_requests(), MamRequestsStatus::Running);
    assert!(matches!(observation.history, HistoryState::Open { .. }));
    dashboard.stop().await?;

    let newer = tempfile::tempdir()?;
    let newer_path = newer.path().join("windlass.sqlite");
    Connection::open(&newer_path)?.execute_batch(
        "CREATE TABLE mam_configuration (singleton INTEGER PRIMARY KEY, request_mode TEXT);
         PRAGMA user_version = 99;",
    )?;
    let dashboard = Dashboard::start_at(Some(newer), newer_path, StartupMode::Resume).await?;
    assert_eq!(dashboard.observations.borrow().state, State::Failed);
    let (_, body) = dashboard.get("/").await?;
    assert!(body.contains("Blocked: startup failed"));
    assert!(body.contains("newer Windlass"));
    let (_, settings) = dashboard.get("/settings").await?;
    assert!(settings.contains("Loading…"));
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn unreadable_settings_are_reported() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    Dashboard::start_at(None, path.clone(), StartupMode::Initialise)
        .await?
        .stop()
        .await?;
    Connection::open(&path)?
        .execute_batch("INSERT INTO settings(singleton, history_limit) VALUES (1, 5);")?;
    let dashboard = Dashboard::start_at(None, path.clone(), StartupMode::Resume).await?;
    assert_eq!(
        dashboard.observations.borrow().settings,
        SettingsState::ReadFailed
    );
    let (_, body) = dashboard.get("/settings").await?;
    assert!(body.contains("Settings could not be read"));
    dashboard.stop().await?;
    Connection::open(&path)?
        .execute_batch("UPDATE settings SET history_limit = NULL, api_cookie = 'bad cookie';")?;
    let dashboard = Dashboard::start_at(None, path, StartupMode::Resume).await?;
    assert_eq!(
        dashboard.observations.borrow().settings,
        SettingsState::ReadFailed
    );
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn live_updates_stream_the_current_state() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    let response = dashboard
        .router
        .clone()
        .oneshot(
            Request::get("/live?page=/history")
                .header(header::HOST, HOST)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(10), body.frame())
        .await?
        .ok_or("stream ended")??;
    let chunk = String::from_utf8(frame.into_data().map_err(|_| "not data")?.to_vec())?;
    assert!(chunk.starts_with("event: live"), "{chunk}");
    assert!(chunk.contains("data-live=\"header\""));
    assert!(chunk.contains("value=\"/history\""));
    assert!(chunk.contains("data-live=\"api-cookie-status\""));
    dashboard.stop().await?;
    // The stream ends once the core stops.
    let rest = tokio::time::timeout(Duration::from_secs(10), body.collect()).await?;
    assert!(rest.is_ok());
    Ok(())
}

#[tokio::test]
async fn commands_after_shutdown_are_refused() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    dashboard.events.shutdown().await?;
    let (status, _, body) = dashboard.post("/mam/resume", "next=/").await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("shutting down"));
    let (status, _, _) = dashboard.post("/settings/api-cookie/clear", "").await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    tokio::time::timeout(Duration::from_secs(10), dashboard.runner).await???;
    Ok(())
}

#[tokio::test]
async fn the_server_answers_over_tcp_and_shuts_down_cleanly() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(web::serve(
        path.clone(),
        StartupMode::Initialise,
        vec![listener],
        Vec::new(),
        async move {
            let _ = stopped.await;
        },
    ));
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream
        .write_all(
            format!("GET /settings HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains(&address.to_string()),
        "listen addresses are shown"
    );
    let _ = stop.send(());
    let state = tokio::time::timeout(Duration::from_secs(10), server).await???;
    assert_eq!(state.mam_requests(), MamRequestsStatus::Paused);
    assert!(path.exists());
    Ok(())
}

#[tokio::test]
async fn a_failed_trim_is_shown_and_not_retried() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    Connection::open(&dashboard.path)?.execute_batch(
        "CREATE TRIGGER reject_trim BEFORE DELETE ON history
         BEGIN SELECT RAISE(ABORT, 'trim rejected'); END;",
    )?;
    dashboard
        .post("/settings/history-limit", "limit=100")
        .await?;
    let pause = Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    });
    for _ in 0..120 {
        dashboard.events.send(pause.clone()).await?;
    }
    dashboard
        .wait(|observation| {
            matches!(
                observation.history,
                HistoryState::Open {
                    trim: Trim::Failed,
                    ..
                }
            )
        })
        .await?;
    let (_, body) = dashboard.get("/").await?;
    assert!(body.contains("removing the oldest failed and resumes after a restart"));
    assert!(body.contains("trim rejected"));
    dashboard.stop().await?;
    Ok(())
}

#[tokio::test]
async fn failed_settings_saves_are_reported() -> TestResult {
    let dashboard = Dashboard::start(StartupMode::Initialise).await?;
    Connection::open(&dashboard.path)?.execute_batch(
        "CREATE TRIGGER reject_settings BEFORE INSERT ON settings
         BEGIN SELECT RAISE(ABORT, 'settings rejected'); END;",
    )?;
    dashboard
        .post("/settings/history-limit", "limit=200")
        .await?;
    dashboard
        .post("/settings/api-cookie", "cookie=value")
        .await?;
    dashboard
        .wait(|observation| {
            observation.settings.loaded().is_some_and(|settings| {
                settings.history_limit.status() == windlass::core::SettingStatus::NotSaved
                    && settings.api_cookie.status() == windlass::core::SettingStatus::NotSaved
            })
        })
        .await?;
    let (_, body) = dashboard.get("/settings").await?;
    assert!(body.contains("Not saved"), "{body}");
    assert!(body.contains("Change not saved; no cookie is used"));
    assert!(
        body.contains("value=\"200\""),
        "the requested value is shown"
    );
    dashboard.stop().await?;
    Ok(())
}
