//! Run Windlass end to end against the fake MAM server (B-0012), a fake
//! qBittorrent web UI, and a fake exit-IP service, driven over its dashboard:
//! the flows of `docs/flows.md`, tests E1 to E7.

#[path = "support/fake_mam.rs"]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the fake's items are pub(super) so the library's own tests can include it too"
)]
mod fake_mam;

use std::{
    collections::BTreeMap,
    error::Error,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{Request, StatusCode, header},
    routing::{get, post},
};
use fake_mam::{FakeMam, Host, Torrent};
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::watch, task::JoinHandle};
use tower::ServiceExt;
use windlass::{
    core::{
        Desired, Event, MamSession, PurchaseOutcome, Registry, SeedboxStatus, StartupEvent,
        StartupMode, State as CoreState,
    },
    runner::{Observation, Runner},
    shell::{
        EventSender, Integrations, MamHosts, TunnelChecks,
        web::{self, Deployment},
    },
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const HOST: &str = "127.0.0.1:8740";
const BOOK: u64 = 1001;
const GIB: u64 = 1 << 30;

/// A qBittorrent web UI that keeps the torrents Windlass adds, each half
/// downloaded and added just now.
#[derive(Debug, Default)]
struct FakeQbittorrent {
    torrents: BTreeMap<String, &'static str>,
    /// The hashes of the next torrents added, in order, as the test knows them.
    next_hashes: Vec<String>,
    added: Vec<usize>,
    stopped: Vec<String>,
    started: Vec<String>,
}

type SharedQbittorrent = Arc<Mutex<FakeQbittorrent>>;

#[derive(Deserialize)]
struct Rid {
    rid: u64,
}

#[derive(Deserialize)]
struct Hashes {
    hashes: String,
}

async fn fake_qbittorrent(fake: SharedQbittorrent) -> TestResult<SocketAddr> {
    let routes = Router::new()
        .route("/api/v2/app/version", get(|| async { "v5.2.2" }))
        .route(
            "/api/v2/app/preferences",
            get(|| async {
                axum::Json(json!({ "listen_port": 6881, "current_network_interface": "wg0" }))
            }),
        )
        .route(
            "/api/v2/sync/maindata",
            get(
                |State(fake): State<SharedQbittorrent>, Query(rid): Query<Rid>| async move {
                    let now = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|since| since.as_secs())
                        .unwrap_or_default();
                    let torrents: serde_json::Map<String, Value> = fake
                        .lock()
                        .map(|fake| fake.torrents.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(hash, state)| {
                            let fields = json!({
                                "name": "Synthetic Book", "state": state, "progress": 0.5,
                                "size": GIB, "total_size": GIB, "completed": 0, "uploaded": 0,
                                "ratio": 0.0, "seeding_time": 0, "added_on": now,
                                "completion_on": -1, "content_path": "", "save_path": "/srv"
                            });
                            (hash, fields)
                        })
                        .collect();
                    axum::Json(
                        json!({ "rid": rid.rid + 1, "full_update": true, "torrents": torrents }),
                    )
                },
            ),
        )
        .route(
            "/api/v2/torrents/add",
            post(|State(fake): State<SharedQbittorrent>, body: Bytes| async move {
                let torrent = String::from_utf8_lossy(&body).contains("d8:announce");
                match fake.lock() {
                    Ok(mut fake) if torrent => {
                        fake.added.push(body.len());
                        if !fake.next_hashes.is_empty() {
                            let hash = fake.next_hashes.remove(0);
                            fake.torrents.insert(hash, "downloading");
                        }
                        StatusCode::OK
                    }
                    _ => StatusCode::BAD_REQUEST,
                }
            }),
        )
        .route(
            "/api/v2/torrents/stop",
            post(
                |State(fake): State<SharedQbittorrent>,
                 axum::Form(form): axum::Form<Hashes>| async move {
                    if let Ok(mut fake) = fake.lock() {
                        for hash in form.hashes.split('|') {
                            fake.stopped.push(hash.to_owned());
                            fake.torrents.insert(hash.to_owned(), "stoppedDL");
                        }
                    }
                    StatusCode::OK
                },
            ),
        )
        .route(
            "/api/v2/torrents/start",
            post(
                |State(fake): State<SharedQbittorrent>,
                 axum::Form(form): axum::Form<Hashes>| async move {
                    if let Ok(mut fake) = fake.lock() {
                        for hash in form.hashes.split('|') {
                            fake.started.push(hash.to_owned());
                            fake.torrents.insert(hash.to_owned(), "downloading");
                        }
                    }
                    StatusCode::OK
                },
            ),
        )
        .with_state(fake);
    serve(routes).await
}

async fn fake_exit_ip() -> TestResult<SocketAddr> {
    serve(Router::new().route("/", get(|| async { "192.0.2.10\n" }))).await
}

async fn serve(routes: Router) -> TestResult<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move { axum::serve(listener, routes).await });
    Ok(address)
}

/// Windlass with its dashboard, wired to the fakes.
struct Windlass {
    router: Router,
    events: EventSender,
    observations: watch::Receiver<Observation>,
    runner: JoinHandle<TestResult<CoreState>>,
    _directory: tempfile::TempDir,
}

impl Windlass {
    async fn start(mam: &FakeMam, qbittorrent: SocketAddr) -> TestResult<Self> {
        Self::start_in(
            tempfile::tempdir()?,
            StartupMode::Initialise,
            mam,
            qbittorrent,
        )
        .await
    }

    /// Stop Windlass and start it again on the same storage.
    async fn restart(self, mam: &FakeMam, qbittorrent: SocketAddr) -> TestResult<Self> {
        let Self {
            events,
            runner,
            _directory: directory,
            ..
        } = self;
        events.shutdown().await?;
        let _ = runner.await??;
        let windlass = Self::start_in(directory, StartupMode::Resume, mam, qbittorrent).await?;
        windlass.ready().await?;
        Ok(windlass)
    }

    async fn start_in(
        directory: tempfile::TempDir,
        mode: StartupMode,
        mam: &FakeMam,
        qbittorrent: SocketAddr,
    ) -> TestResult<Self> {
        let path: PathBuf = directory.path().join("windlass.sqlite");
        let exit_ip = fake_exit_ip().await?;
        let integrations = Integrations {
            qbittorrent: Some(format!("http://{qbittorrent}/").parse()?),
            tunnel: Some(TunnelChecks {
                exit_ip_service: format!("http://{exit_ip}/").parse()?,
                servers: Vec::new(),
                current: None,
                unit_starter: PathBuf::from("true"),
            }),
            ntfy_server: Some("http://127.0.0.1:9/".parse()?),
            mam: Some(MamHosts {
                site: mam.site.clone(),
                tracker: mam.tracker.clone(),
            }),
            ..Integrations::default()
        };
        let enable = integrations.enable_events();
        let (runner, events, observations) = Runner::with_integrations(path.clone(), integrations);
        let runner = tokio::spawn(runner.run());
        events
            .send(Event::Startup(StartupEvent::Start { mode }))
            .await?;
        for event in enable {
            events.send(event).await?;
        }
        let router = web::router(
            events.clone(),
            observations.clone(),
            Deployment {
                listen: vec![HOST.parse::<SocketAddr>()?],
                allowed_hosts: Vec::new(),
                database: path,
                ntfy_server: String::from("http://127.0.0.1:9/"),
                ntfy_relay: None,
                dashboard_url: None,
            },
        );
        Ok(Self {
            router,
            events,
            observations,
            runner,
            _directory: directory,
        })
    }

    async fn wait(&self, predicate: impl FnMut(&Observation) -> bool) -> TestResult<Observation> {
        let mut observations = self.observations.clone();
        let seen = tokio::time::timeout(Duration::from_mins(3), async {
            observations
                .wait_for(predicate)
                .await
                .map(|seen| seen.clone())
        })
        .await;
        if let Ok(seen) = seen {
            return Ok(seen?);
        }
        let last = self.observations.borrow().mam.clone();
        Err(format!("timed out waiting; last saw {last:?}").into())
    }

    /// Wait until startup finished and settings can be changed.
    async fn ready(&self) -> TestResult {
        self.wait(|observation| {
            observation.state.mam_requests() != windlass::core::MamRequestsStatus::Starting
                && observation.settings.loaded().is_some()
        })
        .await?;
        Ok(())
    }

    async fn request(&self, request: Request<Body>) -> TestResult<(StatusCode, String)> {
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let body = response.into_body().collect().await?.to_bytes();
        Ok((status, String::from_utf8(body.to_vec())?))
    }

    async fn get(&self, uri: &str) -> TestResult<String> {
        let request = Request::get(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())?;
        Ok(self.request(request).await?.1)
    }

    async fn post(&self, uri: &str, form: &str) -> TestResult<StatusCode> {
        let request = Request::post(uri)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, format!("http://{HOST}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form.to_owned()))?;
        Ok(self.request(request).await?.0)
    }

    async fn stop(self) -> TestResult {
        self.events.shutdown().await?;
        let _ = self.runner.await??;
        Ok(())
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario follows Windlass from a pasted cookie to a download"
)]
async fn a_pasted_cookie_leads_to_a_registered_seedbox_and_a_wedged_download() -> TestResult {
    let mam = FakeMam::start().await?;
    let book = Torrent::bookmarked("Synthetic Book", GIB);
    let book_hash = book.hash();
    mam.with(|world| {
        world.torrents.insert(BOOK, book);
        // VIP ends in ten days, so Windlass renews it.
        world.account.vip_until = SystemTime::now().checked_add(Duration::from_hours(240));
        world.account.uploaded = 80 * GIB;
    });
    let qbittorrent = Arc::new(Mutex::new(FakeQbittorrent {
        next_hashes: vec![book_hash.clone()],
        ..FakeQbittorrent::default()
    }));
    let webui = fake_qbittorrent(Arc::clone(&qbittorrent)).await?;
    let windlass = Windlass::start(&mam, webui).await?;
    windlass.ready().await?;

    // Before a cookie and a resume, MAM hears nothing.
    let status = windlass.get("/").await?;
    assert!(status.contains("No API cookie"), "{status}");
    assert!(mam.journal().is_empty());
    assert_eq!(
        windlass
            .post("/settings/points-reserve", "reserve=lots")
            .await?,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        windlass
            .post("/settings/points-reserve", "reserve=20%2C000")
            .await?,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        windlass
            .post("/settings/api-cookie", "cookie=fake-session")
            .await?,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        windlass.post("/mam/resume", "next=%2F").await?,
        StatusCode::SEE_OTHER
    );

    // Windlass observes the IP, registers the seedbox, reads the account,
    // renews VIP, lists bookmarks, and downloads the bookmark with a wedge.
    let observation = windlass
        .wait(|observation| {
            matches!(observation.registry, Registry::Loaded(ref records)
                if records.records().any(|record| record.desired == Desired::Active))
                && observation
                    .mam
                    .account
                    .last_purchase
                    .as_ref()
                    .is_some_and(|purchase| {
                        matches!(purchase.outcome, Some(PurchaseOutcome::Made(_)))
                    })
        })
        .await?;
    assert!(matches!(
        observation.mam.seedbox.status,
        SeedboxStatus::Confirmed { .. }
    ));
    assert!(matches!(
        observation.mam.session,
        MamSession::Verified { .. }
    ));
    windlass
        .wait(|observation| match &observation.qbittorrent {
            windlass::core::Qbittorrent::Connected(session) => session
                .torrents
                .keys()
                .any(|hash| hash.as_str() == book_hash),
            _ => false,
        })
        .await?;
    assert_eq!(
        qbittorrent.lock().map(|fake| fake.added.len()).ok(),
        Some(1)
    );
    assert_eq!(mam.with(|world| world.account.wedges), Some(199));
    assert!(
        mam.with(|world| world.account.vip_until)
            .flatten()
            .is_some_and(|until| until > SystemTime::now() + Duration::from_hours(80 * 24))
    );

    let status = windlass.get("/").await?;
    for text in [
        "MAM access",
        "Working",
        "Confirmed for",
        "MAM account",
        "VIP, filled to 90 days",
        "Bookmarks",
        "Synthetic Book",
    ] {
        assert!(status.contains(text), "{text}: {status}");
    }
    let history = windlass.get("/history?kind=mam").await?;
    assert!(
        history.contains("Register the seedbox IP with MAM"),
        "{history}"
    );
    assert!(!history.contains("fake-session") && !history.contains("passkey-secret"));

    // Every request named Windlass, and MAM saw no broken rule.
    let journal = mam.journal();
    assert!(journal.iter().all(|request| request.user_agent.as_deref()
        == Some(concat!("Windlass/", env!("CARGO_PKG_VERSION")))));
    assert!(
        journal
            .iter()
            .any(|request| request.host == Host::Tracker
                && request.path == "/json/dynamicSeedbox.php")
    );
    assert_eq!(mam.violations(), Vec::<String>::new());
    // Windlass keeps using the cookies MAM issues.
    assert!(journal.iter().any(|request| {
        request
            .cookie
            .as_deref()
            .is_some_and(|cookie| cookie.starts_with("fake-rotated-"))
    }));

    // A pause stops every request.
    assert_eq!(
        windlass.post("/mam/pause", "next=%2F").await?,
        StatusCode::SEE_OTHER
    );
    windlass
        .wait(|observation| !observation.mam.available)
        .await?;
    let before = std::time::Instant::now();
    windlass.stop().await?;
    assert!(
        before.elapsed() < Duration::from_secs(5),
        "{:?}",
        before.elapsed()
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_session_is_reported_and_stops_requests() -> TestResult {
    let mam = FakeMam::start().await?;
    let qbittorrent = Arc::new(Mutex::new(FakeQbittorrent::default()));
    let webui = fake_qbittorrent(qbittorrent).await?;
    let windlass = Windlass::start(&mam, webui).await?;
    windlass.ready().await?;
    let _ = windlass
        .post("/settings/api-cookie", "cookie=not-a-session")
        .await?;
    let _ = windlass.post("/mam/resume", "next=%2F").await?;
    windlass
        .wait(|observation| matches!(observation.mam.session, MamSession::Rejected { .. }))
        .await?;
    let status = windlass.get("/").await?;
    assert!(status.contains("Session refused"), "{status}");
    assert!(status.contains("Not read yet"), "{status}");
    windlass.stop().await
}

/// A fake qBittorrent that will report these torrents once Windlass adds them.
fn qbittorrent_adding(hashes: Vec<String>) -> SharedQbittorrent {
    Arc::new(Mutex::new(FakeQbittorrent {
        next_hashes: hashes,
        ..FakeQbittorrent::default()
    }))
}

impl Windlass {
    /// Name an ntfy topic, paste the cookie, and resume MAM requests, as the
    /// owner does. ntfy does not answer here, so notices stay waiting.
    async fn connect(&self) -> TestResult {
        self.ready().await?;
        assert_eq!(
            self.post("/settings/ntfy-topic", "topic=windlass-test")
                .await?,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            self.post("/settings/api-cookie", "cookie=fake-session")
                .await?,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            self.post("/mam/resume", "next=%2F").await?,
            StatusCode::SEE_OTHER
        );
        Ok(())
    }

    /// Wait until qBittorrent lists this torrent.
    async fn in_qbittorrent(&self, hash: &str) -> TestResult<Observation> {
        self.wait(|observation| match &observation.qbittorrent {
            windlass::core::Qbittorrent::Connected(session) => {
                session.torrents.keys().any(|known| known.as_str() == hash)
            }
            _ => false,
        })
        .await
    }
}

/// The registry row for this MAM torrent.
fn record(observation: &Observation, mam: u64) -> Option<windlass::core::TorrentRecord> {
    match &observation.registry {
        Registry::Loaded(records) => windlass::core::MamTorrentId::new(mam)
            .and_then(|id| records.find_mam(id))
            .cloned(),
        _ => None,
    }
}

/// Whether a notice is waiting to be sent; ntfy is unreachable in these tests.
fn noticed(observation: &Observation, notice: &windlass::core::Notice) -> bool {
    let notifications = &observation.notifications;
    notifications.outbox.contains(notice)
        || matches!(&notifications.delivery, windlass::core::Delivery::Sending(sending) if sending == notice)
}

/// Requests Windlass sent to MAM for this path, with their query strings.
fn sent(mam: &FakeMam, path: &str) -> Vec<String> {
    mam.journal()
        .into_iter()
        .filter(|request| request.path == path)
        .map(|request| request.query)
        .collect()
}

/// E3: a bookmark snatched before waits for the owner, and "Download again"
/// downloads it.
#[tokio::test]
async fn a_snatched_bookmark_waits_until_the_owner_downloads_it_again() -> TestResult {
    let mam = FakeMam::start().await?;
    let mut book = Torrent::bookmarked("A Book Read Before", GIB);
    book.snatched = true;
    let hash = book.hash();
    mam.with(|world| {
        world.torrents.insert(BOOK, book);
    });
    let qbittorrent = qbittorrent_adding(vec![hash.clone()]);
    let webui = fake_qbittorrent(Arc::clone(&qbittorrent)).await?;
    let windlass = Windlass::start(&mam, webui).await?;
    windlass.connect().await?;

    let observation = windlass
        .wait(|observation| {
            record(observation, BOOK).is_some_and(|row| row.desired == Desired::Snatched)
        })
        .await?;
    let row = record(&observation, BOOK).ok_or("no row")?;
    let status = windlass.get("/").await?;
    for text in [
        "A Book Read Before",
        "Snatched before, not downloaded",
        "Download again",
    ] {
        assert!(status.contains(text), "{text}: {status}");
    }
    assert!(sent(&mam, "/tor/download.php").is_empty());

    // The owner asks for it on the dashboard.
    assert_eq!(
        windlass
            .post("/torrents/redownload", &format!("record={}", row.id.get()))
            .await?,
        StatusCode::SEE_OTHER
    );
    let observation = windlass.in_qbittorrent(&hash).await?;
    assert_eq!(
        record(&observation, BOOK).map(|row| (row.desired, row.origin)),
        Some((Desired::Active, windlass::core::Origin::Redownload))
    );
    assert_eq!(
        sent(&mam, "/tor/download.php"),
        [format!("tid={BOOK}&fl=1")]
    );
    assert_eq!(mam.violations(), Vec::<String>::new());
    windlass.stop().await
}

/// E4: a purchase whose answer is lost is settled by the owner on the
/// dashboard, and upload credit is then bought.
#[tokio::test]
async fn an_uncertain_purchase_is_settled_on_the_dashboard() -> TestResult {
    let mam = FakeMam::start().await?;
    mam.with(|world| {
        // Little upload beyond downloads, so upload credit is wanted.
        world.account.uploaded = 20 * GIB;
        world.account.points = 100_000.5;
        world.fail("/json/bonusBuy.php", fake_mam::Fault::Status(500), 1);
    });
    let webui = fake_qbittorrent(qbittorrent_adding(Vec::new())).await?;
    let windlass = Windlass::start(&mam, webui).await?;
    windlass.connect().await?;

    let outcome = |observation: &Observation| {
        observation
            .mam
            .account
            .last_purchase
            .as_ref()
            .and_then(|purchase| purchase.outcome.clone())
    };
    windlass
        .wait(|observation| outcome(observation) == Some(PurchaseOutcome::Uncertain))
        .await?;
    // MAM took points but shows no credit, so the next reading cannot tell.
    mam.with(|world| world.account.points -= 25_000.0);
    let observation = windlass
        .wait(|observation| outcome(observation) == Some(PurchaseOutcome::Unresolved))
        .await?;
    assert!(noticed(
        &observation,
        &windlass::core::Notice::PurchaseNeedsChecking {
            purchase: windlass::core::Purchase::Upload(windlass::core::UploadCredit::try_new(50)?),
        }
    ));
    let status = windlass.get("/").await?;
    assert!(status.contains("It went through"), "{status}");
    assert_eq!(sent(&mam, "/json/bonusBuy.php").len(), 1);

    // The owner checked on MAM: it did not go through, so it is bought again.
    assert_eq!(
        windlass.post("/mam/purchase/checked", "made=no").await?,
        StatusCode::SEE_OTHER
    );
    windlass
        .wait(|observation| matches!(outcome(observation), Some(PurchaseOutcome::Made(_))))
        .await?;
    assert_eq!(mam.with(|world| world.account.uploaded), Some(70 * GIB));
    assert_eq!(sent(&mam, "/json/bonusBuy.php").len(), 2);
    windlass.stop().await
}

/// E5, E6, E7: a download that is freeleech for everyone needs no wedge. Its
/// freeleech ends; after a restart, the bookmark listing (not a lookup of its
/// own) shows it, and Windlass wedges it, without registering again within
/// the hour.
#[tokio::test]
async fn a_download_whose_freeleech_ends_is_wedged_after_a_restart() -> TestResult {
    let mam = FakeMam::start().await?;
    let mut book = Torrent::bookmarked("A Free Book", GIB);
    book.free = true;
    let hash = book.hash();
    mam.with(|world| {
        world.torrents.insert(BOOK, book);
    });
    let qbittorrent = qbittorrent_adding(vec![hash.clone()]);
    let webui = fake_qbittorrent(Arc::clone(&qbittorrent)).await?;
    let windlass = Windlass::start(&mam, webui).await?;
    windlass.connect().await?;
    windlass.in_qbittorrent(&hash).await?;
    assert_eq!(sent(&mam, "/tor/download.php"), [format!("tid={BOOK}")]);
    assert_eq!(mam.with(|world| world.account.wedges), Some(200));

    // Freeleech ends while it downloads.
    mam.with(|world| {
        if let Some(book) = world.torrents.get_mut(&BOOK) {
            book.free = false;
        }
    });
    let windlass = windlass.restart(&mam, webui).await?;
    let observation = windlass
        .wait(|observation| {
            observation
                .mam
                .acquisition
                .watched
                .iter()
                .any(|(_, watch)| *watch == windlass::core::DownloadWatch::Wedged)
        })
        .await?;
    assert!(noticed(
        &observation,
        &windlass::core::Notice::WedgedMidDownload
    ));
    assert_eq!(
        sent(&mam, "/tor/download.php"),
        [format!("tid={BOOK}"), format!("tid={BOOK}&fl=1")]
    );
    assert_eq!(mam.with(|world| world.account.wedges), Some(199));
    assert_eq!(
        record(&observation, BOOK).map(|row| row.desired),
        Some(Desired::Active)
    );
    // The bookmark listing covered it: no lookup of its own.
    let searches = sent(&mam, "/tor/js/loadSearchJSONbasic.php");
    assert!(
        searches.iter().all(|query| !query.contains("tor%5Bid%5D")),
        "{searches:?}"
    );
    // The attempt saved before the restart keeps MAM's hour.
    assert_eq!(sent(&mam, "/json/dynamicSeedbox.php").len(), 1);
    assert_eq!(mam.violations(), Vec::<String>::new());
    windlass.stop().await
}

/// E5, E6, E7: a download no longer free, with no wedge left, is stopped; it
/// is no longer bookmarked, so it is looked up by itself. Once it is free
/// again, it runs again.
#[tokio::test]
async fn a_download_that_cannot_be_wedged_stops_and_resumes_when_free() -> TestResult {
    let mam = FakeMam::start().await?;
    let mut book = Torrent::bookmarked("Another Free Book", GIB);
    book.free = true;
    let hash = book.hash();
    mam.with(|world| {
        world.torrents.insert(BOOK, book);
    });
    let qbittorrent = qbittorrent_adding(vec![hash.clone()]);
    let webui = fake_qbittorrent(Arc::clone(&qbittorrent)).await?;
    let windlass = Windlass::start(&mam, webui).await?;
    windlass.connect().await?;
    windlass.in_qbittorrent(&hash).await?;

    // Freeleech ends, the owner removed the bookmark, and no wedge is left.
    let set_free = |free: bool| {
        mam.with(|world| {
            if let Some(book) = world.torrents.get_mut(&BOOK) {
                book.free = free;
                book.bookmarked = None;
            }
            world.account.wedges = 0;
        })
    };
    set_free(false);
    let windlass = windlass.restart(&mam, webui).await?;
    let observation = windlass
        .wait(|observation| {
            record(observation, BOOK).is_some_and(|row| row.desired == Desired::Held)
        })
        .await?;
    assert!(noticed(
        &observation,
        &windlass::core::Notice::DownloadNotFree { stopped: true }
    ));
    let searches = sent(&mam, "/tor/js/loadSearchJSONbasic.php");
    assert!(
        searches
            .iter()
            .any(|query| query == &format!("tor%5Bid%5D={BOOK}")),
        "{searches:?}"
    );
    windlass
        .wait(|_| {
            qbittorrent
                .lock()
                .is_ok_and(|fake| fake.stopped.contains(&hash))
        })
        .await?;

    // It is freeleech again: it runs again.
    set_free(true);
    let windlass = windlass.restart(&mam, webui).await?;
    windlass
        .wait(|observation| {
            record(observation, BOOK).is_some_and(|row| row.desired == Desired::Active)
        })
        .await?;
    windlass
        .wait(|_| {
            qbittorrent
                .lock()
                .is_ok_and(|fake| fake.started.contains(&hash))
        })
        .await?;
    assert_eq!(sent(&mam, "/tor/download.php"), [format!("tid={BOOK}")]);
    assert_eq!(mam.violations(), Vec::<String>::new());
    windlass.stop().await
}
