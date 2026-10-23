use std::time::SystemTime;

use serde::Serialize;

use super::{Action, EntryCount, HistoryLimit};

/// The position of a handled event in this process's processing order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Step(u64);

impl Step {
    pub(super) const ZERO: Self = Self(0);

    pub(super) const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// The number of steps handled after `earlier` and before `self`.
    const fn strictly_after(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0).saturating_sub(1)
    }
}

/// A history storage result reported by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HistoryEvent {
    /// History storage is open. The count includes every entry recorded before
    /// the step that requested opening, and that step's own entry.
    Opened {
        /// The number of entries stored.
        retained: EntryCount,
    },
    /// History storage could not be opened.
    OpenFailed,
    /// The oldest entries were removed. The count is taken as for `Opened`.
    Trimmed {
        /// The number of entries stored after trimming.
        retained: EntryCount,
    },
    /// Removing the oldest entries failed.
    TrimFailed,
    /// Entries could not be recorded. Reported once recording works again, so
    /// this event's own entry marks the gap.
    EntriesLost {
        /// How many entries are missing.
        count: EntryCount,
        /// When the first missing entry was processed.
        from: SystemTime,
        /// When the last missing entry was processed.
        until: SystemTime,
    },
}

/// Whether trimming is outstanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trim {
    /// No trim is outstanding.
    Idle,
    /// The trim requested during this step is outstanding.
    Pending(Step),
    /// The last trim failed. Trimming resumes after a restart.
    Failed,
}

/// The core's knowledge of stored history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HistoryState {
    /// Storage has not been opened yet; the shell buffers entries.
    #[default]
    Closed,
    /// Opening was requested during this step.
    Opening(Step),
    /// History is being recorded.
    Open {
        /// The number of stored entries, including this step's entry.
        retained: EntryCount,
        /// Whether trimming is outstanding.
        trim: Trim,
    },
    /// History storage could not be opened.
    Unavailable,
}

impl HistoryState {
    pub(super) fn open(&mut self, step: Step) -> Vec<Action> {
        *self = Self::Opening(step);
        vec![Action::OpenHistory]
    }

    pub(super) const fn handle(&mut self, step: Step, event: HistoryEvent) {
        match (*self, event) {
            (Self::Opening(requested), HistoryEvent::Opened { retained })
            | (
                Self::Open {
                    trim: Trim::Pending(requested),
                    ..
                },
                HistoryEvent::Trimmed { retained },
            ) => {
                *self = Self::Open {
                    retained: retained.plus(step.strictly_after(requested)),
                    trim: Trim::Idle,
                };
            }
            (Self::Opening(_), HistoryEvent::OpenFailed) => *self = Self::Unavailable,
            (
                Self::Open {
                    retained,
                    trim: Trim::Pending(_),
                },
                HistoryEvent::TrimFailed,
            ) => {
                *self = Self::Open {
                    retained,
                    trim: Trim::Failed,
                };
            }
            (Self::Open { retained, trim }, HistoryEvent::EntriesLost { count, .. }) => {
                *self = Self::Open {
                    retained: retained.minus(count),
                    trim,
                };
            }
            _ => {}
        }
    }

    /// Count this step's entry and trim if the limit is exceeded.
    pub(super) fn after_step(&mut self, step: Step, limit: Option<HistoryLimit>) -> Vec<Action> {
        let Self::Open { retained, trim } = self else {
            return Vec::new();
        };
        *retained = retained.plus(1);
        match limit {
            Some(limit) if *trim == Trim::Idle && limit.is_exceeded_by(*retained) => {
                *trim = Trim::Pending(step);
                vec![Action::TrimHistory {
                    keep: limit.trim_target(),
                }]
            }
            _ => Vec::new(),
        }
    }
}
