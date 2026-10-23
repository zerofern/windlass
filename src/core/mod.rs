//! Pure, synchronous application state and decisions.
//!
//! Time and external observations enter as inputs. Effects leave as actions.

use std::time::{Instant, SystemTime};

/// Clock values recorded where an event occurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// Process-local time for active deadlines and elapsed durations.
    pub monotonic: Instant,
    /// Calendar time for deadlines that must survive a process restart.
    pub wall: SystemTime,
}

/// An event paired with its occurrence time before it enters the event queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedEvent {
    /// When the event occurred, independently of when the runner handles it.
    pub occurred_at: ClockReading,
    /// The command or observed result.
    pub event: Event,
}

/// An incoming command or observed result handled by the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Startup commands and storage availability.
    Startup(StartupEvent),
    /// MAM pause commands and preference storage results.
    MamPause(MamPauseEvent),
}

/// An effect requested by the core for the shell to execute.
///
/// The core permits only one outstanding storage operation for the MAM
/// preference, so its completion is unambiguous without an operation identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open storage using an explicit startup mode.
    OpenStorage {
        /// Whether storage may be created or must already exist.
        mode: StartupMode,
    },
    /// Read the saved MAM preference, reporting absence separately from failure.
    ReadMamRequestMode,
    /// Persist the user's preference and report when the write completes.
    WriteMamRequestMode {
        /// The preference to store.
        mode: MamRequestMode,
    },
    /// Remove the saved preference; absence defaults to paused in the core.
    DeleteMamRequestMode,
}

mod machine;
mod mam_pause;
mod startup;

pub use machine::{Core, State};
pub use mam_pause::{
    MamPauseEvent, MamPreferencePersistence, MamPreferenceState, MamPreferenceUpdate,
    MamRequestMode,
};
pub use startup::{StartupEvent, StartupMode};
