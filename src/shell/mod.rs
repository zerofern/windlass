//! Execute core actions and return timestamped observations through one event stream.

use std::{
    error::Error,
    path::PathBuf,
    time::{Instant, SystemTime},
};

use tokio::sync::{mpsc, watch};

use crate::core::{Action, ClockReading, TimedEvent};

pub mod console;
mod ingress;
mod storage;

pub use ingress::EventSender;
use ingress::{IngressState, Input};

pub(crate) type ShellResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

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
}

impl Shell {
    pub(crate) fn new(database: PathBuf) -> (Self, EventSender) {
        let (sender, receiver) = mpsc::channel(EVENT_QUEUE_CAPACITY);
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

    pub(crate) fn dispatch(&mut self, action: Action) {
        self.active_workers += 1;
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

fn read_clock() -> ClockReading {
    ClockReading {
        monotonic: Instant::now(),
        wall: SystemTime::now(),
    }
}
