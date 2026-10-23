//! Run the core and shell against a fake qBittorrent web UI and a fake NAT-PMP
//! gateway on loopback.

use std::{
    error::Error,
    net::{SocketAddr, SocketAddrV4},
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
        Event, MamApproval, MappingFailure, Port, PortForwarding, Qbittorrent, QbittorrentFailure,
        StartupEvent, StartupMode, State as CoreState, TorrentState,
    },
    runner::{Observation, Runner},
    shell::Integrations,
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Debug, Default)]
struct FakeQbittorrent {
    listen_port: u16,
    port_changes: Vec<u16>,
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
    let port = fake.lock().map_or(0, |fake| fake.listen_port);
    Json(json!({ "listen_port": port, "current_network_interface": "wg0" }))
}

async fn set_preferences(State(fake): State<Shared>, Form(form): Form<Preferences>) -> StatusCode {
    let port = serde_json::from_str::<Value>(&form.json)
        .ok()
        .and_then(|value| value.get("listen_port").and_then(Value::as_u64))
        .and_then(|port| u16::try_from(port).ok());
    match (port, fake.lock()) {
        (Some(port), Ok(mut fake)) => {
            fake.listen_port = port;
            fake.port_changes.push(port);
            StatusCode::OK
        }
        _ => StatusCode::BAD_REQUEST,
    }
}

async fn maindata(Query(rid): Query<Rid>) -> Json<Value> {
    Json(match rid.rid {
        0 => json!({
            "rid": 1,
            "full_update": true,
            "torrents": { HASH: {
                "name": "A Synthetic Audiobook",
                "state": "stalledUP",
                "progress": 1.0,
                "size": 1_073_741_824_u64
            } }
        }),
        rid => json!({ "rid": rid + 1 }),
    })
}

async fn fake_qbittorrent(fake: Shared) -> TestResult<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let routes = Router::new()
        .route("/api/v2/app/version", get(version))
        .route("/api/v2/app/preferences", get(preferences))
        .route("/api/v2/app/setPreferences", post(set_preferences))
        .route("/api/v2/sync/maindata", get(maindata))
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

struct Running {
    events: windlass::shell::EventSender,
    observations: watch::Receiver<Observation>,
    runner: tokio::task::JoinHandle<Result<CoreState, Box<dyn Error + Send + Sync>>>,
    _directory: tempfile::TempDir,
}

async fn run(integrations: Integrations) -> TestResult<Running> {
    let directory = tempfile::tempdir()?;
    let enable = integrations.enable_events();
    let (runner, events, observations) =
        Runner::with_integrations(directory.path().join("windlass.sqlite"), integrations);
    let runner = tokio::spawn(runner.run());
    events
        .send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Initialise,
        }))
        .await?;
    for event in enable {
        events.send(event).await?;
    }
    Ok(Running {
        events,
        observations,
        runner,
        _directory: directory,
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
}

#[tokio::test]
async fn qbittorrent_listens_on_the_forwarded_port() -> TestResult {
    let fake = Arc::new(Mutex::new(FakeQbittorrent {
        listen_port: 6881,
        port_changes: Vec::new(),
    }));
    let webui = fake_qbittorrent(Arc::clone(&fake)).await?;
    let gateway = fake_gateway(Ok(41000)).await?;
    let running = run(Integrations {
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
    assert_eq!(session.preferences.interface.as_deref(), Some("wg0"));
    let torrent = session.torrents.values().next().ok_or("no torrent")?;
    assert_eq!(torrent.name, "A Synthetic Audiobook");
    assert_eq!(torrent.state, TorrentState::SeedStalled);
    assert_eq!(
        fake.lock().map(|fake| fake.port_changes.clone()).ok(),
        Some(vec![41000])
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
