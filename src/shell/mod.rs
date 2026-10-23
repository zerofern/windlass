//! Execute core actions and return timestamped observations through one event stream.

use std::{
    collections::HashMap,
    error::Error,
    net::SocketAddrV4,
    path::PathBuf,
    time::{Instant, SystemTime},
};

use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::core::{
    Action, ClockReading, Event, PortForwardingEvent, QbittorrentEvent, TimedEvent, TimerName,
};

pub mod console;
mod describe;
mod history;
mod ingress;
mod natpmp;
mod qbittorrent;
mod storage;
pub mod web;

use history::{Command, HistoryEntry, HistoryWriter};
use storage::Operation;

/// External services the shell can reach. Absent services are not used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Integrations {
    /// The qBittorrent web UI, such as `http://127.0.0.1:8080/`.
    pub qbittorrent: Option<reqwest::Url>,
    /// The NAT-PMP gateway that forwards ports, such as Proton VPN's `10.2.0.1:5351`.
    pub nat_pmp_gateway: Option<SocketAddrV4>,
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
        (
            Self {
                database,
                sender,
                receiver,
                ingress_state,
                active_workers: 0,
                latest_storage_error: None,
                history,
                qbittorrent: integrations.qbittorrent.map(qbittorrent::Client::new),
                nat_pmp_gateway: integrations.nat_pmp_gateway,
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
            Action::QbittorrentCommand { command } => {
                return self.qbittorrent(qbittorrent::Request::Command(command));
            }
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

/// Read both clocks where an event occurs.
pub(crate) fn read_clock() -> ClockReading {
    ClockReading {
        monotonic: Instant::now(),
        wall: SystemTime::now(),
    }
}
