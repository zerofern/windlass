use super::{Action, ClockReading, State};

/// The user's persistent choice about allowing Windlass to contact MAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MamRequestMode {
    /// Block MAM requests; used for first-time setup and absent preferences.
    Paused,
    /// Permit requests only when the other admission conditions are satisfied.
    Enabled,
}

/// A command or storage result for the MAM pause preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MamPauseEvent {
    /// Save a preference; pause applies immediately, enable waits for persistence.
    Set {
        /// The user's requested preference.
        mode: MamRequestMode,
    },
    /// Forget the saved preference and apply the paused default immediately.
    Delete,
    /// The preference was read successfully; absence defaults to paused.
    Read {
        /// The saved preference, if present.
        mode: Option<MamRequestMode>,
    },
    /// The saved preference could not be read.
    ReadFailed,
    /// A preference write completed durably.
    Written,
    /// A preference write failed.
    WriteFailed,
    /// A preference deletion completed durably.
    Deleted,
    /// A preference deletion failed.
    DeleteFailed,
}

/// A specific update requested for the saved MAM preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MamPreferenceUpdate {
    /// Save an explicit MAM request preference.
    Set(MamRequestMode),
    /// Delete the saved preference and use the paused default.
    Delete,
}

impl MamPreferenceUpdate {
    const fn stored(self) -> Option<MamRequestMode> {
        match self {
            Self::Set(mode) => Some(mode),
            Self::Delete => None,
        }
    }

    const fn mode(self) -> MamRequestMode {
        match self {
            Self::Set(mode) => mode,
            Self::Delete => MamRequestMode::Paused,
        }
    }

    const fn action(self) -> Action {
        match self {
            Self::Set(mode) => Action::WriteMamRequestMode { mode },
            Self::Delete => Action::DeleteMamRequestMode,
        }
    }
}

/// Storage activity for the MAM preference, including the exact pending update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MamPreferencePersistence {
    /// No storage operation is outstanding.
    Idle,
    /// The shell is persisting this update.
    Pending(MamPreferenceUpdate),
    /// The last storage update failed.
    Failed,
}

/// MAM preference data held directly in the core's ready state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MamPreferenceState {
    /// The last preference confirmed by a successful read or update.
    pub saved: Option<MamRequestMode>,
    /// The latest update requested by the user.
    pub requested: MamPreferenceUpdate,
    /// The preference currently applied, independently of other request gates.
    pub effective: MamRequestMode,
    /// The exact update being persisted, or its completion state.
    pub persistence: MamPreferencePersistence,
}

#[derive(Clone, Copy)]
enum MamPreferenceCompletion {
    Written,
    WriteFailed,
    Deleted,
    DeleteFailed,
}

pub(super) fn handle(
    state: &mut State,
    _occurred_at: ClockReading,
    event: MamPauseEvent,
) -> Vec<Action> {
    match event {
        MamPauseEvent::Set { mode } => update_mam_preference(state, MamPreferenceUpdate::Set(mode)),
        MamPauseEvent::Delete => update_mam_preference(state, MamPreferenceUpdate::Delete),
        MamPauseEvent::Read { mode } => loaded(state, mode),
        MamPauseEvent::ReadFailed => {
            if matches!(state, State::ReadingMamPreference { .. }) {
                *state = State::Failed;
            }
            Vec::new()
        }
        MamPauseEvent::Written => complete(state, MamPreferenceCompletion::Written),
        MamPauseEvent::WriteFailed => complete(state, MamPreferenceCompletion::WriteFailed),
        MamPauseEvent::Deleted => complete(state, MamPreferenceCompletion::Deleted),
        MamPauseEvent::DeleteFailed => complete(state, MamPreferenceCompletion::DeleteFailed),
    }
}

fn loaded(state: &mut State, saved: Option<MamRequestMode>) -> Vec<Action> {
    let State::ReadingMamPreference { requested } = *state else {
        return Vec::new();
    };
    let mode = saved.unwrap_or(MamRequestMode::Paused);
    *state = State::Ready(MamPreferenceState {
        saved,
        requested: MamPreferenceUpdate::Set(mode),
        effective: if requested.is_some() {
            MamRequestMode::Paused
        } else {
            mode
        },
        persistence: MamPreferencePersistence::Idle,
    });
    requested.map_or_else(Vec::new, |change| update_mam_preference(state, change))
}

fn update_mam_preference(state: &mut State, change: MamPreferenceUpdate) -> Vec<Action> {
    match state {
        State::OpeningStorage { requested } | State::ReadingMamPreference { requested } => {
            *requested = Some(change);
            Vec::new()
        }
        State::Ready(preference) => preference.update_mam_preference(change),
        State::NotStarted | State::Failed => Vec::new(),
    }
}

fn complete(state: &mut State, completion: MamPreferenceCompletion) -> Vec<Action> {
    match state {
        State::Ready(preference) => preference.complete(completion),
        _ => Vec::new(),
    }
}

impl MamPreferenceState {
    fn update_mam_preference(&mut self, change: MamPreferenceUpdate) -> Vec<Action> {
        self.requested = change;
        if change.mode() == MamRequestMode::Paused {
            self.effective = MamRequestMode::Paused;
        }
        if matches!(self.persistence, MamPreferencePersistence::Pending(_)) {
            return Vec::new();
        }
        if change.stored() == self.saved {
            self.effective = change.mode();
            self.persistence = MamPreferencePersistence::Idle;
            return Vec::new();
        }
        self.persistence = MamPreferencePersistence::Pending(change);
        vec![change.action()]
    }

    fn complete(&mut self, completion: MamPreferenceCompletion) -> Vec<Action> {
        let MamPreferencePersistence::Pending(pending) = self.persistence else {
            return Vec::new();
        };
        let success = match (pending, completion) {
            (MamPreferenceUpdate::Set(_), MamPreferenceCompletion::Written)
            | (MamPreferenceUpdate::Delete, MamPreferenceCompletion::Deleted) => true,
            (MamPreferenceUpdate::Set(_), MamPreferenceCompletion::WriteFailed)
            | (MamPreferenceUpdate::Delete, MamPreferenceCompletion::DeleteFailed) => false,
            _ => return Vec::new(),
        };
        if success {
            self.saved = pending.stored();
        }
        self.persistence = if success {
            MamPreferencePersistence::Idle
        } else {
            MamPreferencePersistence::Failed
        };
        self.effective = if success && pending == self.requested {
            pending.mode()
        } else {
            MamRequestMode::Paused
        };
        if pending != self.requested {
            return self.update_mam_preference(self.requested);
        }
        Vec::new()
    }
}
