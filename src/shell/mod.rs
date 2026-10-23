//! Execute core actions and return timestamped observations through one event stream.

use std::{
    error::Error,
    path::PathBuf,
    time::{Instant, SystemTime},
};

use tokio::sync::{mpsc, watch};

use crate::core::{Action, ClockReading, Event, TimedEvent};

pub mod console;
mod describe;
mod history;
mod ingress;
mod storage;
pub mod web;

use history::{Command, HistoryEntry, HistoryWriter};

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
}

impl Shell {
    /// Create the shell and start its history writer; requires a Tokio runtime.
    pub(crate) fn new(database: PathBuf) -> (Self, EventSender) {
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

    /// Finish writing history after the event stream has ended.
    pub(crate) async fn finish(self) -> ShellResult<()> {
        Ok(self.history.finish().await?)
    }

    pub(crate) fn dispatch(&mut self, action: Action) {
        self.active_workers += 1;
        match action {
            Action::OpenHistory => return self.history.send(Command::Open),
            Action::TrimHistory { keep } => return self.history.send(Command::Trim(keep)),
            _ => {}
        }
        let path = self.database.clone();
        let sender = self.sender.clone();
        let _worker = tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let result = storage::execute(&path, action);
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
