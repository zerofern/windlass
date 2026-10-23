use std::time::{Duration, Instant};

use serde::{Serialize, Serializer};

/// The timers the core schedules. Each name has at most one live instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum TimerName {
    /// Read qBittorrent's state again.
    QbittorrentPoll,
    /// Renew, or retry, the forwarded-port lease.
    PortLease,
}

/// One scheduled instance of a timer. Replacing a timer gives it a new
/// identity, so an obsolete expiry is recognised and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct TimerId(u64);

/// When a timer expires: a monotonic deadline, recorded in history as the
/// delay from the step that scheduled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline {
    /// The monotonic instant at which the timer expires.
    pub at: Instant,
    /// The delay from the scheduling step's occurrence time.
    pub after: Duration,
}

impl Serialize for Deadline {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.after.serialize(serializer)
    }
}

/// Allocates timer identities and turns delays into deadlines.
#[derive(Debug, Default)]
pub(super) struct Timers {
    issued: u64,
}

impl Timers {
    pub(super) const fn new() -> Self {
        Self { issued: 0 }
    }

    pub(super) fn schedule(&mut self, now: Instant, after: Duration) -> (TimerId, Deadline) {
        self.issued += 1;
        (
            TimerId(self.issued),
            Deadline {
                at: now.checked_add(after).unwrap_or(now),
                after,
            },
        )
    }
}
