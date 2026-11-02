//! The watch over ratio use. Windlass never downloads anything MAM counts
//! against the ratio: freeleech, VIP torrents while the account is VIP, and
//! wedged torrents leave MAM's downloaded figure unchanged. If that figure
//! rises anyway, something used ratio, so every download stops until the owner
//! has checked.

use std::time::SystemTime;

use serde::Serialize;

use crate::core::ByteSize;

/// MAM's downloaded figure rose: something used ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RatioUse {
    /// The downloaded figure before.
    pub before: ByteSize,
    /// The downloaded figure MAM reported when the rise was seen.
    pub after: ByteSize,
    /// When the rise was seen.
    pub at: SystemTime,
}

impl RatioUse {
    /// The bytes MAM counted against the ratio.
    #[must_use]
    pub const fn counted(&self) -> ByteSize {
        self.after.less(self.before)
    }
}

/// The guard as storage keeps it across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SavedRatioGuard {
    /// Nothing saved yet: the first reading sets the figure to watch.
    Unset,
    /// Watching from this downloaded figure.
    Watching(ByteSize),
    /// Ratio was used; downloads stay stopped until the owner checks.
    Tripped(RatioUse),
}

/// What the ratio guard knows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum RatioGuard {
    /// The saved guard is being read, or was not asked for yet.
    #[default]
    Reading,
    /// The saved guard could not be read, so no download starts.
    ReadFailed,
    /// Watching MAM's downloaded figure, once a reading has shown it.
    Watching(Option<ByteSize>),
    /// Ratio was used; downloads stay stopped until the owner checks.
    Tripped(RatioUse),
}

/// What a reading changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Seen {
    /// Nothing to save.
    Unchanged,
    /// A new figure to watch from, to save.
    Rebased(SavedRatioGuard),
    /// Ratio was used, to save and to act on.
    Tripped(RatioUse),
}

impl RatioGuard {
    /// The guard read from storage.
    #[must_use]
    pub const fn restored(saved: SavedRatioGuard) -> Self {
        match saved {
            SavedRatioGuard::Unset => Self::Watching(None),
            SavedRatioGuard::Watching(downloaded) => Self::Watching(Some(downloaded)),
            SavedRatioGuard::Tripped(used) => Self::Tripped(used),
        }
    }

    /// Whether downloads may run: the guard is known and has not tripped.
    #[must_use]
    pub const fn permits_downloads(&self) -> bool {
        matches!(self, Self::Watching(_))
    }

    /// Note the downloaded figure from a reading. A rise trips the guard; a
    /// fall, such as a correction by staff, becomes the figure to watch from.
    pub(super) fn observe(&mut self, downloaded: ByteSize, at: SystemTime) -> Seen {
        let Self::Watching(watched) = *self else {
            return Seen::Unchanged;
        };
        match watched {
            Some(before) if downloaded > before => {
                let used = RatioUse {
                    before,
                    after: downloaded,
                    at,
                };
                *self = Self::Tripped(used);
                Seen::Tripped(used)
            }
            Some(before) if downloaded == before => Seen::Unchanged,
            _ => {
                *self = Self::Watching(Some(downloaded));
                Seen::Rebased(SavedRatioGuard::Watching(downloaded))
            }
        }
    }

    /// The owner checked what used ratio: watch again from the latest figure.
    pub(super) fn checked(&mut self, latest: Option<ByteSize>) -> Option<SavedRatioGuard> {
        let Self::Tripped(used) = *self else {
            return None;
        };
        let downloaded = latest.unwrap_or(used.after);
        *self = Self::Watching(Some(downloaded));
        Some(SavedRatioGuard::Watching(downloaded))
    }
}
