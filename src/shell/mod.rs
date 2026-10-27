//! Execute core actions and return timestamped observations through one event stream.

use std::{
    collections::HashMap,
    error::Error,
    net::{SocketAddr, SocketAddrV4},
    path::PathBuf,
    time::{Instant, SystemTime},
};

use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::core::{
    Action, ClockReading, Event, MamEvent, MamFailure, MamReply, MamRequest, PortForwardingEvent,
    QbittorrentEvent, TimedEvent, TimerName, TunnelEvent, VpnServer,
};

pub mod console;
mod describe;
#[cfg(test)]
#[path = "../../tests/support/fake_mam.rs"]
mod fake_mam;
mod history;
mod ingress;
mod mam;
mod mam_reply;
mod natpmp;
mod notify;
mod qbittorrent;
mod storage;
mod torrent_file;
mod tunnel;
pub mod web;

use history::{Command, HistoryEntry, HistoryWriter};
pub use mam::MamHosts;
use storage::Operation;

/// External services the shell can reach. Absent services are not used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Integrations {
    /// The qBittorrent web UI, such as `http://127.0.0.1:8080/`.
    pub qbittorrent: Option<reqwest::Url>,
    /// The NAT-PMP gateway that forwards ports, such as Proton VPN's `10.2.0.1:5351`.
    pub nat_pmp_gateway: Option<SocketAddrV4>,
    /// Tunnel checks and server failover.
    pub tunnel: Option<TunnelChecks>,
    /// The ntfy server notices are published to; ntfy.sh when absent.
    pub ntfy_server: Option<reqwest::Url>,
    /// A relay that forwards connections to the ntfy server, used in place of
    /// the server's own address.
    pub ntfy_relay: Option<SocketAddr>,
    /// The dashboard's address as the user opens it, for links in notices.
    pub dashboard_url: Option<reqwest::Url>,
    /// MAM's hosts; Windlass does not contact MAM without them.
    pub mam: Option<MamHosts>,
}

/// How the tunnel is checked and which servers it may switch between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelChecks {
    /// A neutral service that answers with the caller's IP address as text.
    pub exit_ip_service: reqwest::Url,
    /// The configured servers, in failover order.
    pub servers: Vec<VpnServer>,
    /// The server connected at startup, if known.
    pub current: Option<VpnServer>,
    /// The program that starts server units, normally `systemctl`.
    pub unit_starter: PathBuf,
}

impl Integrations {
    /// Events that start the configured integrations once the runner is running.
    #[must_use]
    pub fn enable_events(&self) -> Vec<Event> {
        let mut events = Vec::new();
        if self.nat_pmp_gateway.is_some() {
            events.push(Event::PortForwarding(PortForwardingEvent::Enable));
        }
        if self.qbittorrent.is_some() {
            events.push(Event::Qbittorrent(QbittorrentEvent::Enable));
        }
        if let Some(tunnel) = &self.tunnel {
            events.push(Event::Tunnel(TunnelEvent::Enable {
                servers: tunnel.servers.clone(),
                current: tunnel.current.clone(),
            }));
        }
        if self.mam.is_some() {
            events.push(Event::Mam(MamEvent::Enable));
        }
        events
    }
}

pub use ingress::EventSender;

use ingress::{IngressState, Input};

pub(crate) type ShellError = Box<dyn Error + Send + Sync>;
pub(crate) type ShellResult<T> = Result<T, ShellError>;

const EVENT_QUEUE_CAPACITY: usize = 64;

#[derive(Debug)]
struct WorkerCompletion {
    event: TimedEvent,
    storage_error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Shell {
    database: PathBuf,
    sender: mpsc::Sender<Input>,
    receiver: mpsc::Receiver<Input>,
    ingress_state: watch::Sender<IngressState>,
    active_workers: usize,
    latest_storage_error: Option<String>,
    history: HistoryWriter,
    qbittorrent: Option<qbittorrent::Client>,
    nat_pmp_gateway: Option<SocketAddrV4>,
    tunnel: Option<tunnel::Client>,
    mam: Option<mam::Client>,
    notifications: notify::Channel,
    timers: HashMap<TimerName, JoinHandle<()>>,
}

impl Shell {
    /// Create the shell and start its history writer; requires a Tokio runtime.
    pub(crate) fn new(database: PathBuf, integrations: Integrations) -> (Self, EventSender) {
        let (sender, receiver) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        let history = HistoryWriter::start(database.clone(), sender.clone());
        let (ingress_state, _) = watch::channel(IngressState::Open);
        let events = EventSender {
            sender: sender.clone(),
            state: ingress_state.clone(),
        };
        let qbittorrent = integrations
            .qbittorrent
            .map(|base| qbittorrent::Client::new(base, database.clone()));
        (
            Self {
                database,
                sender,
                receiver,
                ingress_state,
                active_workers: 0,
                latest_storage_error: None,
                history,
                qbittorrent,
                nat_pmp_gateway: integrations.nat_pmp_gateway,
                tunnel: integrations
                    .tunnel
                    .map(|checks| tunnel::Client::new(checks.exit_ip_service, checks.unit_starter)),
                mam: integrations.mam.as_ref().and_then(mam::Client::new),
                notifications: notify::Channel::new(
                    integrations
                        .ntfy_server
                        .or_else(|| reqwest::Url::parse(notify::NTFY_SH).ok()),
                    integrations.ntfy_relay,
                    integrations.dashboard_url,
                ),
                timers: HashMap::new(),
            },
            events,
        )
    }

    pub(crate) async fn next_event(&mut self) -> ShellResult<Option<TimedEvent>> {
        loop {
            if *self.ingress_state.borrow() == IngressState::Closing
                && self.active_workers == 0
                && self.receiver.is_empty()
            {
                self.receiver.close();
                return Ok(None);
            }
            let Some(input) = self.receiver.recv().await else {
                return Ok(None);
            };
            match input {
                Input::Event(event) => return Ok(Some(event)),
                Input::Completed(result) => {
                    self.active_workers -= 1;
                    let completion = result?;
                    if let Some(error) = completion.storage_error {
                        self.latest_storage_error = Some(error);
                    }
                    return Ok(Some(completion.event));
                }
                Input::Shutdown => {
                    self.ingress_state.send_replace(IngressState::Closing);
                }
            }
        }
    }

    /// Record a handled event and its actions before dispatching those actions.
    pub(crate) fn record(
        &self,
        occurred_at: SystemTime,
        processed_at: SystemTime,
        event: Event,
        actions: Vec<Action>,
    ) {
        self.history.send(Command::Record(HistoryEntry {
            occurred_at,
            processed_at,
            event,
            actions,
        }));
    }

    /// Stop timers and finish writing history after the event stream has ended.
    pub(crate) async fn finish(self) -> ShellResult<()> {
        for timer in self.timers.values() {
            timer.abort();
        }
        Ok(self.history.finish().await?)
    }

    /// Execute an action concurrently; its result returns as an event.
    pub(crate) fn dispatch(&mut self, action: Action) {
        let operation = match action {
            Action::OpenHistory => return self.track(|history| history.send(Command::Open)),
            Action::TrimHistory { keep } => {
                return self.track(|history| history.send(Command::Trim(keep)));
            }
            Action::StartTimer { name, id, deadline } => {
                let sender = self.sender.clone();
                let timer = tokio::spawn(async move {
                    tokio::time::sleep_until(deadline.at.into()).await;
                    let event = TimedEvent {
                        occurred_at: read_clock(),
                        event: Event::TimerExpired { name, id },
                    };
                    let _ = sender.send(Input::Event(event)).await;
                });
                // Replacing a timer makes the previous instance ineffective.
                if let Some(previous) = self.timers.insert(name, timer) {
                    previous.abort();
                }
                return;
            }
            Action::RequestPortMapping { lifetime } => {
                let gateway = self.nat_pmp_gateway;
                return self.spawn(async move {
                    Event::PortForwarding(natpmp::map_ports(gateway, lifetime).await)
                });
            }
            Action::ConnectQbittorrent => return self.qbittorrent(qbittorrent::Request::Connect),
            Action::SyncQbittorrent { position } => {
                return self.qbittorrent(qbittorrent::Request::Sync(position));
            }
            Action::ReadQbittorrentPreferences => {
                return self.qbittorrent(qbittorrent::Request::ReadPreferences);
            }
            Action::SetQbittorrentListenPort { port } => {
                return self.qbittorrent(qbittorrent::Request::SetListenPort(port));
            }
            Action::SetQbittorrentQueue { queue } => {
                return self.qbittorrent(qbittorrent::Request::SetQueue(queue));
            }
            Action::QbittorrentCommand { command } => {
                return self.qbittorrent(qbittorrent::Request::Command(command));
            }
            Action::CheckExitIp => return self.tunnel(tunnel::Request::CheckExitIp),
            Action::SwitchVpnServer { server } => {
                return self.tunnel(tunnel::Request::Switch(server));
            }
            Action::SendNotification { topic, push } => {
                let channel = self.notifications.clone();
                return self.spawn(async move {
                    Event::Notification(notify::send(&channel, &topic, &push).await)
                });
            }
            Action::ReadNoticeLog => Operation::ReadNoticeLog,
            Action::SaveNotice {
                entry,
                forget_through,
            } => Operation::SaveNotice(entry, forget_through),
            Action::MamRequest { request, cookie } => {
                let client = self.mam.clone();
                let database = self.database.clone();
                return self.spawn(async move {
                    let answer = mam::perform(client.as_ref(), &request, cookie.as_ref()).await;
                    let result = keep(&database, &request, answer.result, answer.file).await;
                    Event::Mam(MamEvent::Answered {
                        result,
                        rotated: answer.rotated,
                    })
                });
            }
            Action::WritePointsReserve { reserve } => Operation::WritePointsReserve(reserve),
            Action::WriteTorrentLimits { limits } => Operation::WriteTorrentLimits(limits),
            Action::WriteRotatedApiCookie { cookie } => Operation::WriteRotatedApiCookie(cookie),
            Action::ReadMamState => Operation::ReadMamState,
            Action::SaveRegistrationAttempt { at } => Operation::SaveRegistrationAttempt(at),
            Action::SaveReminder { at } => Operation::SaveReminder(at),
            Action::WriteNtfyTopic { topic } => Operation::WriteNtfyTopic(topic),
            Action::ReadLastVpnSwitch => Operation::ReadLastVpnSwitch,
            Action::SaveVpnSwitch { switch } => Operation::SaveVpnSwitch(switch),
            Action::ReadTorrentRegistry => Operation::ReadTorrentRegistry,
            Action::SaveTorrents { records } => Operation::SaveTorrents(records),
            Action::OpenStorage { mode } => Operation::OpenStorage(mode),
            Action::ReadMamRequestMode => Operation::ReadMamRequestMode,
            Action::WriteMamRequestMode { mode } => Operation::WriteMamRequestMode(mode),
            Action::DeleteMamRequestMode => Operation::DeleteMamRequestMode,
            Action::ReadSettings => Operation::ReadSettings,
            Action::WriteHistoryLimit { limit } => Operation::WriteHistoryLimit(limit),
            Action::WriteApiCookie { cookie } => Operation::WriteApiCookie(cookie),
        };
        self.store(operation);
    }

    /// Count work whose completion arrives through the event stream.
    fn track(&mut self, start: impl FnOnce(&HistoryWriter)) {
        self.active_workers += 1;
        start(&self.history);
    }

    fn spawn(&mut self, work: impl Future<Output = Event> + Send + 'static) {
        self.active_workers += 1;
        let sender = self.sender.clone();
        let _worker = tokio::spawn(async move {
            let event = work.await;
            let completion = WorkerCompletion {
                event: TimedEvent {
                    occurred_at: read_clock(),
                    event,
                },
                storage_error: None,
            };
            // The runner can already have stopped after an unrelated I/O failure.
            let _ = sender.send(Input::Completed(Ok(completion))).await;
        });
    }

    fn qbittorrent(&mut self, request: qbittorrent::Request) {
        let client = self.qbittorrent.clone();
        self.spawn(async move {
            Event::Qbittorrent(qbittorrent::execute(client.as_ref(), request).await)
        });
    }

    fn tunnel(&mut self, request: tunnel::Request) {
        let client = self.tunnel.clone();
        self.spawn(async move { Event::Tunnel(tunnel::perform(client.as_ref(), request).await) });
    }

    fn store(&mut self, operation: Operation) {
        self.active_workers += 1;
        let path = self.database.clone();
        let sender = self.sender.clone();
        let _worker = tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let result = storage::execute(&path, operation);
                let occurred_at = read_clock();
                let (event, storage_error) = match result {
                    Ok(event) => (event, None),
                    Err(failure) => (failure.event, Some(failure.detail)),
                };
                WorkerCompletion {
                    event: TimedEvent { occurred_at, event },
                    storage_error,
                }
            })
            .await;
            // The runner can already have stopped after an unrelated I/O failure.
            let _ = sender.send(Input::Completed(result)).await;
        });
    }

    pub(crate) const fn latest_storage_error(&self) -> Option<&String> {
        self.latest_storage_error.as_ref()
    }
}

/// Keep a fetched torrent file for qBittorrent; a file that cannot be kept
/// makes the fetch fail.
async fn keep(
    database: &std::path::Path,
    request: &MamRequest,
    result: Result<MamReply, MamFailure>,
    file: Option<Vec<u8>>,
) -> Result<MamReply, MamFailure> {
    let (MamRequest::FetchTorrent { id, .. }, Ok(MamReply::Fetched(fetched)), Some(file)) =
        (request, &result, file)
    else {
        return result;
    };
    let (database, id, hash) = (database.to_path_buf(), *id, fetched.hash.clone());
    let kept = tokio::task::spawn_blocking(move || {
        storage::keep_torrent_file(&database, id, &hash, &file).is_ok()
    })
    .await
    .unwrap_or(false);
    if kept {
        result
    } else {
        Err(MamFailure::NotKept)
    }
}

/// Read both clocks where an event occurs.
pub(crate) fn read_clock() -> ClockReading {
    ClockReading {
        monotonic: Instant::now(),
        wall: SystemTime::now(),
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, path::PathBuf, time::SystemTime};

    use crate::core::{
        Action, DeliveryFailure, Event, Notice, NotificationEvent, NtfyTopic, SettingsEvent,
        TunnelEvent, VpnServer, VpnSwitch,
    };

    use super::{Integrations, Shell, TunnelChecks};

    #[tokio::test]
    async fn switch_and_notification_actions_report_their_outcome()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let server = VpnServer::try_new("two")?;
        let topic = NtfyTopic::parse("alerts")?;
        // Storage is never opened, so saves fail, and nothing listens for notices.
        let (mut shell, _events) = Shell::new(
            PathBuf::from("/nonexistent/windlass.sqlite"),
            Integrations {
                tunnel: Some(TunnelChecks {
                    exit_ip_service: "http://127.0.0.1:9/".parse()?,
                    servers: vec![server.clone()],
                    current: None,
                    unit_starter: "true".into(),
                }),
                ntfy_server: Some("http://127.0.0.1:9/".parse()?),
                ..Integrations::default()
            },
        );
        let save = Action::SaveVpnSwitch {
            switch: VpnSwitch {
                server: server.clone(),
                at: SystemTime::UNIX_EPOCH,
            },
        };
        for (action, expected) in [
            (
                Action::SwitchVpnServer { server },
                Event::Tunnel(TunnelEvent::Switched),
            ),
            (save, Event::Tunnel(TunnelEvent::SwitchSaveFailed)),
            (
                Action::SendNotification {
                    topic: topic.clone(),
                    push: crate::core::Push {
                        entry: None,
                        notice: Notice::Test,
                    },
                },
                Event::Notification(NotificationEvent::DeliveryFailed {
                    reason: DeliveryFailure::Unreachable,
                }),
            ),
            (
                Action::WriteNtfyTopic { topic: Some(topic) },
                Event::Settings(SettingsEvent::NtfyTopicWriteFailed),
            ),
        ] {
            shell.dispatch(action);
            let event = shell.next_event().await?.map(|event| event.event);
            assert_eq!(event, Some(expected));
        }
        shell.finish().await?;
        Ok(())
    }

    /// A compile-time torrent ID: its unreachable branch is not run code.
    const KEPT: crate::core::MamTorrentId = match crate::core::MamTorrentId::new(5) {
        Some(id) => id,
        None => unreachable!(),
    };

    #[tokio::test]
    async fn mam_actions_report_their_outcome() -> Result<(), Box<dyn Error + Send + Sync>> {
        use crate::core::{ApiCookie, MamEvent, MamFailure, MamRequest, PointsReserve};

        let fake = super::fake_mam::FakeMam::start().await?;
        fake.with(|world| {
            world
                .torrents
                .insert(5, super::fake_mam::Torrent::bookmarked("Kept", 1024));
        });
        let cookie = ApiCookie::parse("fake-session")?;
        // Storage is never opened, so saves fail and a fetched file cannot be kept.
        let (mut shell, _events) = Shell::new(
            PathBuf::from("/nonexistent/windlass.sqlite"),
            Integrations {
                mam: Some(super::MamHosts {
                    site: fake.site.clone(),
                    tracker: fake.tracker.clone(),
                }),
                ..Integrations::default()
            },
        );
        let fetch = MamRequest::FetchTorrent {
            id: KEPT,
            wedge: crate::core::Wedge::Keep,
        };
        for (action, expected) in [
            (
                Action::MamRequest {
                    request: fetch,
                    cookie: Some(cookie.clone()),
                },
                MamEvent::Answered {
                    result: Err(MamFailure::NotKept),
                    rotated: fake
                        .with(|world| world.newest_cookie())
                        .flatten()
                        .map(|_| ApiCookie::parse("fake-rotated-1"))
                        .transpose()?,
                },
            ),
            (Action::ReadMamState, MamEvent::StateReadFailed),
            (
                Action::SaveRegistrationAttempt {
                    at: SystemTime::UNIX_EPOCH,
                },
                MamEvent::RegistrationAttemptSaveFailed,
            ),
            (
                Action::SaveReminder {
                    at: SystemTime::UNIX_EPOCH,
                },
                MamEvent::ReminderSaveFailed,
            ),
        ] {
            shell.dispatch(action);
            let event = shell.next_event().await?.map(|event| event.event);
            assert_eq!(event, Some(Event::Mam(expected)));
        }
        for (action, expected) in [
            (
                Action::WritePointsReserve {
                    reserve: PointsReserve::default(),
                },
                SettingsEvent::PointsReserveWriteFailed,
            ),
            (
                Action::WriteRotatedApiCookie { cookie },
                SettingsEvent::RotatedApiCookieWriteFailed,
            ),
        ] {
            shell.dispatch(action);
            let event = shell.next_event().await?.map(|event| event.event);
            assert_eq!(event, Some(Event::Settings(expected)));
        }
        shell.finish().await?;
        Ok(())
    }
}
