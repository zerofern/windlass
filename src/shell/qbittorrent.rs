//! Translate between the qBittorrent web UI API and core events.
//!
//! Windlass reaches the web UI on loopback inside the VPN namespace, where the
//! NixOS module disables web UI authentication for local clients.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use reqwest::{
    Client as HttpClient, Response, Url,
    multipart::{Form, Part},
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::core::{
    ByteSize, Completion, ContentPath, ForceStart, IdleTorrents, MamTorrentId, NetworkInterface,
    Port, Progress, QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences, QueueLimit,
    QueuePreferences, Queueing, Ratio, ReportedVersion, SavePath, SyncPosition, SyncScope,
    SyncUpdate, TorrentCommand, TorrentCount, TorrentHash, TorrentName, TorrentState,
    TorrentUpdate,
};

use super::storage;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The requests the core can make of qBittorrent.
#[derive(Debug, Clone)]
pub(super) enum Request {
    Connect,
    Sync(SyncPosition),
    ReadPreferences,
    SetListenPort(Port),
    SetQueue(QueuePreferences),
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
            Self::SetQueue(_) => QbittorrentEvent::QueueSetFailed { reason },
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
    /// Where fetched torrent files are kept.
    database: PathBuf,
}

type Outcome<T> = Result<T, QbittorrentFailure>;

impl Client {
    pub(super) fn new(base: Url, database: PathBuf) -> Self {
        // Building fails only if the TLS backend cannot initialise; plain HTTP
        // to loopback does not need it, so fall back to the default client.
        let http = HttpClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self {
            http,
            base,
            database,
        }
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
            Request::SetQueue(queue) => self
                .set_queue(*queue)
                .await
                .map(|()| QbittorrentEvent::QueueSet),
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
        let version = ReportedVersion::parse(&version).ok_or(QbittorrentFailure::Malformed)?;
        let preferences = self.preferences().await?;
        Ok(QbittorrentEvent::Connected {
            version,
            preferences,
        })
    }

    async fn preferences(&self) -> Outcome<QbittorrentPreferences> {
        let raw: RawPreferences = self.json("api/v2/app/preferences").await?;
        raw.into_preferences()
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

    async fn set_queue(&self, queue: QueuePreferences) -> Outcome<()> {
        let preferences = serde_json::json!({
            "queueing_enabled": queue.queueing == Queueing::On,
            "max_active_downloads": queue_limit(queue.max_downloading),
            "max_active_torrents": queue_limit(queue.max_active),
            "max_active_uploads": queue_limit(queue.max_seeding),
            "dont_count_slow_torrents": queue.idle == IdleTorrents::NotCounted,
        })
        .to_string();
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
            TorrentCommand::Stop(stopped) => {
                self.post("api/v2/torrents/stop", &[("hashes", hashes(stopped))])
                    .await
            }
            TorrentCommand::ForceStart(forced) => {
                let form = [("hashes", hashes(forced)), ("value", String::from("true"))];
                self.post("api/v2/torrents/setForceStart", &form).await
            }
            TorrentCommand::EndForceStart(released) => {
                let form = [
                    ("hashes", hashes(released)),
                    ("value", String::from("false")),
                ];
                self.post("api/v2/torrents/setForceStart", &form).await
            }
            TorrentCommand::Add { mam, .. } => self.add(*mam).await,
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

impl Client {
    /// Add a kept torrent file; qBittorrent starts it at once.
    async fn add(&self, mam: MamTorrentId) -> Outcome<()> {
        let database = self.database.clone();
        let file = tokio::task::spawn_blocking(move || storage::kept_torrent_file(&database, mam))
            .await
            .ok()
            .flatten()
            .ok_or(QbittorrentFailure::Malformed)?;
        let part = Part::bytes(file)
            .file_name(format!("{}.torrent", mam.get()))
            .mime_str("application/x-bittorrent")
            .map_err(|_| QbittorrentFailure::Malformed)?;
        let form = Form::new().part("torrents", part);
        let response = self
            .http
            .post(self.url("api/v2/torrents/add")?)
            .multipart(form)
            .send()
            .await;
        Self::checked(response).map(drop)
    }
}

#[derive(Debug, Deserialize)]
struct RawPreferences {
    /// 0 when qBittorrent picks a port itself.
    listen_port: Option<u16>,
    /// Empty when qBittorrent uses any interface.
    current_network_interface: Option<String>,
    queueing_enabled: Option<bool>,
    max_active_downloads: Option<i64>,
    max_active_torrents: Option<i64>,
    max_active_uploads: Option<i64>,
    dont_count_slow_torrents: Option<bool>,
}

impl RawPreferences {
    fn into_preferences(self) -> Outcome<QbittorrentPreferences> {
        let queue = self.queue()?;
        Ok(QbittorrentPreferences {
            listen_port: self.listen_port.and_then(Port::new),
            interface: self
                .current_network_interface
                .filter(|interface| !interface.is_empty())
                .map(|interface| NetworkInterface::try_new(interface).map_err(malformed))
                .transpose()?,
            queue,
        })
    }

    /// The queue, when qBittorrent reports it: all of it, or none of it.
    fn queue(&self) -> Outcome<Option<QueuePreferences>> {
        let fields = (
            self.queueing_enabled,
            self.max_active_downloads,
            self.max_active_torrents,
            self.max_active_uploads,
            self.dont_count_slow_torrents,
        );
        let (queueing, downloading, active, seeding, slow) = match fields {
            (None, None, None, None, None) => return Ok(None),
            (Some(queueing), Some(downloading), Some(active), Some(seeding), Some(slow)) => {
                (queueing, downloading, active, seeding, slow)
            }
            _ => return Err(QbittorrentFailure::Malformed),
        };
        // qBittorrent reports -1 for no limit.
        let limit = |limit: i64| match limit {
            -1 => Ok(QueueLimit::Unlimited),
            limit => u32::try_from(limit)
                .map(|limit| QueueLimit::At(TorrentCount::new(limit)))
                .map_err(|_| QbittorrentFailure::Malformed),
        };
        Ok(Some(QueuePreferences {
            queueing: if queueing {
                Queueing::On
            } else {
                Queueing::Off
            },
            max_downloading: limit(downloading)?,
            max_active: limit(active)?,
            max_seeding: limit(seeding)?,
            idle: if slow {
                IdleTorrents::NotCounted
            } else {
                IdleTorrents::Counted
            },
        }))
    }
}

/// A queue limit as qBittorrent takes it: -1 for no limit.
fn queue_limit(limit: QueueLimit) -> i64 {
    match limit {
        QueueLimit::Unlimited => -1,
        QueueLimit::At(count) => i64::from(count.get()),
    }
}

/// An update; qBittorrent leaves out what did not change, so an absent
/// `full_update` means a partial update, and absent lists mean none.
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
            scope: if self.full_update {
                SyncScope::Full
            } else {
                SyncScope::Changes
            },
            torrents: self
                .torrents
                .into_iter()
                .map(|(key, fields)| Ok((hash(&key)?, torrent_update(fields)?)))
                .collect::<Outcome<_>>()?,
            removed: self
                .torrents_removed
                .iter()
                .map(|key| hash(key))
                .collect::<Outcome<_>>()?,
        })
    }
}

/// A torrent's changed fields. A field qBittorrent leaves out is unchanged;
/// one it sends must hold a value of its type.
#[derive(Debug, Deserialize)]
struct RawTorrent {
    name: Option<String>,
    state: Option<String>,
    progress: Option<f64>,
    size: Option<u64>,
    total_size: Option<u64>,
    completed: Option<u64>,
    uploaded: Option<u64>,
    ratio: Option<f64>,
    seeding_time: Option<u64>,
    added_on: Option<i64>,
    completion_on: Option<i64>,
    content_path: Option<String>,
    save_path: Option<String>,
    force_start: Option<bool>,
}

/// Any parse error means the answer is not understood.
fn malformed<E>(_: E) -> QbittorrentFailure {
    QbittorrentFailure::Malformed
}

fn torrent_update(fields: Value) -> Outcome<TorrentUpdate> {
    let raw: RawTorrent = serde_json::from_value(fields).map_err(malformed)?;
    // Paths are empty, which no path type holds, and times are 0, -1, or
    // 2^32 - 1, when not known.
    let time = |seconds: i64| match seconds {
        -1 | 0 => Ok(None),
        seconds if seconds == i64::from(u32::MAX) => Ok(None),
        seconds => u64::try_from(seconds)
            .ok()
            .and_then(|seconds| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
            .map(Some)
            .ok_or(QbittorrentFailure::Malformed),
    };
    Ok(TorrentUpdate {
        name: raw
            .name
            .map(|name| TorrentName::try_new(name).map(Some).map_err(malformed))
            .transpose()?,
        state: raw.state.as_deref().map(torrent_state),
        progress: raw
            .progress
            .map(|progress| Progress::from_fraction(progress).ok_or(QbittorrentFailure::Malformed))
            .transpose()?,
        size: raw.size.map(ByteSize),
        total_size: raw.total_size.map(ByteSize),
        downloaded: raw.completed.map(ByteSize),
        uploaded: raw.uploaded.map(ByteSize),
        ratio: raw
            .ratio
            .map(|ratio| Ratio::from_float(ratio).ok_or(QbittorrentFailure::Malformed))
            .transpose()?,
        seeding_time: raw.seeding_time.map(Duration::from_secs),
        added_at: raw.added_on.map(time).transpose()?,
        completed_at: raw.completion_on.map(time).transpose()?,
        content_path: raw.content_path.map(|path| ContentPath::try_new(path).ok()),
        save_path: raw.save_path.map(|path| SavePath::try_new(path).ok()),
        force_start: raw.force_start.map(|forced| {
            if forced {
                ForceStart::Forced
            } else {
                ForceStart::NotForced
            }
        }),
    })
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
        "pausedUP" | "stoppedUP" => TorrentState::Stopped(Completion::Complete),
        "pausedDL" | "stoppedDL" => TorrentState::Stopped(Completion::Incomplete),
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

    use crate::core::{ByteSize, Progress, QbittorrentFailure, QueueLimit, TorrentState};

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
                Request::SetQueue(crate::core::QueuePreferences::for_limits(
                    crate::core::TorrentLimits::default(),
                )),
                QbittorrentEvent::QueueSetFailed { reason },
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

    #[test]
    fn a_queue_is_read_only_when_qbittorrent_reports_all_of_it() -> Result<(), serde_json::Error> {
        let full = json!({
            "queueing_enabled": true,
            "max_active_downloads": 2,
            "max_active_torrents": -1,
            "max_active_uploads": 4,
            "dont_count_slow_torrents": true
        });
        let raw: super::RawPreferences = serde_json::from_value(full.clone())?;
        assert_eq!(
            raw.queue(),
            Ok(Some(crate::core::QueuePreferences {
                queueing: crate::core::Queueing::On,
                max_downloading: crate::core::QueueLimit::At(crate::core::TorrentCount::new(2)),
                max_active: crate::core::QueueLimit::Unlimited,
                max_seeding: crate::core::QueueLimit::At(crate::core::TorrentCount::new(4)),
                idle: crate::core::IdleTorrents::NotCounted,
            }))
        );
        for missing in [
            "queueing_enabled",
            "max_active_downloads",
            "max_active_torrents",
            "max_active_uploads",
            "dont_count_slow_torrents",
        ] {
            let partial: serde_json::Map<String, serde_json::Value> = full
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(name, _)| *name != missing)
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            let raw: super::RawPreferences =
                serde_json::from_value(serde_json::Value::Object(partial))?;
            assert_eq!(raw.queue(), Err(QbittorrentFailure::Malformed), "{missing}");
        }
        Ok(())
    }

    /// A compile-time torrent ID: its unreachable branch is not run code.
    const MAM: crate::core::MamTorrentId = match crate::core::MamTorrentId::new(7) {
        Some(id) => id,
        None => unreachable!(),
    };

    #[tokio::test]
    async fn commands_to_a_closed_web_ui_fail_as_unreachable() -> Result<(), Box<dyn Error>> {
        // Nothing listens on the discard port.
        let client = Client::new("http://127.0.0.1:9/".parse()?, "/nonexistent".into());
        let hash = TorrentHash::parse(&"a".repeat(40)).ok_or("hash")?;
        for command in [
            TorrentCommand::SelectAllFiles(hash.clone()),
            TorrentCommand::Remove(vec![hash.clone()]),
            TorrentCommand::Stop(vec![hash.clone()]),
        ] {
            assert_eq!(
                execute(Some(&client), Request::Command(command)).await,
                QbittorrentEvent::CommandFailed {
                    reason: QbittorrentFailure::Unreachable
                }
            );
        }
        // Without a kept file there is nothing to add.
        let mam = MAM;
        assert_eq!(
            execute(
                Some(&client),
                Request::Command(TorrentCommand::Add { mam, hash })
            )
            .await,
            QbittorrentEvent::CommandFailed {
                reason: QbittorrentFailure::Malformed
            }
        );
        Ok(())
    }

    #[test]
    fn unknown_times_and_paths_are_absent() -> Result<(), QbittorrentFailure> {
        let fields = json!({
            "added_on": 0,
            "completion_on": 4_294_967_295_u64,
            "content_path": "",
            "save_path": "/srv/books"
        });
        let update = torrent_update(fields)?;
        assert_eq!(update.added_at, Some(None));
        assert_eq!(update.completed_at, Some(None));
        assert_eq!(update.content_path, Some(None));
        assert_eq!(
            update.save_path,
            Some(crate::core::SavePath::try_new(String::from("/srv/books")).ok())
        );
        Ok(())
    }

    #[test]
    fn states_cover_old_and_new_names() {
        assert_eq!(
            torrent_state("pausedUP"),
            TorrentState::Stopped(crate::core::Completion::Complete)
        );
        assert_eq!(
            torrent_state("stoppedDL"),
            TorrentState::Stopped(crate::core::Completion::Incomplete)
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
    fn partial_updates_keep_only_the_fields_present() -> Result<(), QbittorrentFailure> {
        let update = torrent_update(json!({ "progress": 0.5, "size": 10 }))?;
        assert_eq!(update.name, None);
        assert_eq!(update.state, None);
        assert_eq!(update.progress, Progress::from_fraction(0.5));
        assert_eq!(update.size, Some(ByteSize(10)));
        Ok(())
    }

    #[test]
    fn invalid_hashes_are_malformed() -> Result<(), serde_json::Error> {
        let value = json!({ "rid": 3, "torrents": { "not-a-hash": { "name": "x" } } });
        let raw: RawSync = serde_json::from_value(value)?;
        assert_eq!(raw.into_update(), Err(QbittorrentFailure::Malformed));
        let value = json!({ "rid": 3, "torrents": { "a".repeat(40): { "name": "" } } });
        let raw: RawSync = serde_json::from_value(value)?;
        assert_eq!(raw.into_update(), Err(QbittorrentFailure::Malformed));
        let value = json!({ "rid": 3, "torrents_removed": ["also-not-a-hash"] });
        let raw: RawSync = serde_json::from_value(value)?;
        assert_eq!(raw.into_update(), Err(QbittorrentFailure::Malformed));
        Ok(())
    }
    #[test]
    fn fields_qbittorrent_sends_must_hold_valid_values() {
        for fields in [
            json!({ "name": 7 }),
            json!({ "name": "" }),
            json!({ "progress": 1.5 }),
            json!({ "progress": -0.1 }),
            json!({ "ratio": -2.0 }),
            json!({ "size": -1 }),
            json!({ "added_on": -5 }),
            json!({ "completion_on": "soon" }),
            json!({ "completion_on": -7 }),
            json!({ "force_start": "yes" }),
        ] {
            assert_eq!(
                torrent_update(fields.clone()),
                Err(QbittorrentFailure::Malformed),
                "{fields}"
            );
        }
    }

    #[test]
    fn queue_limits_and_interfaces_must_be_valid() -> Result<(), serde_json::Error> {
        let queue = |downloading: i64| {
            json!({
                "queueing_enabled": false,
                "max_active_downloads": downloading,
                "max_active_torrents": 5,
                "max_active_uploads": 5,
                "dont_count_slow_torrents": false
            })
        };
        assert_eq!(super::queue_limit(QueueLimit::Unlimited), -1);
        assert_eq!(
            super::queue_limit(QueueLimit::At(crate::core::TorrentCount::new(3))),
            3
        );
        for limits in [
            queue(-2),
            json!({ "max_active_torrents": -3 }),
            json!({ "max_active_uploads": -3 }),
        ] {
            let mut fields = queue(3);
            if let (Some(fields), Some(limits)) = (fields.as_object_mut(), limits.as_object()) {
                fields.extend(limits.clone());
            }
            let raw: super::RawPreferences = serde_json::from_value(fields)?;
            assert_eq!(raw.into_preferences(), Err(QbittorrentFailure::Malformed));
        }
        let raw: super::RawPreferences = serde_json::from_value(queue(3))?;
        assert_eq!(
            raw.queue()
                .map(|queue| queue.map(|queue| (queue.queueing, queue.idle))),
            Ok(Some((
                crate::core::Queueing::Off,
                crate::core::IdleTorrents::Counted
            )))
        );
        let raw: super::RawPreferences =
            serde_json::from_value(json!({ "current_network_interface": "  " }))?;
        assert_eq!(raw.into_preferences(), Err(QbittorrentFailure::Malformed));
        let raw: super::RawPreferences =
            serde_json::from_value(json!({ "current_network_interface": "" }))?;
        assert_eq!(
            raw.into_preferences()
                .map(|preferences| preferences.interface),
            Ok(None)
        );
        Ok(())
    }
}
