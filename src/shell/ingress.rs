use std::io;

use tokio::{
    sync::{mpsc, watch},
    task::JoinError,
};

use crate::core::{Event, TimedEvent};

use super::{WorkerCompletion, read_clock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IngressState {
    Open,
    Closing,
}

#[derive(Debug)]
pub(super) enum Input {
    Event(TimedEvent),
    Completed(Result<WorkerCompletion, JoinError>),
    Shutdown,
}

/// A shared event ingress for console requests and future HTTP handlers.
#[derive(Debug, Clone)]
pub struct EventSender {
    pub(super) sender: mpsc::Sender<Input>,
    pub(super) state: watch::Sender<IngressState>,
}

impl EventSender {
    /// Timestamp an event at this boundary and wait for room in the queue.
    ///
    /// Success acknowledges enqueueing; persistence is reported through core state.
    ///
    /// # Errors
    /// Returns `BrokenPipe` if the runner has stopped or is shutting down.
    pub async fn send(&self, event: Event) -> io::Result<()> {
        self.send_timed(TimedEvent {
            occurred_at: read_clock(),
            event,
        })
        .await
    }

    /// Submit an event whose producer has already recorded its occurrence time.
    ///
    /// # Errors
    /// Returns `BrokenPipe` if the runner has stopped or is shutting down.
    pub async fn send_timed(&self, event: TimedEvent) -> io::Result<()> {
        let permit = self.sender.reserve().await.map_err(closed)?;
        // Keep acceptance and enqueueing together across shutdown.
        let state = self.state.borrow();
        if *state == IngressState::Closing {
            return Err(closed("shutdown requested"));
        }
        permit.send(Input::Event(event));
        drop(state);
        Ok(())
    }

    /// Request shutdown after queued inputs and their resulting effects finish.
    ///
    /// All producers share this lifecycle. Remaining ingress clones do not keep
    /// the runner alive after shutdown, and further submissions are rejected.
    ///
    /// # Errors
    /// Returns `BrokenPipe` if the runner has already stopped.
    pub async fn shutdown(&self) -> io::Result<()> {
        let permit = self.sender.reserve().await.map_err(closed)?;
        self.state.send_replace(IngressState::Closing);
        permit.send(Input::Shutdown);
        Ok(())
    }
}

fn closed(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        format!("event ingress closed: {error}"),
    )
}
