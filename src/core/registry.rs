//! The torrent registry: the torrents Windlass wants, and what it last saw of
//! them. Rows are never deleted; see ADR-0004.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use super::{Action, ClockReading, Session, Torrent, TorrentCommand, TorrentHash, TorrentState};

/// How often changed upload totals, progress, and seeding time are saved.
pub const COUNTER_SAVE_INTERVAL: Duration = Duration::from_mins(15);
/// How long an unchanged command waits before it is sent again.
const COMMAND_REPEAT_AFTER: Duration = Duration::from_mins(1);

/// A registry row's identity, allocated by the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct RecordId(NonZeroU64);

impl RecordId {
    /// An identity read from storage; zero is not one.
    #[must_use]
    pub const fn new(id: u64) -> Option<Self> {
        match NonZeroU64::new(id) {
            Some(id) => Some(Self(id)),
            None => None,
        }
    }

    /// The identity as stored.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    const fn after(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A torrent's identity on MAM, the stable link to its metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct MamTorrentId(NonZeroU64);

impl MamTorrentId {
    /// A MAM torrent ID; zero is not one.
    #[must_use]
    pub const fn new(id: u64) -> Option<Self> {
        match NonZeroU64::new(id) {
            Some(id) => Some(Self(id)),
            None => None,
        }
    }

    /// The ID as MAM shows it.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// How a row links to MAM and to qBittorrent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Identity {
    /// Known on MAM; not yet added to qBittorrent.
    Mam(MamTorrentId),
    /// Found in qBittorrent; its MAM torrent ID is not established.
    Client(TorrentHash),
    /// Known on both.
    Linked {
        /// The MAM torrent ID.
        mam: MamTorrentId,
        /// The info hash.
        hash: TorrentHash,
    },
}

impl Identity {
    /// An identity from its parts; a row needs at least one.
    #[must_use]
    pub fn new(mam: Option<MamTorrentId>, hash: Option<TorrentHash>) -> Option<Self> {
        match (mam, hash) {
            (Some(mam), Some(hash)) => Some(Self::Linked { mam, hash }),
            (Some(mam), None) => Some(Self::Mam(mam)),
            (None, Some(hash)) => Some(Self::Client(hash)),
            (None, None) => None,
        }
    }

    /// The info hash, once known.
    #[must_use]
    pub const fn hash(&self) -> Option<&TorrentHash> {
        match self {
            Self::Mam(_) => None,
            Self::Client(hash) | Self::Linked { hash, .. } => Some(hash),
        }
    }

    /// The MAM torrent ID, once known.
    #[must_use]
    pub const fn mam_id(&self) -> Option<MamTorrentId> {
        match self {
            Self::Mam(mam) | Self::Linked { mam, .. } => Some(*mam),
            Self::Client(_) => None,
        }
    }
}

/// How a torrent came to be in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Origin {
    /// Selected from a MAM bookmark.
    Bookmark,
    /// Downloaded again at the user's request.
    Redownload,
    /// Found in qBittorrent without a row.
    Adopted,
}

/// What Windlass wants for a torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Desired {
    /// Selected, waiting for a free MAM slot.
    Queued,
    /// In qBittorrent, whole, and running.
    Active,
    /// Not in qBittorrent; the row stays as download history.
    Removed,
}

/// What qBittorrent last reported about a torrent, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    /// The torrent as reported.
    pub torrent: Torrent,
    /// When it was reported.
    pub last_seen: SystemTime,
}

/// One registry row: intent, then observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TorrentRecord {
    /// The row's identity.
    pub id: RecordId,
    /// How the row links to MAM and qBittorrent.
    pub identity: Identity,
    /// How the torrent arrived.
    pub origin: Origin,
    /// What Windlass wants for it.
    pub desired: Desired,
    /// Where its content is saved.
    pub save_path: Option<String>,
    /// Whether the user allows its content to be deleted for space.
    pub deletion_allowed: bool,
    /// What qBittorrent last reported, if it ever did.
    pub observation: Option<Observation>,
}

/// Results of registry storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RegistryEvent {
    /// Every row was read.
    Read(Vec<TorrentRecord>),
    /// Reading failed.
    ReadFailed,
    /// The last batch was saved.
    Saved,
    /// Saving the last batch failed.
    SaveFailed,
}

/// The registry's availability.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Registry {
    /// Storage is not open yet.
    #[default]
    Unread,
    /// Reading the rows.
    Reading,
    /// The rows could not be read; torrents are watched but not managed.
    ReadFailed,
    /// The rows are in memory.
    Loaded(Records),
}

/// The rows in memory and their save progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Records {
    entries: BTreeMap<RecordId, Entry>,
    by_hash: BTreeMap<TorrentHash, RecordId>,
    next_id: RecordId,
    unsaved: BTreeSet<RecordId>,
    saving: Option<BTreeSet<RecordId>>,
    save_failed: bool,
    last_command: Option<(TorrentCommand, Instant)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    record: TorrentRecord,
    saved_at: Option<Instant>,
    counters_unsaved: bool,
}

impl Registry {
    pub(super) fn start_reading(&mut self) -> Vec<Action> {
        *self = Self::Reading;
        vec![Action::ReadTorrentRegistry]
    }

    pub(super) fn handle(&mut self, now: Instant, event: RegistryEvent) -> Vec<Action> {
        match (&mut *self, event) {
            (Self::Reading, RegistryEvent::Read(records)) => {
                *self = Self::Loaded(Records::new(records));
                Vec::new()
            }
            (Self::Reading, RegistryEvent::ReadFailed) => {
                *self = Self::ReadFailed;
                Vec::new()
            }
            (Self::Loaded(records), RegistryEvent::Saved) if records.saving.is_some() => {
                records.saving = None;
                records.save_failed = false;
                records.save(now)
            }
            (Self::Loaded(records), RegistryEvent::SaveFailed) if records.saving.is_some() => {
                // Retried with the next qBittorrent update.
                records
                    .unsaved
                    .extend(records.saving.take().into_iter().flatten());
                records.save_failed = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Adopt unknown torrents and record what qBittorrent reports.
    pub(super) fn observe(&mut self, now: ClockReading, session: &Session) -> Vec<Action> {
        match self {
            Self::Loaded(records) if session.listed() => records.observe(now, session),
            _ => Vec::new(),
        }
    }

    /// The change qBittorrent needs to match the registry, if any.
    pub(super) fn command(&mut self, now: Instant, session: &Session) -> Option<TorrentCommand> {
        match self {
            Self::Loaded(records) if session.listed() => records.command(now, session),
            _ => None,
        }
    }
}

impl Records {
    fn new(records: Vec<TorrentRecord>) -> Self {
        let next_id = records
            .iter()
            .map(|record| record.id)
            .max()
            .map_or(RecordId(NonZeroU64::MIN), RecordId::after);
        let by_hash = records
            .iter()
            .filter_map(|record| Some((record.identity.hash()?.clone(), record.id)))
            .collect();
        let entries = records
            .into_iter()
            .map(|record| {
                (
                    record.id,
                    Entry {
                        record,
                        saved_at: None,
                        counters_unsaved: false,
                    },
                )
            })
            .collect();
        Self {
            entries,
            by_hash,
            next_id,
            unsaved: BTreeSet::new(),
            saving: None,
            save_failed: false,
            last_command: None,
        }
    }

    /// Every row, oldest first.
    pub fn records(&self) -> impl Iterator<Item = &TorrentRecord> {
        self.entries.values().map(|entry| &entry.record)
    }

    /// The row for this info hash.
    #[must_use]
    pub fn find(&self, hash: &TorrentHash) -> Option<&TorrentRecord> {
        let id = self.by_hash.get(hash)?;
        self.entries.get(id).map(|entry| &entry.record)
    }

    /// Whether the last save failed; it is retried with the next update.
    #[must_use]
    pub const fn save_failed(&self) -> bool {
        self.save_failed
    }

    fn observe(&mut self, now: ClockReading, session: &Session) -> Vec<Action> {
        for (hash, torrent) in &session.torrents {
            let observation = Observation {
                torrent: torrent.clone(),
                last_seen: now.wall,
            };
            let known = self.by_hash.get(hash).copied();
            let id = known.unwrap_or_else(|| self.adopt(hash, torrent));
            if let Some(entry) = self.entries.get_mut(&id)
                && entry.observe(now.monotonic, observation)
            {
                self.unsaved.insert(id);
            }
        }
        self.save(now.monotonic)
    }

    fn adopt(&mut self, hash: &TorrentHash, torrent: &Torrent) -> RecordId {
        let id = self.next_id;
        self.next_id = id.after();
        self.by_hash.insert(hash.clone(), id);
        self.entries.insert(
            id,
            Entry {
                record: TorrentRecord {
                    id,
                    identity: Identity::Client(hash.clone()),
                    origin: Origin::Adopted,
                    desired: Desired::Active,
                    save_path: torrent.save_path.clone(),
                    deletion_allowed: false,
                    observation: None,
                },
                saved_at: None,
                counters_unsaved: false,
            },
        );
        id
    }

    /// Save every changed row in one batch, unless a batch is outstanding.
    fn save(&mut self, now: Instant) -> Vec<Action> {
        if self.saving.is_some() || self.unsaved.is_empty() {
            return Vec::new();
        }
        let ids = std::mem::take(&mut self.unsaved);
        let records = self
            .entries
            .iter_mut()
            .filter(|(id, _)| ids.contains(id))
            .map(|(_, entry)| {
                entry.saved_at = Some(now);
                entry.counters_unsaved = false;
                entry.record.clone()
            })
            .collect();
        self.saving = Some(ids);
        vec![Action::SaveTorrents { records }]
    }

    fn command(&mut self, now: Instant, session: &Session) -> Option<TorrentCommand> {
        let command = self.reconcile(session)?;
        if let Some((last, at)) = &self.last_command
            && *last == command
            && now.duration_since(*at) < COMMAND_REPEAT_AFTER
        {
            return None;
        }
        self.last_command = Some((command.clone(), now));
        Some(command)
    }

    /// The first change needed: removals, then whole content, then starts.
    fn reconcile(&self, session: &Session) -> Option<TorrentCommand> {
        let present = |desired: Desired| {
            self.records()
                .filter(move |record| record.desired == desired)
                .filter_map(|record| {
                    let hash = record.identity.hash()?;
                    Some((hash, session.torrents.get(hash)?))
                })
        };
        let removed: Vec<TorrentHash> = present(Desired::Removed)
            .map(|(hash, _)| hash.clone())
            .collect();
        if !removed.is_empty() {
            return Some(TorrentCommand::Remove(removed));
        }
        if let Some((hash, _)) = present(Desired::Active).find(|(_, torrent)| partial(torrent)) {
            return Some(TorrentCommand::SelectAllFiles(hash.clone()));
        }
        let stopped: Vec<TorrentHash> = present(Desired::Active)
            .filter(|(_, torrent)| matches!(torrent.state, TorrentState::Stopped { .. }))
            .map(|(hash, _)| hash.clone())
            .collect();
        (!stopped.is_empty()).then_some(TorrentCommand::Start(stopped))
    }
}

impl Entry {
    /// Record a new observation; whether the row must be saved now.
    fn observe(&mut self, now: Instant, observation: Observation) -> bool {
        let current = observation.torrent.clone();
        match self.record.observation.replace(observation) {
            Some(previous) if !significant(&previous.torrent, &current) => {
                self.counters_unsaved |= previous.torrent != current;
                let due = self
                    .saved_at
                    .is_none_or(|saved_at| now.duration_since(saved_at) >= COUNTER_SAVE_INTERVAL);
                self.counters_unsaved && due
            }
            _ => true,
        }
    }
}

/// Whether some files are not selected. Sizes are unknown until metadata arrives.
fn partial(torrent: &Torrent) -> bool {
    torrent.state != TorrentState::FetchingMetadata && torrent.size < torrent.total_size
}

/// Whether a change is saved at once rather than with the periodic counters.
fn significant(previous: &Torrent, current: &Torrent) -> bool {
    previous.name != current.name
        || previous.state != current.state
        || previous.size != current.size
        || previous.total_size != current.total_size
        || previous.added_at != current.added_at
        || previous.completed_at != current.completed_at
        || previous.content_path != current.content_path
        || previous.save_path != current.save_path
}
