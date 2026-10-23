//! Translate between the qBittorrent web UI API and core events.
//!
//! Windlass reaches the web UI on loopback inside the VPN namespace, where the
//! NixOS module disables web UI authentication for local clients.

use std::time::{Duration, SystemTime};

use reqwest::{Client as HttpClient, Response, Url};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::core::{
    ByteSize, Port, Progress, QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences, Ratio,
    SyncPosition, SyncUpdate, TorrentCommand, TorrentHash, TorrentState, TorrentUpdate,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The requests the core can make of qBittorrent.
#[derive(Debug, Clone)]
pub(super) enum Request {
    Connect,
    Sync(SyncPosition),
    ReadPreferences,
    SetListenPort(Port),
    Command(TorrentCommand),
}

impl Request {
    /// The core event reporting that this request failed.
    const fn failed(&self, reason: QbittorrentFailure) -> QbittorrentEvent {
        match self {
            Self::Connect => QbittorrentEvent::ConnectFailed { reason },
            Self::Sync(_) => QbittorrentEvent::SyncFailed { reason },
            Self::ReadPreferences => QbittorrentEvent::PreferencesReadFailed { reason },
            Self::SetListenPort(_) => QbittorrentEvent::ListenPortSetFailed { reason },
            Self::Command(_) => QbittorrentEvent::CommandFailed { reason },
        }
    }
}

/// Perform a request; without a configured web UI it fails as unreachable.
pub(super) async fn execute(client: Option<&Client>, request: Request) -> QbittorrentEvent {
    let outcome = match client {
        Some(client) => client.perform(&request).await,
        None => Err(QbittorrentFailure::Unreachable),
    };
    outcome.unwrap_or_else(|reason| request.failed(reason))
}

/// A client for one qBittorrent web UI.
#[derive(Debug, Clone)]
pub(super) struct Client {
    http: HttpClient,
    base: Url,
}

type Outcome<T> = Result<T, QbittorrentFailure>;

impl Client {
    pub(super) fn new(base: Url) -> Self {
        // Building fails only if the TLS backend cannot initialise; plain HTTP
        // to loopback does not need it, so fall back to the default client.
        let http = HttpClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self { http, base }
    }

    async fn perform(&self, request: &Request) -> Outcome<QbittorrentEvent> {
        match request {
            Request::Connect => self.connect().await,
            Request::Sync(position) => self.sync(*position).await.map(QbittorrentEvent::Synced),
            Request::ReadPreferences => self
                .preferences()
                .await
                .map(QbittorrentEvent::PreferencesRead),
            Request::SetListenPort(port) => self
                .set_listen_port(*port)
                .await
                .map(|()| QbittorrentEvent::ListenPortSet),
            Request::Command(command) => self
                .command(command)
                .await
                .map(|()| QbittorrentEvent::CommandDone),
        }
    }

    fn url(&self, path: &str) -> Outcome<Url> {
        self.base
            .join(path)
            .map_err(|_| QbittorrentFailure::Malformed)
    }

    fn checked(response: Result<Response, reqwest::Error>) -> Outcome<Response> {
        let response = response.map_err(|_| QbittorrentFailure::Unreachable)?;
        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            Err(QbittorrentFailure::Rejected {
                status: status.as_u16(),
            })
        }
    }

    async fn get(&self, path: &str) -> Outcome<Response> {
        Self::checked(self.http.get(self.url(path)?).send().await)
    }

    async fn json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Outcome<T> {
        self.get(path)
            .await?
            .json()
            .await
            .map_err(|_| QbittorrentFailure::Malformed)
    }

    async fn connect(&self) -> Outcome<QbittorrentEvent> {
        let version = self
            .get("api/v2/app/version")
            .await?
            .text()
            .await
            .map_err(|_| QbittorrentFailure::Malformed)?;
        let preferences = self.preferences().await?;
        Ok(QbittorrentEvent::Connected {
            version: version.trim().to_owned(),
            preferences,
        })
    }

    async fn preferences(&self) -> Outcome<QbittorrentPreferences> {
        let raw: RawPreferences = self.json("api/v2/app/preferences").await?;
        Ok(QbittorrentPreferences {
            listen_port: raw.listen_port.and_then(Port::new),
            interface: raw
                .current_network_interface
                .filter(|interface| !interface.is_empty()),
        })
    }

    async fn sync(&self, position: SyncPosition) -> Outcome<SyncUpdate> {
        let raw: RawSync = self
            .json(&format!("api/v2/sync/maindata?rid={}", position.0))
            .await?;
        raw.into_update()
    }

    async fn post(&self, path: &str, form: &[(&str, String)]) -> Outcome<()> {
        let response = self.http.post(self.url(path)?).form(form).send().await;
        Self::checked(response).map(drop)
    }

    async fn set_listen_port(&self, port: Port) -> Outcome<()> {
        let preferences = serde_json::json!({ "listen_port": port.get() }).to_string();
        self.post("api/v2/app/setPreferences", &[("json", preferences)])
            .await
    }

    async fn command(&self, command: &TorrentCommand) -> Outcome<()> {
        let hashes = |hashes: &[TorrentHash]| {
            hashes
                .iter()
                .map(TorrentHash::as_str)
                .collect::<Vec<_>>()
                .join("|")
        };
        match command {
            TorrentCommand::Start(started) => {
                self.post("api/v2/torrents/start", &[("hashes", hashes(started))])
                    .await
            }
            TorrentCommand::Remove(removed) => {
                let form = [
                    ("hashes", hashes(removed)),
                    ("deleteFiles", String::from("true")),
                ];
                self.post("api/v2/torrents/delete", &form).await
            }
            TorrentCommand::SelectAllFiles(hash) => {
                let files: Vec<Value> = self
                    .json(&format!("api/v2/torrents/files?hash={}", hash.as_str()))
                    .await?;
                let ids = (0..files.len())
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join("|");
                let form = [
                    ("hash", hash.as_str().to_owned()),
                    ("id", ids),
                    ("priority", String::from("1")),
                ];
                self.post("api/v2/torrents/filePrio", &form).await
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawPreferences {
    listen_port: Option<u16>,
    current_network_interface: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSync {
    rid: u64,
    #[serde(default)]
    full_update: bool,
    #[serde(default)]
    torrents: Map<String, Value>,
    #[serde(default)]
    torrents_removed: Vec<String>,
}

impl RawSync {
    fn into_update(self) -> Outcome<SyncUpdate> {
        let hash = |hash: &str| TorrentHash::parse(hash).ok_or(QbittorrentFailure::Malformed);
        Ok(SyncUpdate {
            position: SyncPosition(self.rid),
            full: self.full_update,
            torrents: self
                .torrents
                .iter()
                .map(|(key, fields)| Ok((hash(key)?, torrent_update(fields))))
                .collect::<Outcome<_>>()?,
            removed: self
                .torrents_removed
                .iter()
                .map(|key| hash(key))
                .collect::<Outcome<_>>()?,
        })
    }
}

fn torrent_update(fields: &Value) -> TorrentUpdate {
    let text = |name| fields.get(name).and_then(Value::as_str);
    let bytes = |name| fields.get(name).and_then(Value::as_u64).map(ByteSize);
    // Paths are empty, and times are zero, negative, or 2^32 - 1, when not known.
    let path = |name| text(name).map(|path| Some(path.to_owned()).filter(|path| !path.is_empty()));
    let time = |name| {
        fields.get(name).and_then(Value::as_i64).map(|seconds| {
            u64::try_from(seconds)
                .ok()
                .filter(|seconds| *seconds > 0 && *seconds != u64::from(u32::MAX))
                .map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        })
    };
    TorrentUpdate {
        name: text("name").map(str::to_owned),
        state: text("state").map(torrent_state),
        progress: fields
            .get("progress")
            .and_then(Value::as_f64)
            .map(Progress::from_fraction),
        size: bytes("size"),
        total_size: bytes("total_size"),
        downloaded: bytes("completed"),
        uploaded: bytes("uploaded"),
        ratio: fields
            .get("ratio")
            .and_then(Value::as_f64)
            .map(Ratio::from_float),
        seeding_time: fields
            .get("seeding_time")
            .and_then(Value::as_u64)
            .map(Duration::from_secs),
        added_at: time("added_on"),
        completed_at: time("completion_on"),
        content_path: path("content_path"),
        save_path: path("save_path"),
    }
}

/// qBittorrent's state names, from 4.x `paused*` to 5.x `stopped*`.
pub(super) fn torrent_state(state: &str) -> TorrentState {
    match state {
        "metaDL" | "forcedMetaDL" => TorrentState::FetchingMetadata,
        "allocating" | "downloading" | "forcedDL" => TorrentState::Downloading,
        "stalledDL" => TorrentState::DownloadStalled,
        "queuedDL" => TorrentState::DownloadQueued,
        "uploading" | "forcedUP" => TorrentState::Seeding,
        "stalledUP" => TorrentState::SeedStalled,
        "queuedUP" => TorrentState::SeedQueued,
        "pausedUP" | "stoppedUP" => TorrentState::Stopped { complete: true },
        "pausedDL" | "stoppedDL" => TorrentState::Stopped { complete: false },
        "checkingUP" | "checkingDL" | "checkingResumeData" => TorrentState::Checking,
        "moving" => TorrentState::Moving,
        "missingFiles" => TorrentState::MissingFiles,
        "error" => TorrentState::Error,
        _ => TorrentState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::{ByteSize, Progress, QbittorrentFailure, TorrentState};

    use crate::core::{Port, QbittorrentEvent, SyncPosition};

    use std::error::Error;

    use crate::core::{TorrentCommand, TorrentHash};

    use super::{Client, RawSync, Request, execute, torrent_state, torrent_update};

    #[tokio::test]
    async fn requests_without_a_web_ui_fail_as_unreachable() -> Result<(), &'static str> {
        let reason = QbittorrentFailure::Unreachable;
        let port = Port::new(1).ok_or("zero port")?;
        for (request, expected) in [
            (Request::Connect, QbittorrentEvent::ConnectFailed { reason }),
            (
                Request::Sync(SyncPosition(0)),
                QbittorrentEvent::SyncFailed { reason },
            ),
            (
                Request::ReadPreferences,
                QbittorrentEvent::PreferencesReadFailed { reason },
            ),
            (
                Request::SetListenPort(port),
                QbittorrentEvent::ListenPortSetFailed { reason },
            ),
            (
                Request::Command(TorrentCommand::Start(Vec::new())),
                QbittorrentEvent::CommandFailed { reason },
            ),
        ] {
            assert_eq!(execute(None, request).await, expected);
        }
        Ok(())
    }

    #[tokio::test]
    async fn commands_to_a_closed_web_ui_fail_as_unreachable() -> Result<(), Box<dyn Error>> {
        let closed = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
        let client = Client::new(format!("http://{closed}/").parse()?);
        let hash = TorrentHash::parse(&"a".repeat(40)).ok_or("hash")?;
        for command in [
            TorrentCommand::SelectAllFiles(hash.clone()),
            TorrentCommand::Remove(vec![hash]),
        ] {
            assert_eq!(
                execute(Some(&client), Request::Command(command)).await,
                QbittorrentEvent::CommandFailed {
                    reason: QbittorrentFailure::Unreachable
                }
            );
        }
        Ok(())
    }

    #[test]
    fn unknown_times_and_paths_are_absent() {
        let update = torrent_update(&json!({
            "added_on": 0,
            "completion_on": 4_294_967_295_u64,
            "content_path": "",
            "save_path": "/srv/books"
        }));
        assert_eq!(update.added_at, Some(None));
        assert_eq!(update.completed_at, Some(None));
        assert_eq!(update.content_path, Some(None));
        assert_eq!(update.save_path, Some(Some(String::from("/srv/books"))));
    }

    #[test]
    fn states_cover_old_and_new_names() {
        assert_eq!(
            torrent_state("pausedUP"),
            TorrentState::Stopped { complete: true }
        );
        assert_eq!(
            torrent_state("stoppedDL"),
            TorrentState::Stopped { complete: false }
        );
        for (name, state) in [
            ("forcedMetaDL", TorrentState::FetchingMetadata),
            ("allocating", TorrentState::Downloading),
            ("stalledDL", TorrentState::DownloadStalled),
            ("queuedDL", TorrentState::DownloadQueued),
            ("forcedUP", TorrentState::Seeding),
            ("stalledUP", TorrentState::SeedStalled),
            ("queuedUP", TorrentState::SeedQueued),
            ("checkingResumeData", TorrentState::Checking),
            ("moving", TorrentState::Moving),
            ("missingFiles", TorrentState::MissingFiles),
            ("error", TorrentState::Error),
            ("somethingNew", TorrentState::Unknown),
        ] {
            assert_eq!(torrent_state(name), state, "{name}");
        }
    }

    #[test]
    fn partial_updates_keep_only_the_fields_present() {
        let update = torrent_update(&json!({ "progress": 0.5, "size": 10 }));
        assert_eq!(update.name, None);
        assert_eq!(update.state, None);
        assert_eq!(update.progress, Some(Progress::from_fraction(0.5)));
        assert_eq!(update.size, Some(ByteSize(10)));
    }

    #[test]
    fn invalid_hashes_are_malformed() -> Result<(), serde_json::Error> {
        let value = json!({ "rid": 3, "torrents": { "not-a-hash": { "name": "x" } } });
        let raw: RawSync = serde_json::from_value(value)?;
        assert_eq!(raw.into_update(), Err(QbittorrentFailure::Malformed));
        let value = json!({ "rid": 3, "torrents_removed": ["also-not-a-hash"] });
        let raw: RawSync = serde_json::from_value(value)?;
        assert_eq!(raw.into_update(), Err(QbittorrentFailure::Malformed));
        Ok(())
    }
}
