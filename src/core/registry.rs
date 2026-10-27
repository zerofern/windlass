//! The torrent registry: the torrents Windlass wants, and what it last saw of
//! them. Rows are never deleted; see ADR-0004.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use super::{
    Action, ByteSize, ClockReading, ForceStart, Progress, SavePath, Session, Torrent,
    TorrentCommand, TorrentHash, TorrentState,
};

/// How often changed upload totals, progress, and seeding time are saved.
pub const COUNTER_SAVE_INTERVAL: Duration = Duration::from_mins(15);
/// How long an unchanged command waits before it is sent again.
const COMMAND_REPEAT_AFTER: Duration = Duration::from_mins(1);
/// MAM requires 72 hours of seeding; until then a torrent is unsatisfied and
/// stopping it risks a hit-and-run.
pub const SEEDING_REQUIRED: Duration = Duration::from_hours(72);
/// Windlass seeds a day beyond MAM's requirement, since the seeding time
/// qBittorrent reports is not MAM's own count.
pub const SEEDING_TARGET: Duration = SEEDING_REQUIRED.saturating_add(Duration::from_hours(24));

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
    /// Bookmarked, but MAM reports it snatched before; it waits for the owner to
    /// ask for it again.
    Snatched,
    /// In qBittorrent, whole, and running.
    Active,
    /// In qBittorrent and stopped, because a download's wedge was not spent.
    Held,
    /// Not in qBittorrent; the row stays as download history.
    Removed,
}

/// A download Windlass added, as the guard over running downloads sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// The torrent on MAM.
    pub mam: MamTorrentId,
    /// Its info hash.
    pub hash: TorrentHash,
    /// Whether Windlass keeps it stopped.
    pub held: bool,
    /// Whether qBittorrent reports it complete.
    pub finished: bool,
    /// When qBittorrent added it; unknown until qBittorrent lists it.
    pub added_at: Option<SystemTime>,
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
    pub save_path: Option<SavePath>,
    /// Whether the user allows its content to be deleted for space.
    pub deletion_allowed: bool,
    /// What qBittorrent last reported, if it ever did.
    pub observation: Option<Observation>,
}

/// Results of registry storage, and the owner's requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RegistryEvent {
    /// The owner asked to download a snatched torrent again.
    Redownload(RecordId),
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
    by_mam: BTreeMap<MamTorrentId, RecordId>,
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
            (Self::Loaded(records), RegistryEvent::Redownload(id)) => records.redownload(now, id),
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

    /// The rows, once they are loaded.
    pub(super) const fn records_mut(&mut self) -> Option<&mut Records> {
        match self {
            Self::Loaded(records) => Some(records),
            Self::Unread | Self::Reading | Self::ReadFailed => None,
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
        let by_mam = records
            .iter()
            .filter_map(|record| Some((record.identity.mam_id()?, record.id)))
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
            by_mam,
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

    /// The row for this MAM torrent.
    #[must_use]
    pub fn find_mam(&self, id: MamTorrentId) -> Option<&TorrentRecord> {
        let id = self.by_mam.get(&id)?;
        self.entries.get(id).map(|entry| &entry.record)
    }

    /// Downloads Windlass added from MAM, wanted running or held.
    pub fn downloads(&self) -> impl Iterator<Item = Download> + '_ {
        self.records().filter_map(|record| {
            let Identity::Linked { mam, hash } = &record.identity else {
                return None;
            };
            let seen = record.observation.as_ref().map(|seen| &seen.torrent);
            (matches!(record.desired, Desired::Active | Desired::Held)
                && record.origin != Origin::Adopted)
                .then(|| Download {
                    mam: *mam,
                    hash: hash.clone(),
                    held: record.desired == Desired::Held,
                    finished: seen.is_some_and(|torrent| torrent.completed_at.is_some()),
                    added_at: seen.and_then(|torrent| torrent.added_at),
                })
        })
    }

    /// Queued torrents, oldest first.
    pub fn queued(&self) -> impl Iterator<Item = MamTorrentId> + '_ {
        self.records()
            .filter(|record| record.desired == Desired::Queued)
            .filter_map(|record| record.identity.mam_id())
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

    /// Whether a row exists for this MAM torrent.
    pub(super) fn knows(&self, id: MamTorrentId) -> bool {
        self.by_mam.contains_key(&id)
    }

    /// Record new bookmarks: queued, or waiting for the owner when snatched before.
    pub(super) fn bookmarked(
        &mut self,
        now: Instant,
        bookmarks: impl IntoIterator<Item = (MamTorrentId, Desired)>,
    ) -> Vec<Action> {
        for (mam, desired) in bookmarks {
            if self.by_mam.contains_key(&mam) {
                continue;
            }
            let id = self.allocate();
            self.by_mam.insert(mam, id);
            self.entries.insert(
                id,
                Entry::new(TorrentRecord {
                    id,
                    identity: Identity::Mam(mam),
                    origin: Origin::Bookmark,
                    desired,
                    save_path: None,
                    deletion_allowed: false,
                    observation: None,
                }),
            );
            self.unsaved.insert(id);
        }
        self.save(now)
    }

    /// Queue a snatched torrent the owner wants again.
    fn redownload(&mut self, now: Instant, id: RecordId) -> Vec<Action> {
        let Some(entry) = self
            .entries
            .get_mut(&id)
            .filter(|entry| entry.record.desired == Desired::Snatched)
        else {
            return Vec::new();
        };
        entry.record.desired = Desired::Queued;
        entry.record.origin = Origin::Redownload;
        self.unsaved.insert(id);
        self.save(now)
    }

    /// A queued torrent's file was fetched: it becomes active under its hash.
    pub(super) fn fetched(
        &mut self,
        now: Instant,
        mam: MamTorrentId,
        hash: TorrentHash,
    ) -> Vec<Action> {
        // Only a queued row is fetched, so the MAM ID has a row.
        if let Some(&id) = self.by_mam.get(&mam) {
            self.link(id, mam, hash);
        }
        self.save(now)
    }

    /// A queued bookmark MAM now reports snatched before waits for the owner.
    pub(super) fn snatched(&mut self, now: Instant, mam: MamTorrentId) -> Vec<Action> {
        let id = self.by_mam.get(&mam).copied().filter(|id| {
            self.entries.get(id).is_some_and(|entry| {
                entry.record.desired == Desired::Queued && entry.record.origin == Origin::Bookmark
            })
        });
        if let Some(id) = id {
            self.set_desired(id, Desired::Snatched);
        }
        self.save(now)
    }

    /// Let a held torrent run again.
    pub(super) fn resume(&mut self, now: Instant, hash: &TorrentHash) -> Vec<Action> {
        let id = self.by_hash.get(hash).copied().filter(|id| {
            self.entries
                .get(id)
                .is_some_and(|entry| entry.record.desired == Desired::Held)
        });
        if let Some(id) = id {
            self.set_desired(id, Desired::Active);
        }
        self.save(now)
    }

    /// Stop this torrent and keep it stopped.
    pub(super) fn hold(&mut self, now: Instant, hash: &TorrentHash) -> Vec<Action> {
        // Only a fetched torrent is held, so the hash has a row.
        if let Some(&id) = self.by_hash.get(hash) {
            self.set_desired(id, Desired::Held);
        }
        self.save(now)
    }

    /// Link a fetched row to its hash and make it active. A torrent already in
    /// qBittorrent keeps its row, which gains the MAM ID; the queued row stays
    /// as history.
    fn link(&mut self, id: RecordId, mam: MamTorrentId, hash: TorrentHash) {
        let target = match self.by_hash.get(&hash) {
            Some(&existing) if existing != id => {
                self.set_desired(id, Desired::Removed);
                self.by_mam.insert(mam, existing);
                existing
            }
            _ => id,
        };
        let identity = Identity::Linked {
            mam,
            hash: hash.clone(),
        };
        self.entries
            .entry(target)
            .and_modify(|entry| entry.record.identity = identity);
        self.set_desired(target, Desired::Active);
        self.by_hash.insert(hash, target);
    }

    fn set_desired(&mut self, id: RecordId, desired: Desired) {
        self.entries
            .entry(id)
            .and_modify(|entry| entry.record.desired = desired);
        self.unsaved.insert(id);
    }

    const fn allocate(&mut self) -> RecordId {
        let id = self.next_id;
        self.next_id = id.after();
        id
    }

    fn adopt(&mut self, hash: &TorrentHash, torrent: &Torrent) -> RecordId {
        let id = self.allocate();
        self.by_hash.insert(hash.clone(), id);
        self.entries.insert(
            id,
            Entry::new(TorrentRecord {
                id,
                identity: Identity::Client(hash.clone()),
                origin: Origin::Adopted,
                desired: Desired::Active,
                save_path: torrent.save_path.clone(),
                deletion_allowed: false,
                observation: None,
            }),
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

    /// The first change needed: removals, whole content, held torrents
    /// stopped, fetched torrents added, starts, then force starts: a complete
    /// torrent short of the seeding target runs regardless of qBittorrent's
    /// queue, and goes back to the queue once it has seeded long enough.
    ///
    /// Nothing here adds an unsatisfied torrent on MAM: a torrent that already
    /// downloaded something is counted already, and a torrent found stopped
    /// that has downloaded nothing is left stopped, since starting it would
    /// pass the unsatisfied limit's check. Windlass's own downloads passed it
    /// when they were admitted.
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
        let running: Vec<TorrentHash> = present(Desired::Held)
            .filter(|(_, torrent)| !matches!(torrent.state, TorrentState::Stopped(_)))
            .map(|(hash, _)| hash.clone())
            .collect();
        if !running.is_empty() {
            return Some(TorrentCommand::Stop(running));
        }
        let missing = self.records().find_map(|record| match &record.identity {
            Identity::Linked { mam, hash }
                if record.desired == Desired::Active
                    && record.origin != Origin::Adopted
                    && !session.torrents.contains_key(hash) =>
            {
                Some(TorrentCommand::Add {
                    mam: *mam,
                    hash: hash.clone(),
                })
            }
            _ => None,
        });
        if missing.is_some() {
            return missing;
        }
        let stopped: Vec<TorrentHash> = self
            .records()
            .filter(|record| record.desired == Desired::Active)
            .filter_map(|record| {
                let hash = record.identity.hash()?;
                let torrent = session.torrents.get(hash)?;
                let untouched = torrent.progress == Progress::default()
                    && torrent.downloaded == ByteSize::new(0);
                (matches!(torrent.state, TorrentState::Stopped(_))
                    && !(record.origin == Origin::Adopted && untouched))
                    .then(|| hash.clone())
            })
            .collect();
        if !stopped.is_empty() {
            return Some(TorrentCommand::Start(stopped));
        }
        let unsatisfied = |torrent: &Torrent| {
            torrent.completed_at.is_some() && torrent.seeding_time < SEEDING_TARGET
        };
        let force: Vec<TorrentHash> = present(Desired::Active)
            .filter(|(_, torrent)| {
                unsatisfied(torrent) && torrent.force_start == ForceStart::NotForced
            })
            .map(|(hash, _)| hash.clone())
            .collect();
        if !force.is_empty() {
            return Some(TorrentCommand::ForceStart(force));
        }
        let release: Vec<TorrentHash> = present(Desired::Active)
            .filter(|(_, torrent)| {
                !unsatisfied(torrent) && torrent.force_start == ForceStart::Forced
            })
            .map(|(hash, _)| hash.clone())
            .collect();
        (!release.is_empty()).then_some(TorrentCommand::EndForceStart(release))
    }
}

impl Entry {
    const fn new(record: TorrentRecord) -> Self {
        Self {
            record,
            saved_at: None,
            counters_unsaved: false,
        }
    }

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
