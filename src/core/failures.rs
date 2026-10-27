//! How many attempts in a row have failed.

use derive_more::Display;
use serde::Serialize;

/// Attempts that failed in a row since the last success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Display, Serialize)]
pub struct FailureCount(u32);

impl FailureCount {
    /// No failures since the last success.
    pub const NONE: Self = Self(0);

    /// This many failures in a row.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    /// The number of failures.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The count after one more failure.
    #[must_use]
    pub const fn after_another(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Whether anything failed since the last success.
    #[must_use]
    pub const fn any(self) -> bool {
        self.0 > 0
    }
}
