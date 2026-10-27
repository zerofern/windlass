//! Run the core and shell against a fake qBittorrent web UI and a fake NAT-PMP
//! gateway on loopback.

use std::{
    collections::BTreeMap,
    error::Error,
    net::{SocketAddr, SocketAddrV4},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::UdpSocket, sync::watch};
use windlass::{
    core::{
        ByteSize, Desired, Event, LastSwitch, MamApproval, MappingFailure, Port, PortForwarding,
        Protection, Qbittorrent, QbittorrentFailure, Ratio, Registry, StartupEvent, StartupMode,
        State as CoreState, TorrentHash, TorrentState, VpnServer,
    },
    runner::{Observation, Runner},
    shell::{Integrations, TunnelChecks},
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Debug, Clone)]
struct FakeTorrent {
    state: &'static str,
    size: u64,
    total_size: u64,
    /// When it completed, in Unix seconds; -1 when unknown.
    completion_on: i64,
    seeding_time: u64,
    force_start: bool,
}

#[derive(Debug, Default)]
struct FakeQbittorrent {
    listen_port: u16,
    port_changes: Vec<u16>,
    /// Queue preferences, as qBittorrent names them; none when it reports none.
    queue: Option<serde_json::Map<String, Value>>,
    torrents: BTreeMap<String, FakeTorrent>,
    commands: Vec<String>,
}

impl FakeQbittorrent {
    fn with(torrents: &[(&str, FakeTorrent)]) -> Shared {
        Arc::new(Mutex::new(Self {
            listen_port: 6881,
            torrents: torrents
                .iter()
                .map(|(hash, torrent)| ((*hash).to_owned(), torrent.clone()))
                .collect(),
            ..Self::default()
        }))
    }
}

const fn complete(state: &'static str) -> FakeTorrent {
    FakeTorrent {
        state,
        size: 1_073_741_824,
        total_size: 1_073_741_824,
        completion_on: -1,
        seeding_time: 3600,
        force_start: false,
    }
}

type Shared = Arc<Mutex<FakeQbittorrent>>;

#[derive(Deserialize)]
struct Rid {
    rid: u64,
}

#[derive(Deserialize)]
struct Preferences {
    json: String,
}

async fn version() -> &'static str {
    "v5.2.2"
}

async fn preferences(State(fake): State<Shared>) -> Json<Value> {
    let (port, queue) = fake
        .lock()
        .map(|fake| (fake.listen_port, fake.queue.clone()))
        .unwrap_or_default();
    let mut preferences = json!({ "listen_port": port, "current_network_interface": "wg0" });
    if let (Some(preferences), Some(queue)) = (preferences.as_object_mut(), queue) {
        preferences.extend(queue);
    }
    Json(preferences)
}

async fn set_preferences(State(fake): State<Shared>, Form(form): Form<Preferences>) -> StatusCode {
    let changes = serde_json::from_str::<serde_json::Map<String, Value>>(&form.json);
    let (Ok(changes), Ok(mut fake)) = (changes, fake.lock()) else {
        return StatusCode::BAD_REQUEST;
    };
    if let Some(port) = changes
        .get("listen_port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
    {
        fake.listen_port = port;
        fake.port_changes.push(port);
    } else if let Some(queue) = &mut fake.queue {
        queue.extend(changes.clone());
        let command = format!("queue {}", Value::Object(changes));
        fake.commands.push(command);
    } else {
        return StatusCode::BAD_REQUEST;
    }
    StatusCode::OK
}

/// Every answer is a full list, which qBittorrent may send at any time.
async fn maindata(State(fake): State<Shared>, Query(rid): Query<Rid>) -> Json<Value> {
    let torrents = fake
        .lock()
        .map(|fake| fake.torrents.clone())
        .unwrap_or_default();
    let torrents: serde_json::Map<String, Value> = torrents
        .into_iter()
        .map(|(hash, torrent)| {
            let fields = json!({
                "name": "A Synthetic Audiobook",
                "state": torrent.state,
                "progress": 1.0,
                "size": torrent.size,
                "total_size": torrent.total_size,
                "completed": torrent.size,
                "uploaded": 2048,
                "ratio": 1.5,
                "seeding_time": torrent.seeding_time,
                "added_on": 1_700_000_000,
                "completion_on": torrent.completion_on,
                "force_start": torrent.force_start,
                "content_path": "/srv/books/A Synthetic Audiobook",
                "save_path": "/srv/books"
            });
            (hash, fields)
        })
        .collect();
    Json(json!({ "rid": rid.rid + 1, "full_update": true, "torrents": torrents }))
}

#[derive(Deserialize)]
struct Hashes {
    hashes: String,
    #[serde(rename = "deleteFiles")]
    delete_files: Option<String>,
}

#[derive(Deserialize)]
struct FilePriority {
    hash: String,
    id: String,
    priority: String,
}

#[derive(Deserialize)]
struct Hash {
    hash: String,
}

fn record(fake: &Shared, change: impl FnOnce(&mut FakeQbittorrent) -> String) {
    if let Ok(mut fake) = fake.lock() {
        let command = change(&mut fake);
        fake.commands.push(command);
    }
}

async fn start(State(fake): State<Shared>, Form(form): Form<Hashes>) {
    record(&fake, |fake| {
        for hash in form.hashes.split('|') {
            if let Some(torrent) = fake.torrents.get_mut(hash) {
                torrent.state = "downloading";
            }
        }
        format!("start {}", form.hashes)
    });
}

#[derive(Deserialize)]
struct ForceStart {
    hashes: String,
    value: bool,
}

async fn set_force_start(State(fake): State<Shared>, Form(form): Form<ForceStart>) {
    record(&fake, |fake| {
        for hash in form.hashes.split('|') {
            if let Some(torrent) = fake.torrents.get_mut(hash) {
                torrent.force_start = form.value;
            }
        }
        format!("forceStart {} {}", form.hashes, form.value)
    });
}

async fn delete(State(fake): State<Shared>, Form(form): Form<Hashes>) {
    record(&fake, |fake| {
        for hash in form.hashes.split('|') {
            fake.torrents.remove(hash);
        }
        format!(
            "delete {} {}",
            form.hashes,
            form.delete_files.unwrap_or_default()
        )
    });
}

async fn files(Query(hash): Query<Hash>) -> Json<Value> {
    Json(json!([{ "index": 0, "name": hash.hash }, { "index": 1, "name": "cover.jpg" }]))
}

async fn file_priority(State(fake): State<Shared>, Form(form): Form<FilePriority>) {
    record(&fake, |fake| {
        if let Some(torrent) = fake.torrents.get_mut(&form.hash) {
            torrent.size = torrent.total_size;
        }
        format!("filePrio {} {} {}", form.hash, form.id, form.priority)
    });
}

async fn fake_qbittorrent(fake: Shared) -> TestResult<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let routes = Router::new()
        .route("/api/v2/app/version", get(version))
        .route("/api/v2/app/preferences", get(preferences))
        .route("/api/v2/app/setPreferences", post(set_preferences))
        .route("/api/v2/sync/maindata", get(maindata))
        .route("/api/v2/torrents/start", post(start))
        .route("/api/v2/torrents/setForceStart", post(set_force_start))
        .route("/api/v2/torrents/delete", post(delete))
        .route("/api/v2/torrents/files", get(files))
        .route("/api/v2/torrents/filePrio", post(file_priority))
        .with_state(fake);
    tokio::spawn(async move { axum::serve(listener, routes).await });
    Ok(address)
}

/// Answer NAT-PMP mapping requests with `port`, or with a result code.
async fn fake_gateway(answer: Result<u16, u16>) -> TestResult<SocketAddrV4> {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let SocketAddr::V4(address) = socket.local_addr()? else {
        return Err("not IPv4".into());
    };
    tokio::spawn(async move {
        let mut buffer = [0_u8; 64];
        while let Ok((length, from)) = socket.recv_from(&mut buffer).await {
            let opcode = buffer.get(1).copied().unwrap_or_default();
            if length != 12 {
                continue;
            }
            let (result, port) = match answer {
                Ok(port) => (0_u16, port),
                Err(code) => (code, 0),
            };
            let mut reply = vec![0, opcode + 128];
            reply.extend(result.to_be_bytes());
            reply.extend(1_u32.to_be_bytes());
            reply.extend(0_u16.to_be_bytes());
            reply.extend(port.to_be_bytes());
            reply.extend(60_u32.to_be_bytes());
            let _ = socket.send_to(&reply, from).await;
        }
    });
    Ok(address)
}

fn database(directory: &Path) -> std::path::PathBuf {
    directory.join("windlass.sqlite")
}

struct Running {
    events: windlass::shell::EventSender,
    observations: watch::Receiver<Observation>,
    runner: tokio::task::JoinHandle<Result<CoreState, Box<dyn Error + Send + Sync>>>,
    directory: tempfile::TempDir,
}

async fn run(integrations: Integrations) -> TestResult<Running> {
    run_in(tempfile::tempdir()?, StartupMode::Initialise, integrations).await
}

async fn run_in(
    directory: tempfile::TempDir,
    mode: StartupMode,
    integrations: Integrations,
) -> TestResult<Running> {
    let enable = integrations.enable_events();
    let (runner, events, observations) =
        Runner::with_integrations(database(directory.path()), integrations);
    let runner = tokio::spawn(runner.run());
    events
        .send(Event::Startup(StartupEvent::Start { mode }))
        .await?;
    for event in enable {
        events.send(event).await?;
    }
    Ok(Running {
        events,
        observations,
        runner,
        directory,
    })
}

impl Running {
    async fn wait(&self, predicate: impl FnMut(&Observation) -> bool) -> TestResult<Observation> {
        let mut observations = self.observations.clone();
        let observation =
            tokio::time::timeout(Duration::from_secs(20), observations.wait_for(predicate))
                .await??;
        Ok(observation.clone())
    }

    async fn stop(self) -> TestResult<CoreState> {
        self.events.shutdown().await?;
        tokio::time::timeout(Duration::from_secs(20), self.runner).await??
    }

    /// Stop, keeping the storage directory.
    async fn stop_keeping(self) -> TestResult<tempfile::TempDir> {
        self.events.shutdown().await?;
        let _ = tokio::time::timeout(Duration::from_secs(20), self.runner).await??;
        Ok(self.directory)
    }
}

#[tokio::test]
async fn qbittorrent_listens_on_the_forwarded_port() -> TestResult {
    let fake = FakeQbittorrent::with(&[(HASH, complete("stalledUP"))]);
    let webui = fake_qbittorrent(Arc::clone(&fake)).await?;
    let gateway = fake_gateway(Ok(41000)).await?;
    let running = run(Integrations {
        tunnel: None,
        ntfy_server: None,
        ntfy_relay: None,
        dashboard_url: None,
        mam: None,
        qbittorrent: Some(format!("http://{webui}/").parse()?),
        nat_pmp_gateway: Some(gateway),
    })
    .await?;
    let forwarded = Port::new(41000);
    let observation = running
        .wait(|observation| match &observation.qbittorrent {
            Qbittorrent::Connected(session) => {
                session.preferences.listen_port == forwarded && !session.torrents.is_empty()
            }
            _ => false,
        })
        .await?;
    assert_eq!(observation.port_forwarding.forwarded_port(), forwarded);
    let Qbittorrent::Connected(session) = &observation.qbittorrent else {
        return Err("qBittorrent is not connected".into());
    };
    assert_eq!(session.approval, MamApproval::Recommended);
    assert_eq!(
        session.preferences.interface,
        windlass::core::NetworkInterface::try_new("wg0").ok()
    );
    let torrent = session.torrents.values().next().ok_or("no torrent")?;
    assert_eq!(
        torrent.name,
        windlass::core::TorrentName::try_new("A Synthetic Audiobook").ok()
    );
    assert_eq!(torrent.state, TorrentState::SeedStalled);
    assert_eq!(
        fake.lock().map(|fake| fake.port_changes.clone()).ok(),
        Some(vec![41000])
    );
    running.stop().await?;
    Ok(())
}

#[tokio::test]
async fn the_queue_follows_the_limits_and_unsatisfied_seeds_always_run() -> TestResult {
    const SATISFIED: &str = "89abcdef0123456789abcdef0123456789abcdef";
    let unsatisfied = FakeTorrent {
        completion_on: 1_700_000_600,
        ..complete("queuedUP")
    };
    let satisfied = FakeTorrent {
        completion_on: 1_700_000_600,
        seeding_time: 100 * 3600,
        force_start: true,
        ..complete("forcedUP")
    };
    let fake = FakeQbittorrent::with(&[(HASH, unsatisfied), (SATISFIED, satisfied)]);
    if let Ok(mut fake) = fake.lock() {
        // qBittorrent's own defaults.
        fake.queue = json!({
            "queueing_enabled": false,
            "max_active_downloads": 3,
            "max_active_torrents": 5,
            "max_active_uploads": -1,
            "dont_count_slow_torrents": false
        })
        .as_object()
        .cloned();
    }
    let webui = fake_qbittorrent(Arc::clone(&fake)).await?;
    let running = run(Integrations {
        tunnel: None,
        ntfy_server: None,
        ntfy_relay: None,
        dashboard_url: None,
        mam: None,
        qbittorrent: Some(format!("http://{webui}/").parse()?),
        nat_pmp_gateway: None,
    })
    .await?;
    let wanted = windlass::core::QueuePreferences {
        queueing: windlass::core::Queueing::On,
        max_downloading: windlass::core::QueueLimit::At(windlass::core::TorrentCount::new(5)),
        max_active: windlass::core::QueueLimit::At(windlass::core::TorrentCount::new(10)),
        max_seeding: windlass::core::QueueLimit::At(windlass::core::TorrentCount::new(10)),
        idle: windlass::core::IdleTorrents::NotCounted,
    };
    running
        .wait(|observation| match &observation.qbittorrent {
            Qbittorrent::Connected(session) => {
                let forced = |hash: &str| {
                    TorrentHash::parse(hash)
                        .and_then(|hash| session.torrents.get(&hash))
                        .map(|torrent| torrent.force_start)
                };
                session.preferences.queue == Some(wanted)
                    && forced(HASH) == Some(windlass::core::ForceStart::Forced)
                    && forced(SATISFIED) == Some(windlass::core::ForceStart::NotForced)
            }
            _ => false,
        })
        .await?;
    let commands = fake
        .lock()
        .map(|fake| fake.commands.clone())
        .unwrap_or_default();
    assert!(
        commands.iter().any(|command| command.starts_with("queue ")
            && command.contains("\"max_active_downloads\":5")
            && command.contains("\"max_active_uploads\":10")),
        "{commands:?}"
    );
    assert!(
        commands.contains(&format!("forceStart {HASH} true")),
        "{commands:?}"
    );
    assert!(
        commands.contains(&format!("forceStart {SATISFIED} false")),
        "{commands:?}"
    );
    running.stop().await?;
    Ok(())
}

#[tokio::test]
async fn unavailable_services_are_reported_and_retried() -> TestResult {
    // Nothing listens on these: an unused TCP port and a gateway that refuses.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?
    };
    let gateway = fake_gateway(Err(3)).await?;
    let running = run(Integrations {
        tunnel: None,
        ntfy_server: None,
        ntfy_relay: None,
        dashboard_url: None,
        mam: None,
        qbittorrent: Some(format!("http://{closed}/").parse()?),
        nat_pmp_gateway: Some(gateway),
    })
    .await?;
    let observation = running
        .wait(|observation| {
            matches!(
                observation.qbittorrent,
                Qbittorrent::Unreachable {
                    reason: QbittorrentFailure::Unreachable,
                    ..
                }
            ) && matches!(
                observation.port_forwarding,
                PortForwarding::Enabled {
                    last_failure: Some(MappingFailure::Refused { result_code: 3 }),
                    ..
                }
            )
        })
        .await?;
    assert_eq!(observation.port_forwarding.forwarded_port(), None);
    // The retry timer fires and the gateway is asked again.
    running
        .wait(|observation| {
            matches!(
                observation.port_forwarding,
                PortForwarding::Enabled { failures: 2.., .. }
            )
        })
        .await?;
    running.stop().await?;
    Ok(())
}

/// A web UI that answers its version but fails other requests as given.
async fn broken_qbittorrent(
    preferences: &'static str,
    maindata: StatusCode,
) -> TestResult<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let routes = Router::new()
        .route("/api/v2/app/version", get(version))
        .route(
            "/api/v2/app/preferences",
            get(move || async move { preferences }),
        )
        .route(
            "/api/v2/sync/maindata",
            get(move || async move { maindata }),
        );
    tokio::spawn(async move { axum::serve(listener, routes).await });
    Ok(address)
}

#[tokio::test]
async fn rejected_and_malformed_answers_are_distinguished() -> TestResult {
    for (preferences, maindata, expected) in [
        (
            "{\"listen_port\": 6881}",
            StatusCode::FORBIDDEN,
            QbittorrentFailure::Rejected { status: 403 },
        ),
        ("not json", StatusCode::OK, QbittorrentFailure::Malformed),
    ] {
        let webui = broken_qbittorrent(preferences, maindata).await?;
        let running = run(Integrations {
            tunnel: None,
            ntfy_server: None,
            ntfy_relay: None,
            dashboard_url: None,
            mam: None,
            qbittorrent: Some(format!("http://{webui}/").parse()?),
            nat_pmp_gateway: None,
        })
        .await?;
        running
            .wait(|observation| {
                matches!(observation.qbittorrent, Qbittorrent::Unreachable { reason, .. } if reason == expected)
            })
            .await?;
        running.stop().await?;
    }
    Ok(())
}

#[tokio::test]
async fn silent_gateways_time_out() -> TestResult {
    // A bound socket that never answers.
    let silent = UdpSocket::bind("127.0.0.1:0").await?;
    let running = run(Integrations {
        tunnel: None,
        ntfy_server: None,
        ntfy_relay: None,
        dashboard_url: None,
        mam: None,
        qbittorrent: None,
        nat_pmp_gateway: Some(match silent.local_addr()? {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => return Err("not IPv4".into()),
        }),
    })
    .await?;
    running
        .wait(|observation| {
            matches!(
                observation.port_forwarding,
                PortForwarding::Enabled {
                    last_failure: Some(MappingFailure::NoResponse),
                    ..
                }
            )
        })
        .await?;
    assert_eq!(
        running.observations.borrow().qbittorrent,
        Qbittorrent::Disabled
    );
    running.stop().await?;
    Ok(())
}

const PARTIAL: &str = "1111111111111111111111111111111111111111";
const UNWANTED: &str = "2222222222222222222222222222222222222222";

#[tokio::test]
async fn qbittorrent_is_brought_to_match_the_registry() -> TestResult {
    // A first run adopts the unwanted torrent; the owner then marks it removed.
    let fake = FakeQbittorrent::with(&[(UNWANTED, complete("stalledUP"))]);
    let webui = fake_qbittorrent(Arc::clone(&fake)).await?;
    let integrations = Integrations {
        tunnel: None,
        ntfy_server: None,
        ntfy_relay: None,
        dashboard_url: None,
        mam: None,
        qbittorrent: Some(format!("http://{webui}/").parse()?),
        nat_pmp_gateway: None,
    };
    let running = run(integrations.clone()).await?;
    running
        .wait(|observation| match &observation.registry {
            Registry::Loaded(records) => records.records().count() == 1,
            _ => false,
        })
        .await?;
    let directory = running.stop_keeping().await?;
    rusqlite::Connection::open(database(directory.path()))?
        .execute_batch("UPDATE torrents SET desired = 'removed';")?;

    if let Ok(mut fake) = fake.lock() {
        fake.torrents.insert(
            PARTIAL.to_owned(),
            FakeTorrent {
                state: "stoppedDL",
                size: 5,
                total_size: 10,
                ..complete("stoppedDL")
            },
        );
    }
    let running = run_in(directory, StartupMode::Resume, integrations).await?;
    let partial = TorrentHash::parse(PARTIAL).ok_or("hash")?;
    let observation = running
        .wait(|observation| match &observation.qbittorrent {
            Qbittorrent::Connected(session) => session
                .torrents
                .get(&partial)
                .is_some_and(|torrent| torrent.state == TorrentState::Downloading),
            _ => false,
        })
        .await?;
    assert_eq!(
        fake.lock().map(|fake| fake.commands.clone()).ok(),
        Some(vec![
            format!("delete {UNWANTED} true"),
            format!("filePrio {PARTIAL} 0|1 1"),
            format!("start {PARTIAL}"),
        ])
    );
    let Registry::Loaded(records) = &observation.registry else {
        return Err("registry not loaded".into());
    };
    let unwanted = TorrentHash::parse(UNWANTED).ok_or("hash")?;
    assert_eq!(
        records.find(&unwanted).map(|row| row.desired),
        Some(Desired::Removed)
    );
    let seen = records
        .find(&partial)
        .and_then(|row| row.observation.clone())
        .ok_or("partial torrent not observed")?;
    assert_eq!(seen.torrent.total_size, ByteSize(10));
    assert_eq!(seen.torrent.uploaded, ByteSize(2048));
    assert_eq!(seen.torrent.ratio, Ratio(1500));
    assert_eq!(seen.torrent.seeding_time, Duration::from_hours(1));
    assert_eq!(seen.torrent.completed_at, None);
    assert!(seen.torrent.added_at.is_some());
    assert_eq!(
        seen.torrent.save_path,
        windlass::core::SavePath::try_new("/srv/books").ok()
    );
    let directory = running.stop_keeping().await?;
    let rows: i64 = rusqlite::Connection::open(database(directory.path()))?.query_row(
        "SELECT count(*) FROM torrents WHERE observed_save_path = '/srv/books'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(rows, 2, "both rows and their observations are saved");
    Ok(())
}

#[tokio::test]
async fn the_tunnel_is_checked_at_startup() -> TestResult {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let service = listener.local_addr()?;
    let routes = Router::new().route("/ip", get(|| async { "192.0.2.7\n" }));
    tokio::spawn(async move { axum::serve(listener, routes).await });
    let one = VpnServer::try_new("one")?;
    let running = run(Integrations {
        tunnel: Some(TunnelChecks {
            exit_ip_service: format!("http://{service}/ip").parse()?,
            servers: vec![one.clone(), VpnServer::try_new("two")?],
            current: Some(one.clone()),
            unit_starter: "false".into(),
        }),
        ..Integrations::default()
    })
    .await?;
    let observation =
        running
            .wait(|observation| {
                observation.tunnel.last_switch == LastSwitch::Known(None)
                    && observation.tunnel.watch.as_ref().is_some_and(|watch| {
                        matches!(watch.protection, Protection::Available { .. })
                    })
            })
            .await?;
    let watch = observation.tunnel.watch.ok_or("no tunnel checks")?;
    assert_eq!(watch.current, Some(one));
    assert!(matches!(
        watch.protection,
        Protection::Available { exit_ip, .. } if exit_ip.to_string() == "192.0.2.7"
    ));
    running.stop().await?;
    Ok(())
}
