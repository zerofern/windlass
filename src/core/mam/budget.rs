//! The token bucket every Windlass request to MAM draws from (Direction): five
//! tokens, one restored every ten seconds, starting full.

use std::time::{Duration, Instant};

/// Tokens the bucket holds when full.
pub const REQUEST_BURST: u32 = 5;
/// How often one token is restored.
pub const REQUEST_REFILL: Duration = Duration::from_secs(10);

/// The shared request budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    tokens: u32,
    refilled_at: Option<Instant>,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

impl Budget {
    /// A full bucket.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tokens: REQUEST_BURST,
            refilled_at: None,
        }
    }

    /// Take a token, if one is available.
    pub(super) fn take(&mut self, now: Instant) -> bool {
        let (tokens, refilled_at) = self.refilled(now);
        if tokens == 0 {
            return false;
        }
        self.tokens = tokens - 1;
        self.refilled_at = Some(refilled_at.unwrap_or(now));
        true
    }

    /// When the next token becomes available, if none is now.
    #[must_use]
    pub fn next_token(&self, now: Instant) -> Option<Instant> {
        match self.refilled(now) {
            (0, Some(refilled_at)) => refilled_at.checked_add(REQUEST_REFILL),
            _ => None,
        }
    }

    /// The tokens after restoring those due by `now`, and when the last was
    /// restored; a full bucket has no restoring in progress.
    fn refilled(&self, now: Instant) -> (u32, Option<Instant>) {
        let Some(refilled_at) = self.refilled_at else {
            return (self.tokens, None);
        };
        let elapsed = now.saturating_duration_since(refilled_at);
        let restored =
            u32::try_from(elapsed.as_secs() / REQUEST_REFILL.as_secs()).unwrap_or(u32::MAX);
        let tokens = self.tokens.saturating_add(restored).min(REQUEST_BURST);
        if tokens == REQUEST_BURST {
            return (tokens, None);
        }
        let advanced = REQUEST_REFILL.saturating_mul(restored);
        (tokens, refilled_at.checked_add(advanced))
    }
}
