use super::{Action, ClockReading, State};

/// Whether startup initialises stored records or resumes an existing instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    /// Create initial records explicitly, refusing to overwrite existing records.
    Initialise,
    /// Load existing records, failing if required records are missing or unreadable.
    Resume,
}

/// A startup command or observed storage result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupEvent {
    /// Begin startup; the core decides which stored facts to request.
    Start {
        /// Whether to initialise records or resume an existing instance.
        mode: StartupMode,
    },
    /// The storage requested during startup is open and ready for reads.
    StorageOpened,
    /// Storage could not be opened as requested.
    StorageOpenFailed,
}

pub(super) fn handle(
    state: &mut State,
    _occurred_at: ClockReading,
    event: StartupEvent,
) -> Vec<Action> {
    match event {
        StartupEvent::Start { mode } => {
            if !matches!(state, State::NotStarted) {
                return Vec::new();
            }
            *state = State::OpeningStorage { requested: None };
            vec![Action::OpenStorage { mode }]
        }
        StartupEvent::StorageOpened => {
            let State::OpeningStorage { requested } = *state else {
                return Vec::new();
            };
            *state = State::ReadingMamPreference { requested };
            vec![Action::ReadMamRequestMode]
        }
        StartupEvent::StorageOpenFailed => {
            if matches!(state, State::OpeningStorage { .. }) {
                *state = State::Failed;
            }
            Vec::new()
        }
    }
}
