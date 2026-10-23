use super::{
    Action, ClockReading, Event, MamPreferencePersistence, MamPreferenceState, MamPreferenceUpdate,
    mam_pause, startup,
};

/// Owns application state and makes decisions without performing I/O.
#[derive(Debug, Default)]
pub struct Core {
    state: State,
}

/// The actual in-memory state of the core.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum State {
    /// Startup has not been requested.
    #[default]
    NotStarted,
    /// Waiting for the shell to open storage.
    OpeningStorage {
        /// The latest preference update received during startup.
        requested: Option<MamPreferenceUpdate>,
    },
    /// Waiting for the shell to read the MAM preference.
    ReadingMamPreference {
        /// The latest preference update received during startup.
        requested: Option<MamPreferenceUpdate>,
    },
    /// Startup completed; this is the live MAM preference state.
    Ready(MamPreferenceState),
    /// Startup failed; external work remains blocked.
    Failed,
}

impl State {
    /// Whether startup or the latest preference update failed.
    #[must_use]
    pub const fn has_failed(&self) -> bool {
        matches!(
            self,
            Self::Failed
                | Self::Ready(MamPreferenceState {
                    persistence: MamPreferencePersistence::Failed,
                    ..
                })
        )
    }
}

impl Core {
    /// Construct an idle core. Storage is read only after a startup event.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::NotStarted,
        }
    }

    /// Handle one input and return effects for the shell to execute.
    ///
    /// Both event handlers receive the full state and the event's occurrence time.
    #[must_use]
    pub fn handle(&mut self, occurred_at: ClockReading, event: Event) -> Vec<Action> {
        match event {
            Event::Startup(event) => startup::handle(&mut self.state, occurred_at, event),
            Event::MamPause(event) => mam_pause::handle(&mut self.state, occurred_at, event),
        }
    }

    /// Inspect the actual state without allowing mutation outside event handling.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }
}
