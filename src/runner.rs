//! Deliver timestamped events to the core and dispatch its actions through the shell.

use std::path::PathBuf;

use tokio::sync::watch;

use crate::{
    core::{Core, HistoryState, SettingsState, State, TimedEvent},
    shell::{EventSender, Shell, ShellResult, read_clock},
};

/// A read-only observation containing the actual core state and event metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The core's in-memory state after handling the event.
    pub state: State,
    /// The core's dashboard settings.
    pub settings: SettingsState,
    /// What the core knows about stored history.
    pub history: HistoryState,
    /// The most recently handled event, absent before the first input.
    pub last_event: Option<TimedEvent>,
    /// The latest storage error, retained across subsequent state updates.
    pub storage_error: Option<String>,
}

/// Owns the core and consumes one event stream, independently of input sources.
#[derive(Debug)]
pub struct Runner {
    core: Core,
    shell: Shell,
    observations: watch::Sender<Observation>,
}

impl Runner {
    /// Create a runner, a cloneable event ingress, and a state subscription.
    ///
    /// Producers submit startup and user requests through the ingress. Cloning
    /// the subscription gives another observer access to the latest core state.
    #[must_use]
    pub fn new(database: PathBuf) -> (Self, EventSender, watch::Receiver<Observation>) {
        let core = Core::new();
        let (shell, events) = Shell::new(database);
        let (observations, receiver) = watch::channel(Observation {
            state: core.state().clone(),
            settings: core.settings().clone(),
            history: *core.history(),
            last_event: None,
            storage_error: None,
        });
        (
            Self {
                core,
                shell,
                observations,
            },
            events,
            receiver,
        )
    }

    /// Handle events until shutdown finishes all accepted inputs and effects.
    ///
    /// Each handled event and its actions are recorded in history, in processing
    /// order, before the actions are dispatched.
    ///
    /// # Errors
    /// Returns an error if a storage worker fails unexpectedly. Ordinary storage
    /// failures return as core events and remain visible in the observations.
    pub async fn run(mut self) -> ShellResult<State> {
        while let Some(event) = self.shell.next_event().await? {
            let processed_at = read_clock().wall;
            let actions = self.core.handle(event.occurred_at, event.event.clone());
            self.shell.record(
                event.occurred_at.wall,
                processed_at,
                event.event.clone(),
                actions.clone(),
            );
            for action in actions {
                self.shell.dispatch(action);
            }
            self.observations.send_replace(Observation {
                state: self.core.state().clone(),
                settings: self.core.settings().clone(),
                history: *self.core.history(),
                last_event: Some(event),
                storage_error: self.shell.latest_storage_error().cloned(),
            });
        }
        self.shell.finish().await?;
        Ok(self.core.state().clone())
    }
}
