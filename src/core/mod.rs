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
    /// VPN tunnel commands, checks, and switches.
    Tunnel(TunnelEvent),
    /// Notification commands and delivery results.
    Notification(NotificationEvent),
    /// MAM answers and MAM state storage results.
    Mam(MamEvent),
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
    /// Ask a neutral service for the tunnel's public address.
    CheckExitIp,
    /// Read the last VPN server switch.
    ReadLastVpnSwitch,
    /// Save a VPN server switch before starting it.
    SaveVpnSwitch {
        /// The switch.
        switch: VpnSwitch,
    },
    /// Start the NixOS unit that connects to this server.
    SwitchVpnServer {
        /// The server.
        server: VpnServer,
    },
    /// Persist the ntfy topic, or remove it when absent.
    WriteNtfyTopic {
        /// The topic to store, if any.
        topic: Option<NtfyTopic>,
    },
    /// Push a notice to the ntfy topic: only a link to its entry in the log.
    SendNotification {
        /// The topic.
        topic: NtfyTopic,
        /// The notice and its entry.
        push: Push,
    },
    /// Read the newest id in the notifications log.
    ReadNoticeLog,
    /// Save a notice to the notifications log.
    SaveNotice {
        /// The notice and its place in the log.
        entry: Logged,
        /// Older notices up to this id are forgotten.
        forget_through: Option<NoticeId>,
    },
    /// Persist the points reserve.
    WritePointsReserve {
        /// The reserve to store.
        reserve: PointsReserve,
    },
    /// Set qBittorrent's queue.
    SetQbittorrentQueue {
        /// The queue settings.
        queue: QueuePreferences,
    },
    /// Persist the torrent limits.
    WriteTorrentLimits {
        /// The limits to store.
        limits: TorrentLimits,
    },
    /// Persist a cookie MAM issued in place of the saved one.
    WriteRotatedApiCookie {
        /// The cookie.
        cookie: ApiCookie,
    },
    /// Read the saved MAM state: the last registration attempt and reminder.
    ReadMamState,
    /// Save a seedbox registration attempt before sending it.
    SaveRegistrationAttempt {
        /// When it is made.
        at: SystemTime,
    },
    /// Save when the owner was last reminded to visit MAM.
    SaveReminder {
        /// When.
        at: SystemTime,
    },
    /// Send a request to MAM. A fetched torrent file is kept for qBittorrent.
    MamRequest {
        /// The request.
        request: MamRequest,
        /// The cookie to send, for requests that need one.
        cookie: Option<ApiCookie>,
    },
}

mod failures;
mod history;
mod http_status;
mod machine;
mod mam;
mod mam_pause;
mod notifications;
mod port_forwarding;
mod qbittorrent;
mod registry;
mod settings;
mod startup;
mod timer;
mod tunnel;

pub use failures::FailureCount;
pub use history::{HistoryEvent, HistoryState, Step, Trim};
pub use http_status::HttpStatus;
pub use machine::{Core, State};
pub use mam::{
    ACCOUNT_FRESH_FOR, ACCOUNT_READ_EVERY, Account, Acquisition, Asn, Attention, BOOKMARKS_EVERY,
    BookTitle, BookTitleError, Bookmark, BookmarkCount, BookmarkPage, Bookmarks, Budget,
    CLIENT_STATS_EVERY, CategoryName, CategoryNameError, ClassName, ClassNameError, ClientList,
    ClientListing, ClientRefusal, ClientStanding, Connectability, Connectable, Connection,
    ConnectionEpoch, DownloadWatch, FetchedTorrent, Findings, Freeleech, HISTORY_FRESH_FOR, Hold,
    IpFinding, IpObservation, IpStanding, LOOKUP_FRESH_FOR, LOOKUP_SPACING, LastAttempt,
    LastReminder, LeaseRenewal, Listed, ListedClient, Lookup, MAX_BOOKMARK_PAGES, Mam, MamEvent,
    MamFailure, MamHosts, MamListing, MamMessage, MamMessageError, MamNotices, MamReply,
    MamRequest, Network, NetworkName, NetworkNameError, NoticeCount, OBSERVE_EVERY, Observed,
    Outstanding, Partials, PersonalFreeleech, Points, PortFinding, Purchase, PurchaseOutcome,
    PurchaseRecord, RATIO_FLOOR, RATIO_TARGET, RATIO_URGENT, RECENT_ADMISSION,
    REGISTRATION_SPACING, REMINDER_EVERY, REPAIR_SETTLES_AFTER, REQUEST_BURST, REQUEST_REFILL,
    Reading, RedCategory, RedKind, Registration, RegistrationAllowance, RegistrationFailure,
    RequestGate, Sale, Seedbox, SeedboxConfirmation, SeedboxStatus, Session as MamSession,
    SessionRejection, SettledBy, SnatchSummary, Snatched, Tokens, TorrentCount,
    UNSATISFIED_RESERVE, UPLOAD_BUFFER, UPLOAD_PRICE_PER_GIB, UPLOAD_STEP, UnsatisfiedCount,
    UnsatisfiedLimit, UploadCredit, UploadCreditError, UserClass, UserData, VIP_FILL,
    VIP_PRICE_PER_WEEK, VIP_RENEW_WITHIN, VIP_WARN_WITHIN, VipTorrent, WATCH_FRESH_FOR,
    WEDGE_RECORDED_AFTER, Wedge, WedgeAccounting, WedgeCount, WedgeHistory, WedgeSpent,
};
pub use mam_pause::{
    MamPauseEvent, MamPreferencePersistence, MamPreferenceState, MamPreferenceUpdate,
    MamRequestMode, MamRequestsStatus,
};
pub use notifications::{
    ClientWarning, Condition, Delivery, DeliveryFailure, LOG_KEPT, Logged, NotFreeOutcome, Notice,
    NoticeId, NoticeLog, NotificationEvent, Notifications, OUTBOX_LIMIT, Outcome, Phase, Push,
    PushCount, Reach, SettingName, SwitchBlock, Urgency,
};
pub use port_forwarding::{
    LEASE_LIFETIME, Lease, LeaseRequest, MappingFailure, NatPmpRefusal, Port, PortForwarding,
    PortForwardingEvent,
};
pub use qbittorrent::{
    Activity, ByteSize, ClientVersion, Completion, ContentPath, ContentPathError, DecidingAfter,
    ForceStart, IdleTorrents, MamApproval, NetworkInterface, NetworkInterfaceError, POLL_INTERVAL,
    Progress, Qbittorrent, QbittorrentEvent, QbittorrentFailure, QbittorrentPreferences,
    QueueLimit, QueuePreferences, Queueing, Ratio, ReportedVersion, SavePath, SavePathError,
    Session, SyncPosition, SyncScope, SyncUpdate, Torrent, TorrentCommand, TorrentHash,
    TorrentName, TorrentNameError, TorrentState, TorrentUpdate, VersionText, VersionTextError,
    Wanted,
};
pub use registry::{
    COUNTER_SAVE_INTERVAL, ContentDeletion, Desired, Download, Identity, MamTorrentId, Observation,
    Origin, RecordId, Records, Registry, RegistryEvent, SEEDING_REQUIRED, SEEDING_TARGET,
    TorrentRecord,
};
pub use settings::{
    ActiveLimit, ActiveLimitError, ApiCookie, DownloadingLimit, DownloadingLimitError, EntryCount,
    FewerActiveThanDownloading, HistoryLimit, HistoryLimitError, InvalidApiCookie,
    InvalidNtfyTopic, NtfyTopic, PointsReserve, PointsReserveError, Setting, SettingPersistence,
    SettingStatus, Settings, SettingsEvent, SettingsState, TorrentLimits,
};
pub use startup::{StartupEvent, StartupMode};
pub use timer::{Deadline, TimerId, TimerName};
pub use tunnel::{
    CHECK_INTERVAL, Check, ExitIpFailure, FAILURES_BEFORE_SWITCH, Failover, LastSwitch, Protection,
    RETRY_AFTER, SWITCH_SPACING, SwitchAllowance, Tunnel, TunnelEvent, VpnServer, VpnServerError,
    VpnSwitch, Watch,
};
