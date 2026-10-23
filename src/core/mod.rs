//! Pure, synchronous application state and decisions.
//!
//! Time and external observations enter as inputs. Effects leave as actions.

use std::time::{Instant, SystemTime};

use serde::Serialize;

/// Clock values recorded where an event occurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// Process-local time for active deadlines and elapsed durations.
    pub monotonic: Instant,
    /// Calendar time for deadlines that must survive a process restart.
    pub wall: SystemTime,
}

/// An event paired with its occurrence time before it enters the event queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedEvent {
    /// When the event occurred, independently of when the runner handles it.
    pub occurred_at: ClockReading,
    /// The command or observed result.
    pub event: Event,
}

/// An incoming command or observed result handled by the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Event {
    /// Startup commands and storage availability.
    Startup(StartupEvent),
    /// MAM pause commands and preference storage results.
    MamPause(MamPauseEvent),
    /// Dashboard settings commands and their storage results.
    Settings(SettingsEvent),
    /// History storage results.
    History(HistoryEvent),
    /// A scheduled timer expired.
    TimerExpired {
        /// Which timer.
        name: TimerName,
        /// Which instance; obsolete instances are ignored.
        id: TimerId,
    },
    /// Proton VPN port-forwarding commands and NAT-PMP results.
    PortForwarding(PortForwardingEvent),
    /// qBittorrent commands and web UI API results.
    Qbittorrent(QbittorrentEvent),
    /// Torrent registry storage results.
    Registry(RegistryEvent),
}

/// An effect requested by the core for the shell to execute.
///
/// The core permits only one outstanding storage operation for the MAM
/// preference, so its completion is unambiguous without an operation identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// Read the saved dashboard settings, reporting absent values separately.
    ReadSettings,
    /// Persist the history limit.
    WriteHistoryLimit {
        /// The limit to store.
        limit: HistoryLimit,
    },
    /// Persist the API cookie, or remove it when absent.
    WriteApiCookie {
        /// The cookie to store, if any.
        cookie: Option<ApiCookie>,
    },
    /// Open history storage and record entries buffered until now.
    OpenHistory,
    /// Remove the oldest history entries, keeping the newest `keep`.
    TrimHistory {
        /// How many entries to keep.
        keep: EntryCount,
    },
    /// Start a timer, replacing any running timer with the same name.
    StartTimer {
        /// Which timer.
        name: TimerName,
        /// This instance's identity, reported back on expiry.
        id: TimerId,
        /// When it expires.
        deadline: Deadline,
    },
    /// Ask the NAT-PMP gateway to map TCP and UDP ports for this long.
    RequestPortMapping {
        /// The requested lease lifetime.
        lifetime: std::time::Duration,
    },
    /// Read qBittorrent's version and preferences.
    ConnectQbittorrent,
    /// Read qBittorrent's changes since this position.
    SyncQbittorrent {
        /// The position from the previous update.
        position: SyncPosition,
    },
    /// Read qBittorrent's preferences.
    ReadQbittorrentPreferences,
    /// Set the port qBittorrent listens on for peers.
    SetQbittorrentListenPort {
        /// The forwarded port.
        port: Port,
    },
    /// Change qBittorrent to match the torrent registry.
    QbittorrentCommand {
        /// The change.
        command: TorrentCommand,
    },
    /// Read every torrent registry row.
    ReadTorrentRegistry,
    /// Insert or replace these registry rows in one transaction.
    SaveTorrents {
        /// The rows as they are now.
        records: Vec<TorrentRecord>,
    },
}

mod history;
mod machine;
mod mam_pause;
mod port_forwarding;
mod qbittorrent;
mod registry;
mod settings;
mod startup;
mod timer;

pub use history::{HistoryEvent, HistoryState, Step, Trim};
pub use machine::{Core, State};
pub use mam_pause::{
    MamPauseEvent, MamPreferencePersistence, MamPreferenceState, MamPreferenceUpdate,
    MamRequestMode, MamRequestsStatus,
};
pub use port_forwarding::{
    LEASE_LIFETIME, Lease, LeaseRequest, MappingFailure, Port, PortForwarding, PortForwardingEvent,
};
pub use qbittorrent::{
    Activity, ByteSize, ClientVersion, MamApproval, POLL_INTERVAL, Progress, Qbittorrent,
    QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences, Ratio, Session, SyncPosition,
    SyncUpdate, Torrent, TorrentCommand, TorrentHash, TorrentState, TorrentUpdate,
};
pub use registry::{
    COUNTER_SAVE_INTERVAL, Desired, Identity, MamTorrentId, Observation, Origin, RecordId, Records,
    Registry, RegistryEvent, TorrentRecord,
};
pub use settings::{
    ApiCookie, EntryCount, HistoryLimit, HistoryLimitError, InvalidApiCookie, Setting,
    SettingPersistence, SettingStatus, Settings, SettingsEvent, SettingsState,
};
pub use startup::{StartupEvent, StartupMode};
pub use timer::{Deadline, TimerId, TimerName};
