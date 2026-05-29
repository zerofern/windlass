#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::{AuthCookie, MamTorrentId, TorrentHash, TorrentRecord, TorrentState, VpnPort};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QbitConfig {
    pub auth_retry: Duration,
    pub sync_retry: Duration,
    pub torrent_refresh: Duration,
    /// Minimum seed time required to satisfy the `HnR` rule.
    /// Defaults to 72 hours (MAM rules 2.5 & 2.7).
    pub hnr_seed_time: Duration,
    /// Maximum number of unsatisfied torrents allowed by the user's MAM class
    /// (MAM Rule 2.8).  `0` means the gate is disabled (no limit enforced).
    /// Default for production: 100 (MAM Power User class cap).
    pub unsatisfied_quota_limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitCommand {
    EnsureAuthenticated,
    EnsureListenPort {
        port: VpnPort,
    },
    RefreshTorrents,
    PauseTorrent {
        hash: TorrentHash,
    },
    ResumeTorrent {
        hash: TorrentHash,
    },
    /// Request deletion of a torrent. Blocked if the torrent is HnR-unsatisfied.
    DeleteTorrent {
        hash: TorrentHash,
    },
    /// Evict the single highest-ranked HnR-satisfied torrent to relieve disk
    /// pressure.
    ///
    /// Placeholder rank: longest `seed_time` first (descending) among all
    /// HnR-satisfied torrents known to the machine.  At most one
    /// `DeleteTorrent` action is emitted — the caller re-evaluates after the
    /// next disk observation.
    ///
    /// The four real rank classes (completed+low-rating, DNF,
    /// completed+high-rating+long-since-listened, unstarted+low-AI-score)
    /// require librarian data outside operator scope and are deferred.
    EvictOneForDiskPressure,
    /// §29: add a torrent to qBittorrent.  Only emitted by the qBit core
    /// when the domain's composite admission predicate authorises the add
    /// (DOM-17).  The qBit core does not re-check the gates — admission is
    /// owned by the domain.  qBit core simply requires a cookie (QBIT-1).
    AddTorrent {
        mam_id: MamTorrentId,
        dl_url: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitTimer {
    AuthRetry,
    SyncRetry,
    TorrentRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitEvent {
    Init,
    AuthSucceeded {
        cookie: AuthCookie,
    },
    AuthFailed {
        reason: String,
    },
    PreferencesRead {
        listen_port: Option<VpnPort>,
        /// Whether DHT is enabled — banned on private trackers (MAM Rule 6.1).
        dht: bool,
        /// Whether Peer Exchange (`PeX`) is enabled — banned on private trackers (MAM Rule 6.1).
        pex: bool,
        /// Whether Local Service Discovery is enabled — banned on private trackers (MAM Rule 6.1).
        lsd: bool,
        /// Maximum number of simultaneously active torrents.
        ///
        /// `u32::MAX` means "no limit" (mirrors qBittorrent's negative value
        /// for unlimited, and is also the safe default for the legacy bridge so
        /// that events from the old code path never trigger orchestration).
        max_active_torrents: u32,
    },
    PreferencesFailed {
        reason: String,
    },
    ListenPortSet {
        port: VpnPort,
    },
    ListenPortSetFailed {
        port: VpnPort,
        reason: String,
    },
    /// Full torrent listing from qBittorrent, including compliance data.
    TorrentsListed {
        torrents: Vec<TorrentRecord>,
    },
    /// Shell successfully disabled DHT, `PeX`, and LSD.
    PrivacySettingsDisabled,
    /// Shell failed to disable banned privacy settings.
    PrivacySettingsDisableFailed {
        reason: String,
    },
    TimerFired(QbitTimer),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitAction {
    Login,
    ReadPreferences {
        cookie: AuthCookie,
    },
    SetListenPort {
        cookie: AuthCookie,
        port: VpnPort,
    },
    ListTorrents {
        cookie: AuthCookie,
    },
    PauseTorrent {
        cookie: AuthCookie,
        hash: TorrentHash,
    },
    ResumeTorrent {
        cookie: AuthCookie,
        hash: TorrentHash,
    },
    /// Delete a torrent from qBittorrent. Only emitted when the `HnR` lock permits.
    DeleteTorrent {
        cookie: AuthCookie,
        hash: TorrentHash,
    },
    /// Set all files in a torrent to download (MAM "no partials" rule — §21).
    /// Emitted once for every newly-seen torrent hash when a cookie is present.
    SetAllFilesPriority {
        cookie: AuthCookie,
        hash: TorrentHash,
    },
    /// Disable DHT, `PeX`, and LSD on qBittorrent (MAM Rule 6.1 — §23).
    /// Emitted when any of these settings is observed as `true`.
    DisableBannedPrivacySettings {
        cookie: AuthCookie,
    },
    /// Force-resume a torrent, bypassing seeding ratio/time limits (§24).
    /// Emitted by the queue-orchestration path to wake an HnR-unsatisfied
    /// torrent that was parked by the active-torrent limit.
    ForceResumeTorrent {
        cookie: AuthCookie,
        hash: TorrentHash,
    },
    /// §29: add a torrent to qBittorrent.  Only emitted in response to a
    /// `QbitCommand::AddTorrent` that arrives via the domain's admission
    /// gate.  Requires a cookie (QBIT-1).
    AddTorrent {
        cookie: AuthCookie,
        mam_id: MamTorrentId,
        dl_url: String,
    },
    ScheduleTimer {
        timer: QbitTimer,
        after: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitPublish {
    Ready,
    Unavailable {
        reason: String,
    },
    ListenPortReady {
        port: VpnPort,
    },
    TorrentsUpdated {
        hashes: Vec<TorrentHash>,
    },
    /// Published when the qBit core authorises and emits a delete for a dead
    /// (zero-byte, stalled/errored/paused) torrent.  Subscribers — primarily
    /// the domain core — use this to blacklist the MAM ID in the DB.
    DeadTorrentRemoved {
        hash: TorrentHash,
        mam_id: Option<MamTorrentId>,
    },
    /// Published when a `PreferencesRead` reveals that at least one of DHT, `PeX`,
    /// or LSD is enabled — a MAM Rule 6.1 violation.  The domain core uses this
    /// to fire a `Critical` alert.
    BannedPrivacySettingsObserved {
        dht: bool,
        pex: bool,
        lsd: bool,
    },
    /// Published when queue orchestration fires: the machine paused a satisfied
    /// seeder and force-resumed a parked unsatisfied torrent to protect `HnR` (§24).
    /// Subscribers (primarily the domain core) use this to record activity.
    QueueOrchestrated {
        paused: TorrentHash,
        force_resumed: TorrentHash,
    },
    /// Published when the unsatisfied-torrent count meets or exceeds
    /// `config.unsatisfied_quota_limit` (MAM Rule 2.8 — §25).  The domain
    /// core turns this into a `Critical` alert and activity entry.
    UnsatisfiedQuotaCritical {
        unsatisfied: u32,
        limit: u32,
    },
    /// Published when the unsatisfied-torrent count is within 5 of
    /// `config.unsatisfied_quota_limit` but has not yet reached it (§25).
    /// The domain core turns this into a `Warning` alert and activity entry.
    UnsatisfiedQuotaApproaching {
        unsatisfied: u32,
        limit: u32,
    },
    /// §29: positive counterpart to `UnsatisfiedQuotaCritical/Approaching`.
    /// Published on every `TorrentsListed` where the unsatisfied count is
    /// strictly below `limit - 5` (i.e. outside both the critical and the
    /// approaching bands).  Gives the domain a rising-edge positive signal
    /// for the admission-gate state.
    UnsatisfiedQuotaOk {
        unsatisfied: u32,
        limit: u32,
    },
    /// §29: positive counterpart to `BannedPrivacySettingsObserved`.
    /// Published on every `PreferencesRead` where DHT, `PeX`, and LSD are
    /// all `false`.  Gives the domain a rising-edge positive signal for the
    /// admission-gate state.
    PrivacyClean,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitTopic {
    Availability,
    ListenPort,
    Torrents,
    /// Privacy-settings violations (MAM Rule 6.1 — §23).
    Privacy,
    /// Queue-orchestration events (§24): active-limit management.
    Queue,
    /// Unsatisfied-torrent quota events (MAM Rule 2.8 — §25).
    Quota,
}

impl HasTopic<QbitTopic> for QbitPublish {
    fn topic(&self) -> QbitTopic {
        match self {
            Self::Ready | Self::Unavailable { .. } => QbitTopic::Availability,
            Self::ListenPortReady { .. } => QbitTopic::ListenPort,
            // `DeadTorrentRemoved` is routed on `Torrents` so the domain's
            // existing `Torrents` subscription delivers it without a new topic.
            Self::TorrentsUpdated { .. } | Self::DeadTorrentRemoved { .. } => QbitTopic::Torrents,
            Self::BannedPrivacySettingsObserved { .. } | Self::PrivacyClean => QbitTopic::Privacy,
            Self::QueueOrchestrated { .. } => QbitTopic::Queue,
            Self::UnsatisfiedQuotaCritical { .. }
            | Self::UnsatisfiedQuotaApproaching { .. }
            | Self::UnsatisfiedQuotaOk { .. } => QbitTopic::Quota,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitResponse {
    Accepted,
}

/// Last-observed state of the three privacy settings banned by MAM Rule 6.1.
/// Grouped to avoid triggering `clippy::struct_excessive_bools` on `QbitMachine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PrivacySettings {
    dht: bool,
    pex: bool,
    lsd: bool,
}

impl PrivacySettings {
    /// Returns `true` if any banned setting is enabled.
    const fn any_banned(self) -> bool {
        self.dht || self.pex || self.lsd
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QbitMachine {
    config: QbitConfig,
    cookie: Option<AuthCookie>,
    listen_port: Option<VpnPort>,
    desired_listen_port: Option<VpnPort>,
    /// True once the self-perpetuating `TorrentRefresh` timer chain has been started.
    /// Prevents a second independent chain from being spawned on re-authentication.
    refresh_scheduled: bool,
    /// Per-torrent state updated on every `TorrentsListed` event.
    torrents: HashMap<TorrentHash, TorrentRecord>,
    /// Last-observed privacy settings (MAM Rule 6.1 — all must be false).
    privacy: PrivacySettings,
    /// Maximum number of simultaneously active torrents, as last observed from
    /// qBittorrent preferences.
    ///
    /// Initialised to `u32::MAX` ("no limit") so orchestration never fires until
    /// a real `PreferencesRead` event sets a concrete value.  The legacy bridge
    /// also defaults this to `u32::MAX` so bridged events cannot trigger
    /// orchestration on the old code path.
    max_active_torrents: u32,
}

impl QbitMachine {
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.cookie.is_some()
    }

    #[must_use]
    pub const fn listen_port(&self) -> Option<VpnPort> {
        self.listen_port
    }

    /// Returns whether the torrent with the given hash satisfies the `HnR` seeding
    /// requirement, or `None` if the hash is not in the known torrent map.
    ///
    /// A torrent is `HnR`-satisfied iff:
    /// - `downloaded_bytes == 0` (nothing was downloaded, so no seeding obligation), or
    /// - `seed_time >= config.hnr_seed_time` (the required seed window has elapsed).
    #[must_use]
    pub fn hnr_satisfied(&self, hash: &TorrentHash) -> Option<bool> {
        self.torrents
            .get(hash)
            .map(|t| t.downloaded_bytes == 0 || t.seed_time >= self.config.hnr_seed_time)
    }

    /// The single authorisation gate for all torrent-deletion paths.
    ///
    /// Returns a `DeleteTorrent` action only when:
    /// - there is an active cookie (qBit is connected), AND
    /// - the torrent is NOT a known HnR-unsatisfied torrent.
    ///
    /// An unknown torrent (not in the map) is treated as deletable, mirroring the
    /// legacy `on_delete_torrent_requested` semantics.  A known torrent with
    /// `downloaded_bytes > 0 && seed_time < hnr_seed_time` is blocked.
    fn authorize_delete(&self, hash: &TorrentHash) -> Vec<QbitAction> {
        let Some(cookie) = self.cookie.clone() else {
            return Vec::new();
        };
        // Block if known and HnR-unsatisfied; allow if unknown or satisfied.
        if let Some(t) = self.torrents.get(hash)
            && t.downloaded_bytes > 0
            && t.seed_time < self.config.hnr_seed_time
        {
            return Vec::new();
        }
        vec![QbitAction::DeleteTorrent {
            cookie,
            hash: hash.clone(),
        }]
    }

    /// Select and authorise the deletion of the single highest-ranked
    /// HnR-satisfied torrent, using the placeholder rank of longest `seed_time`
    /// first.
    ///
    /// Returns at most one `DeleteTorrent` action (QBIT-11).  If there are no
    /// satisfied candidates, or no cookie is present, returns an empty vec.
    fn evict_one_for_disk_pressure(&self) -> Vec<QbitAction> {
        // Select the HnR-satisfied torrent with the longest seed_time.
        let candidate = self
            .torrents
            .values()
            .filter(|t| t.downloaded_bytes == 0 || t.seed_time >= self.config.hnr_seed_time)
            .max_by_key(|t| t.seed_time);

        candidate.map_or_else(Vec::new, |t| self.authorize_delete(&t.hash.clone()))
    }

    /// Returns `true` when the torrent's state and download size classify it as
    /// "dead" for the purposes of the zero-byte dead-torrent cleanup path
    /// (story 20).
    ///
    /// A torrent is dead when:
    /// - `downloaded_bytes == 0` (nothing was downloaded), **and**
    /// - `state` is one of the stalled / error / paused variants.
    const fn is_dead(record: &TorrentRecord) -> bool {
        record.downloaded_bytes == 0
            && matches!(
                record.state,
                TorrentState::StalledDownloading
                    | TorrentState::StalledUploading
                    | TorrentState::Error
                    | TorrentState::PausedDownloading
                    | TorrentState::PausedUploading
            )
    }

    /// Queue-orchestration step (§24 / QBIT-14/15/16).
    ///
    /// When `active_count >= max_active_torrents`, finds a parked HnR-unsatisfied
    /// torrent and the oldest HnR-satisfied active seeder, then pauses the satisfied
    /// seeder and force-resumes the unsatisfied one.
    ///
    /// Iteration over the torrent map is non-deterministic, so candidates are
    /// sorted by `TorrentHash` before selection.  This gives a stable, reproducible
    /// pick across runs for the same map contents.
    ///
    /// Returns `(actions, publishes)`.  Both vecs are empty unless orchestration
    /// actually fires.
    fn orchestrate_queue(&self) -> (Vec<QbitAction>, Vec<QbitPublish>) {
        let Some(cookie) = self.cookie.clone() else {
            return (Vec::new(), Vec::new());
        };

        // Count active torrents in the current map.
        let active_count = u32::try_from(
            self.torrents
                .values()
                .filter(|t| t.state.is_active())
                .count(),
        )
        .unwrap_or(u32::MAX);

        if active_count < self.max_active_torrents {
            return (Vec::new(), Vec::new());
        }

        // Collect and sort candidates for determinism.
        let mut all_torrents: Vec<&TorrentRecord> = self.torrents.values().collect();
        all_torrents.sort_by(|a, b| a.hash.0.cmp(&b.hash.0));

        // Parked: unsatisfied torrent in PausedUploading or StalledUploading.
        let parked = all_torrents.iter().find(|t| {
            t.downloaded_bytes > 0
                && t.seed_time < self.config.hnr_seed_time
                && matches!(
                    t.state,
                    TorrentState::PausedUploading | TorrentState::StalledUploading
                )
        });

        let Some(parked) = parked else {
            return (Vec::new(), Vec::new());
        };

        // Oldest satisfied seeder: seed_time >= hnr_seed_time and state == Uploading.
        let oldest_satisfied = all_torrents
            .iter()
            .filter(|t| {
                t.seed_time >= self.config.hnr_seed_time
                    && matches!(t.state, TorrentState::Uploading)
            })
            .max_by_key(|t| t.seed_time);

        let Some(oldest_satisfied) = oldest_satisfied else {
            return (Vec::new(), Vec::new());
        };

        let actions = vec![
            QbitAction::PauseTorrent {
                cookie: cookie.clone(),
                hash: oldest_satisfied.hash.clone(),
            },
            QbitAction::ForceResumeTorrent {
                cookie,
                hash: parked.hash.clone(),
            },
        ];
        let publishes = vec![QbitPublish::QueueOrchestrated {
            paused: oldest_satisfied.hash.clone(),
            force_resumed: parked.hash.clone(),
        }];

        (actions, publishes)
    }

    fn retry_listen_port_or_read_preferences(&self) -> Vec<QbitAction> {
        let Some(cookie) = self.cookie.clone() else {
            return vec![QbitAction::Login];
        };
        match self.desired_listen_port {
            None => vec![QbitAction::ReadPreferences { cookie }],
            Some(port) => vec![QbitAction::SetListenPort { cookie, port }],
        }
    }

    fn converge_listen_port(&self) -> Vec<QbitAction> {
        let Some(port) = self.desired_listen_port else {
            return Vec::new();
        };
        if self.listen_port == Some(port) {
            return Vec::new();
        }
        self.cookie.clone().map_or_else(
            || vec![QbitAction::Login],
            |cookie| vec![QbitAction::SetListenPort { cookie, port }],
        )
    }

    fn listen_port_publish(&self, listen_port: Option<VpnPort>) -> Vec<QbitPublish> {
        listen_port
            .filter(|port| {
                self.desired_listen_port
                    .is_none_or(|desired_port| desired_port == *port)
            })
            .map(|port| QbitPublish::ListenPortReady { port })
            .into_iter()
            .collect()
    }

    /// Count of torrents in `self.torrents` that are HnR-unsatisfied:
    /// `downloaded_bytes > 0 && seed_time < config.hnr_seed_time`.
    ///
    /// Returns `u32::MAX` on overflow (impossible with typical torrent counts, but
    /// saturating is safer than panicking).
    #[must_use]
    pub fn unsatisfied_count(&self) -> u32 {
        u32::try_from(
            self.torrents
                .values()
                .filter(|t| t.downloaded_bytes > 0 && t.seed_time < self.config.hnr_seed_time)
                .count(),
        )
        .unwrap_or(u32::MAX)
    }

    /// Returns `true` iff the unsatisfied-quota gate is enabled
    /// (`config.unsatisfied_quota_limit > 0`) and the current unsatisfied count
    /// has met or exceeded the limit.
    ///
    /// Story 29 will consume this as a fail-closed admission predicate.
    #[must_use]
    pub fn unsatisfied_quota_full(&self) -> bool {
        self.config.unsatisfied_quota_limit > 0
            && self.unsatisfied_count() >= self.config.unsatisfied_quota_limit
    }

    /// Evaluate the unsatisfied-quota state and return any quota publish (§25 /
    /// QBIT-17/18).  Returns an empty vec when the gate is disabled (`limit == 0`).
    /// §29: also returns the positive `UnsatisfiedQuotaOk` publish when the count
    /// is safely below the warning threshold, so the domain admission state has a
    /// rising-edge positive signal.
    fn quota_publish(&self) -> Vec<QbitPublish> {
        let limit = self.config.unsatisfied_quota_limit;
        if limit == 0 {
            return Vec::new();
        }
        let unsatisfied = self.unsatisfied_count();
        if unsatisfied >= limit {
            vec![QbitPublish::UnsatisfiedQuotaCritical { unsatisfied, limit }]
        } else if unsatisfied >= limit.saturating_sub(5) {
            vec![QbitPublish::UnsatisfiedQuotaApproaching { unsatisfied, limit }]
        } else {
            vec![QbitPublish::UnsatisfiedQuotaOk { unsatisfied, limit }]
        }
    }
}

impl Machine for QbitMachine {
    type Config = QbitConfig;
    type Event = QbitEvent;
    type Action = QbitAction;
    type Publish = QbitPublish;
    type Topic = QbitTopic;
    type Command = QbitCommand;
    type Response = QbitResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            cookie: None,
            listen_port: None,
            desired_listen_port: None,
            refresh_scheduled: false,
            torrents: HashMap::new(),
            privacy: PrivacySettings::default(),
            // Initialised to MAX so orchestration is disabled until a real
            // PreferencesRead arrives.
            max_active_torrents: u32::MAX,
        }
    }

    // Each event arm is a small, self-contained decision; the function is long
    // because the event set is large, not because any single arm is complex.
    #[allow(clippy::too_many_lines)]
    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            QbitEvent::Init | QbitEvent::TimerFired(QbitTimer::AuthRetry) => Outcome {
                actions: vec![QbitAction::Login],
                publish: Vec::new(),
            },
            QbitEvent::AuthSucceeded { cookie } => {
                self.cookie = Some(cookie.clone());
                let mut actions = self.desired_listen_port.map_or_else(
                    || {
                        vec![QbitAction::ReadPreferences {
                            cookie: cookie.clone(),
                        }]
                    },
                    |port| {
                        vec![QbitAction::SetListenPort {
                            cookie: cookie.clone(),
                            port,
                        }]
                    },
                );
                if !self.refresh_scheduled {
                    self.refresh_scheduled = true;
                    actions.push(QbitAction::ScheduleTimer {
                        timer: QbitTimer::TorrentRefresh,
                        after: self.config.torrent_refresh,
                    });
                }
                Outcome {
                    actions,
                    publish: vec![QbitPublish::Ready],
                }
            }
            QbitEvent::AuthFailed { reason } => Outcome {
                actions: vec![QbitAction::ScheduleTimer {
                    timer: QbitTimer::AuthRetry,
                    after: self.config.auth_retry,
                }],
                publish: vec![QbitPublish::Unavailable { reason }],
            },
            QbitEvent::PreferencesRead {
                listen_port,
                dht,
                pex,
                lsd,
                max_active_torrents,
            } => {
                self.listen_port = listen_port;
                self.privacy = PrivacySettings { dht, pex, lsd };
                self.max_active_torrents = max_active_torrents;

                let mut actions = self.converge_listen_port();
                let mut publish = self.listen_port_publish(listen_port);

                // QBIT-12: if any banned privacy setting is enabled, disable
                // them and publish the observation.  The disable action is only
                // emitted when a cookie is present (QBIT-1).  §29: when all
                // three are clean, publish the positive `PrivacyClean` signal
                // so the domain admission state can clear.
                if self.privacy.any_banned() {
                    if let Some(cookie) = self.cookie.clone() {
                        actions.push(QbitAction::DisableBannedPrivacySettings { cookie });
                    }
                    publish.push(QbitPublish::BannedPrivacySettingsObserved { dht, pex, lsd });
                } else {
                    publish.push(QbitPublish::PrivacyClean);
                }

                Outcome { actions, publish }
            }
            // All retryable failures: schedule one SyncRetry, publish Unavailable.
            // QBIT-5 (PreferencesFailed / ListenPortSetFailed) and QBIT-13
            // (PrivacySettingsDisableFailed) share this arm because the action is identical.
            QbitEvent::PreferencesFailed { reason }
            | QbitEvent::ListenPortSetFailed { reason, .. }
            | QbitEvent::PrivacySettingsDisableFailed { reason } => Outcome {
                actions: vec![QbitAction::ScheduleTimer {
                    timer: QbitTimer::SyncRetry,
                    after: self.config.sync_retry,
                }],
                publish: vec![QbitPublish::Unavailable { reason }],
            },
            QbitEvent::ListenPortSet { port } => {
                self.listen_port = Some(port);
                // Route through the desired-port filter so QBIT-4 holds for any
                // event, not just well-formed shell events. (Story 18.)
                Outcome {
                    actions: Vec::new(),
                    publish: self.listen_port_publish(Some(port)),
                }
            }
            QbitEvent::TorrentsListed { torrents } => {
                let hashes: Vec<TorrentHash> = torrents.iter().map(|t| t.hash.clone()).collect();

                // Compute newly-seen hashes BEFORE replacing the map (§21 / QBIT-10).
                // A hash is newly-seen when it is not already a key in self.torrents.
                let newly_seen: Vec<TorrentHash> = torrents
                    .iter()
                    .filter(|t| !self.torrents.contains_key(&t.hash))
                    .map(|t| t.hash.clone())
                    .collect();

                // Collect dead-torrent metadata before the map is updated.
                let dead: Vec<(TorrentHash, Option<MamTorrentId>)> = torrents
                    .iter()
                    .filter(|t| Self::is_dead(t))
                    .map(|t| (t.hash.clone(), t.mam_id))
                    .collect();

                self.torrents = torrents.into_iter().map(|t| (t.hash.clone(), t)).collect();

                let mut actions = Vec::new();
                let mut publish = vec![QbitPublish::TorrentsUpdated { hashes }];

                // For each newly-seen torrent, emit SetAllFilesPriority to enforce
                // the MAM "no partials" rule (§21 / QBIT-10).  Mirrors legacy
                // `check_new_torrents` which ran before `check_dead_torrents`.
                if let Some(c) = &self.cookie {
                    for hash in newly_seen {
                        actions.push(QbitAction::SetAllFilesPriority {
                            cookie: c.clone(),
                            hash,
                        });
                    }
                }

                // For each dead torrent, attempt an authorised delete.  Only
                // emit `DeadTorrentRemoved` when the delete is actually
                // authorised (i.e. a cookie is present), so we never announce
                // a removal that didn't happen.
                for (hash, mam_id) in dead {
                    let delete_actions = self.authorize_delete(&hash);
                    if !delete_actions.is_empty() {
                        actions.extend(delete_actions);
                        publish.push(QbitPublish::DeadTorrentRemoved { hash, mam_id });
                    }
                }

                // Queue-orchestration step (§24 / QBIT-14/15/16):
                // if the active-torrent limit is reached and there is a parked
                // unsatisfied torrent, pause the oldest satisfied seeder and
                // force-resume the parked one.  Only runs when a cookie is present
                // (checked inside orchestrate_queue).
                let (orch_actions, orch_publish) = self.orchestrate_queue();
                actions.extend(orch_actions);
                publish.extend(orch_publish);

                // Quota evaluation step (§25 / QBIT-17/18): after the map is
                // replaced, evaluate the unsatisfied count against the configured
                // class limit.  Emits at most one quota publish per listing.
                publish.extend(self.quota_publish());

                Outcome { actions, publish }
            }
            // QBIT-12: success is a no-op — next PreferencesRead will confirm
            // the settings are now false.
            QbitEvent::PrivacySettingsDisabled => Outcome::none(),
            QbitEvent::TimerFired(QbitTimer::SyncRetry) => Outcome {
                actions: self.retry_listen_port_or_read_preferences(),
                publish: Vec::new(),
            },
            QbitEvent::TimerFired(QbitTimer::TorrentRefresh) => {
                let mut actions = self.cookie.clone().map_or_else(Vec::new, |cookie| {
                    // Piggyback a prefs read on every torrent-refresh cycle so
                    // banned privacy settings are caught continuously (§23).
                    vec![
                        QbitAction::ListTorrents {
                            cookie: cookie.clone(),
                        },
                        QbitAction::ReadPreferences { cookie },
                    ]
                });
                actions.push(QbitAction::ScheduleTimer {
                    timer: QbitTimer::TorrentRefresh,
                    after: self.config.torrent_refresh,
                });
                Outcome {
                    actions,
                    publish: Vec::new(),
                }
            }
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            QbitCommand::EnsureAuthenticated => vec![QbitAction::Login],
            QbitCommand::EnsureListenPort { port } => {
                self.desired_listen_port = Some(port);
                if self.listen_port == Some(port) {
                    return Self::outcome_with_publish(
                        Vec::new(),
                        vec![QbitPublish::ListenPortReady { port }],
                        QbitResponse::Accepted,
                    );
                }
                self.cookie.clone().map_or_else(
                    || vec![QbitAction::Login],
                    |cookie| vec![QbitAction::SetListenPort { cookie, port }],
                )
            }
            QbitCommand::RefreshTorrents => self
                .cookie
                .clone()
                .map_or_else(Vec::new, |cookie| vec![QbitAction::ListTorrents { cookie }]),
            QbitCommand::PauseTorrent { hash } => {
                self.cookie.clone().map_or_else(Vec::new, |cookie| {
                    vec![QbitAction::PauseTorrent { cookie, hash }]
                })
            }
            QbitCommand::ResumeTorrent { hash } => {
                self.cookie.clone().map_or_else(Vec::new, |cookie| {
                    vec![QbitAction::ResumeTorrent { cookie, hash }]
                })
            }
            QbitCommand::DeleteTorrent { hash } => {
                return Self::outcome(self.authorize_delete(&hash), QbitResponse::Accepted);
            }
            QbitCommand::EvictOneForDiskPressure => {
                return Self::outcome(self.evict_one_for_disk_pressure(), QbitResponse::Accepted);
            }
            // §29 / QBIT-1: only emit `AddTorrent` when a cookie is present.
            // Admission is owned by the domain (DOM-17) — the qBit core does
            // not re-check the §29 gates here.
            QbitCommand::AddTorrent { mam_id, dl_url } => {
                self.cookie.clone().map_or_else(Vec::new, |cookie| {
                    vec![QbitAction::AddTorrent {
                        cookie,
                        mam_id,
                        dl_url,
                    }]
                })
            }
        };
        Self::outcome(actions, QbitResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
    use windlass_types::{AuthCookie, TorrentHash, TorrentRecord, TorrentState, VpnPort};

    use crate::{
        QbitAction, QbitCommand, QbitConfig, QbitEvent, QbitMachine, QbitPublish, QbitTimer,
    };

    const HNR_SEED_TIME: Duration = Duration::from_secs(72 * 3600);

    fn machine() -> QbitMachine {
        QbitMachine::new(
            QbitConfig {
                auth_retry: Duration::from_secs(1),
                sync_retry: Duration::from_secs(2),
                torrent_refresh: Duration::from_secs(30),
                hnr_seed_time: HNR_SEED_TIME,
                unsatisfied_quota_limit: 0,
            },
            Instant::now(),
        )
    }

    fn record(hash: &TorrentHash, downloaded: u64, seed_secs: u64) -> TorrentRecord {
        TorrentRecord {
            hash: hash.clone(),
            downloaded_bytes: downloaded,
            seed_time: Duration::from_secs(seed_secs),
            state: TorrentState::Uploading,
            mam_id: None,
        }
    }

    fn handle(machine: &mut QbitMachine, event: QbitEvent) -> Outcome<QbitAction, QbitPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn init_logs_in() {
        let mut machine = machine();

        let out = handle(&mut machine, QbitEvent::Init);

        assert_eq!(out.actions, vec![QbitAction::Login]);
    }

    #[test]
    fn auth_success_publishes_ready_and_reads_preferences() {
        let mut machine = machine();

        let cookie = AuthCookie::new("sid".to_string());
        let out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        assert!(machine.is_authenticated());
        assert_eq!(
            out.actions,
            vec![
                QbitAction::ReadPreferences { cookie },
                QbitAction::ScheduleTimer {
                    timer: QbitTimer::TorrentRefresh,
                    after: Duration::from_secs(30),
                },
            ]
        );
        assert_eq!(out.publish, vec![QbitPublish::Ready]);
    }

    #[test]
    fn ensure_listen_port_requires_authentication() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        assert_eq!(out.actions, vec![QbitAction::Login]);
    }

    #[test]
    fn auth_success_sets_desired_port_after_pre_auth_request() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        let out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        assert_eq!(
            out.actions,
            vec![
                QbitAction::SetListenPort { cookie, port },
                QbitAction::ScheduleTimer {
                    timer: QbitTimer::TorrentRefresh,
                    after: Duration::from_secs(30),
                },
            ]
        );
        assert_eq!(out.publish, vec![QbitPublish::Ready]);
    }

    #[test]
    fn ensure_listen_port_carries_cookie_when_authenticated() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        let out = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        assert_eq!(
            out.actions,
            vec![QbitAction::SetListenPort { cookie, port }]
        );
    }

    #[test]
    fn preference_mismatch_sets_desired_port_without_publishing_ready() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let desired = VpnPort::try_new(51_820).unwrap();
        let observed = VpnPort::try_new(42_000).unwrap();
        let _ = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let _ = machine.handle_command(
            Instant::now(),
            QbitCommand::EnsureListenPort { port: desired },
        );

        let out = handle(
            &mut machine,
            QbitEvent::PreferencesRead {
                listen_port: Some(observed),
                dht: false,
                pex: false,
                lsd: false,
                max_active_torrents: u32::MAX,
            },
        );

        assert_eq!(
            out.actions,
            vec![QbitAction::SetListenPort {
                cookie,
                port: desired,
            }]
        );
        // §29: PreferencesRead with all three privacy settings off now
        // emits a positive PrivacyClean publish.
        assert_eq!(out.publish, vec![QbitPublish::PrivacyClean]);
    }

    #[test]
    fn set_failure_publishes_unavailable_and_retries_desired_port() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let _ = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        let failed = handle(
            &mut machine,
            QbitEvent::ListenPortSetFailed {
                port,
                reason: "forbidden".to_string(),
            },
        );

        assert_eq!(
            failed.actions,
            vec![QbitAction::ScheduleTimer {
                timer: QbitTimer::SyncRetry,
                after: Duration::from_secs(2),
            }]
        );
        assert_eq!(
            failed.publish,
            vec![QbitPublish::Unavailable {
                reason: "forbidden".to_string(),
            }]
        );

        let retry = handle(&mut machine, QbitEvent::TimerFired(QbitTimer::SyncRetry));

        assert_eq!(
            retry.actions,
            vec![QbitAction::SetListenPort { cookie, port }]
        );
    }

    #[test]
    fn ensure_listen_port_publishes_when_already_converged() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = handle(&mut machine, QbitEvent::ListenPortSet { port });

        let out = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        assert!(out.actions.is_empty());
        assert_eq!(out.publish, vec![QbitPublish::ListenPortReady { port }]);
    }

    #[test]
    fn listen_port_set_publishes_ready_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(&mut machine, QbitEvent::ListenPortSet { port });

        assert_eq!(machine.listen_port(), Some(port));
        assert_eq!(out.publish, vec![QbitPublish::ListenPortReady { port }]);
    }

    #[test]
    fn listen_port_set_filters_mismatched_desired_port() {
        // QBIT-4 / story 18: with desired=X, a ListenPortSet { port: Y } where Y != X
        // must NOT publish ListenPortReady, even though it records the port. (Dishonest
        // shell event defense — the machine filters through the desired-port gate.)
        let mut machine = machine();
        let desired = VpnPort::try_new(51_820).unwrap();
        let other = VpnPort::try_new(42_000).unwrap();

        // Set a desired port without authenticating (command records state regardless).
        let _ = machine.handle_command(
            Instant::now(),
            QbitCommand::EnsureListenPort { port: desired },
        );

        let out = handle(&mut machine, QbitEvent::ListenPortSet { port: other });

        assert_eq!(
            machine.listen_port(),
            Some(other),
            "listen_port must still be recorded"
        );
        assert!(
            out.publish.is_empty(),
            "must not publish ListenPortReady when port != desired_listen_port"
        );
    }

    #[test]
    fn torrent_refresh_timer_round_trips() {
        // Phase 1: AuthSucceeded schedules the TorrentRefresh timer.
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let auth_out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        assert!(
            auth_out.actions.contains(&QbitAction::ScheduleTimer {
                timer: QbitTimer::TorrentRefresh,
                after: Duration::from_secs(30),
            }),
            "AuthSucceeded must schedule TorrentRefresh timer"
        );

        // Phase 2: When the timer fires, ListTorrents is issued and the timer re-schedules.
        let fired_out = handle(
            &mut machine,
            QbitEvent::TimerFired(QbitTimer::TorrentRefresh),
        );
        assert!(
            fired_out
                .actions
                .contains(&QbitAction::ListTorrents { cookie }),
            "TorrentRefresh timer must issue ListTorrents"
        );
        assert!(
            fired_out.actions.contains(&QbitAction::ScheduleTimer {
                timer: QbitTimer::TorrentRefresh,
                after: Duration::from_secs(30),
            }),
            "TorrentRefresh timer must re-schedule itself"
        );
    }

    #[test]
    fn auth_succeeded_twice_schedules_refresh_timer_only_once() {
        // Two consecutive AuthSucceeded events (e.g. from dual-Init login race) must not
        // produce a second independent TorrentRefresh timer chain.
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());

        let first_out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let schedule_action = QbitAction::ScheduleTimer {
            timer: QbitTimer::TorrentRefresh,
            after: Duration::from_secs(30),
        };
        assert!(
            first_out.actions.contains(&schedule_action),
            "first AuthSucceeded must schedule TorrentRefresh"
        );

        let second_out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        assert!(
            !second_out.actions.contains(&schedule_action),
            "second AuthSucceeded must NOT schedule a second TorrentRefresh chain"
        );
    }

    // ── HnR seed-time lock unit tests (QBIT-8) ───────────────────────────────

    fn authenticated_machine() -> (QbitMachine, AuthCookie) {
        let mut m = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let _ = m.handle(
            Instant::now(),
            Timed::now(QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            }),
        );
        (m, cookie)
    }

    fn load_torrent(m: &mut QbitMachine, r: TorrentRecord) {
        let _ = m.handle(
            Instant::now(),
            Timed::now(QbitEvent::TorrentsListed { torrents: vec![r] }),
        );
    }

    #[test]
    fn hnr_unsatisfied_torrent_blocks_delete() {
        // A torrent with downloaded_bytes > 0 and seed_time < 72h must NOT be deleted.
        let (mut m, _) = authenticated_machine();
        let hash = TorrentHash("a".repeat(40));
        // 1 byte downloaded, only 1 hour seeded — well under 72h
        load_torrent(&mut m, record(&hash, 1, 3600));
        let out = m.handle_command(
            Instant::now(),
            QbitCommand::DeleteTorrent { hash: hash.clone() },
        );
        assert!(
            out.actions.is_empty(),
            "delete must be blocked for HnR-unsatisfied torrent"
        );
    }

    #[test]
    fn hnr_satisfied_torrent_allows_delete() {
        // A torrent with seed_time >= 72h should be deletable.
        let (mut m, cookie) = authenticated_machine();
        let hash = TorrentHash("b".repeat(40));
        // 1 byte downloaded, 72h seeded — exactly at the threshold
        load_torrent(&mut m, record(&hash, 1, 72 * 3600));
        let out = m.handle_command(
            Instant::now(),
            QbitCommand::DeleteTorrent { hash: hash.clone() },
        );
        assert_eq!(
            out.actions,
            vec![QbitAction::DeleteTorrent {
                cookie,
                hash: hash.clone(),
            }],
            "delete must be emitted for HnR-satisfied torrent"
        );
    }

    #[test]
    fn zero_byte_torrent_allows_delete_even_with_low_seed_time() {
        // A torrent with downloaded_bytes == 0 is always HnR-satisfied, regardless of seed time.
        let (mut m, cookie) = authenticated_machine();
        let hash = TorrentHash("c".repeat(40));
        // 0 bytes downloaded, 0 seconds seeded
        load_torrent(&mut m, record(&hash, 0, 0));
        let out = m.handle_command(
            Instant::now(),
            QbitCommand::DeleteTorrent { hash: hash.clone() },
        );
        assert_eq!(
            out.actions,
            vec![QbitAction::DeleteTorrent {
                cookie,
                hash: hash.clone(),
            }],
            "delete must be emitted for zero-byte torrent"
        );
    }

    #[test]
    fn no_cookie_blocks_delete_regardless_of_hnr_status() {
        // Without a cookie (qBit not connected), no delete action is emitted.
        let mut m = machine();
        let hash = TorrentHash("d".repeat(40));
        // Load a satisfied torrent (seed_time >= 72h)
        load_torrent(&mut m, record(&hash, 1, 72 * 3600));
        let out = m.handle_command(
            Instant::now(),
            QbitCommand::DeleteTorrent { hash: hash.clone() },
        );
        assert!(
            out.actions.is_empty(),
            "delete must be blocked when no cookie is present"
        );
    }

    #[test]
    fn unknown_torrent_allows_delete_when_authenticated() {
        // A torrent not in the map (unknown) is treated as deletable.
        let (mut m, cookie) = authenticated_machine();
        let hash = TorrentHash("e".repeat(40));
        // Do NOT load the torrent — it should be unknown
        let out = m.handle_command(
            Instant::now(),
            QbitCommand::DeleteTorrent { hash: hash.clone() },
        );
        assert_eq!(
            out.actions,
            vec![QbitAction::DeleteTorrent {
                cookie,
                hash: hash.clone(),
            }],
            "delete must be emitted for unknown torrent"
        );
    }

    // ── Dead-torrent auto-cleanup unit tests (QBIT-9 / story 20) ─────────────

    fn stalled_zero_byte_record(hash: &TorrentHash) -> TorrentRecord {
        TorrentRecord {
            hash: hash.clone(),
            downloaded_bytes: 0,
            seed_time: Duration::ZERO,
            state: TorrentState::StalledDownloading,
            mam_id: None,
        }
    }

    #[test]
    fn dead_torrent_authenticated_emits_delete_and_removed_publish() {
        // A zero-byte StalledDownloading torrent + authenticated session →
        // emits DeleteTorrent action AND DeadTorrentRemoved publish.
        let (mut m, cookie) = authenticated_machine();
        let hash = TorrentHash("a".repeat(40));
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![stalled_zero_byte_record(&hash)],
            },
        );
        assert!(
            out.actions.contains(&QbitAction::DeleteTorrent {
                cookie,
                hash: hash.clone(),
            }),
            "DeleteTorrent must be emitted for a dead zero-byte torrent"
        );
        assert!(
            out.publish
                .contains(&QbitPublish::DeadTorrentRemoved { hash, mam_id: None }),
            "DeadTorrentRemoved must be published when delete is authorised"
        );
    }

    #[test]
    fn active_downloading_torrent_not_deleted() {
        // A zero-byte Downloading torrent is active, not dead — must NOT be deleted.
        let (mut m, _cookie) = authenticated_machine();
        let hash = TorrentHash("b".repeat(40));
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![TorrentRecord {
                    hash: hash.clone(),
                    downloaded_bytes: 0,
                    seed_time: Duration::ZERO,
                    state: TorrentState::Downloading,
                    mam_id: None,
                }],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::DeleteTorrent { .. })),
            "an active Downloading torrent must NOT trigger auto-delete"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::DeadTorrentRemoved { .. })),
            "DeadTorrentRemoved must NOT be published for an active torrent"
        );
    }

    #[test]
    fn non_zero_byte_stalled_torrent_not_auto_deleted() {
        // A non-zero-byte StalledDownloading torrent must NOT be deleted by the
        // dead-torrent path (it falls under the HnR lock instead).
        let (mut m, _cookie) = authenticated_machine();
        let hash = TorrentHash("c".repeat(40));
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![TorrentRecord {
                    hash: hash.clone(),
                    downloaded_bytes: 1024,
                    seed_time: Duration::ZERO,
                    state: TorrentState::StalledDownloading,
                    mam_id: None,
                }],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::DeleteTorrent { .. })),
            "a non-zero-byte stalled torrent must NOT be auto-deleted"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::DeadTorrentRemoved { .. })),
            "DeadTorrentRemoved must NOT be published for a non-zero-byte torrent"
        );
    }

    #[test]
    fn dead_torrent_no_cookie_no_delete_no_publish() {
        // Without a cookie, a dead torrent produces no delete action and no
        // DeadTorrentRemoved publish.
        let mut m = machine();
        let hash = TorrentHash("d".repeat(40));
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![stalled_zero_byte_record(&hash)],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::DeleteTorrent { .. })),
            "no delete must be emitted without a cookie"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::DeadTorrentRemoved { .. })),
            "no DeadTorrentRemoved must be published without a cookie"
        );
    }

    // ── No-partials enforcement unit tests (QBIT-10 / story 21) ─────────────

    #[test]
    fn new_torrent_with_cookie_emits_set_all_files_priority() {
        // QBIT-10: first time a hash appears with an active cookie →
        // SetAllFilesPriority is emitted.
        let (mut m, cookie) = authenticated_machine();
        let hash = TorrentHash("f".repeat(40));
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![record(&hash, 0, 0)],
            },
        );
        assert!(
            out.actions.contains(&QbitAction::SetAllFilesPriority {
                cookie,
                hash: hash.clone(),
            }),
            "SetAllFilesPriority must be emitted for a newly-seen torrent"
        );
    }

    #[test]
    fn already_known_torrent_no_set_all_files_priority_on_second_listing() {
        // QBIT-10: fire-once — a hash already in the map must NOT produce a
        // second SetAllFilesPriority on the next TorrentsListed.
        let (mut m, _) = authenticated_machine();
        let hash = TorrentHash("g".repeat(40));
        // First listing — loads the torrent into the map.
        let _ = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![record(&hash, 0, 0)],
            },
        );
        // Second listing — same hash; must NOT emit SetAllFilesPriority again.
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![record(&hash, 0, 0)],
            },
        );
        assert!(
            !out.actions.iter().any(
                |a| matches!(a, QbitAction::SetAllFilesPriority { hash: h, .. } if h == &hash)
            ),
            "SetAllFilesPriority must NOT be re-emitted for an already-known torrent"
        );
    }

    #[test]
    fn no_cookie_no_set_all_files_priority() {
        // QBIT-10 + QBIT-1: without a cookie, no SetAllFilesPriority is emitted.
        let mut m = machine();
        let hash = TorrentHash("h".repeat(40));
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![record(&hash, 0, 0)],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::SetAllFilesPriority { .. })),
            "SetAllFilesPriority must NOT be emitted without a cookie"
        );
    }

    #[test]
    fn first_time_seen_dead_torrent_emits_both_set_all_files_and_delete() {
        // QBIT-10 co-existence with QBIT-9: a first-time-seen dead torrent gets
        // BOTH SetAllFilesPriority AND DeleteTorrent (mirrors legacy ordering:
        // check_new_torrents ran before check_dead_torrents).
        let (mut m, cookie) = authenticated_machine();
        let hash = TorrentHash("i".repeat(40));
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![stalled_zero_byte_record(&hash)],
            },
        );
        assert!(
            out.actions.contains(&QbitAction::SetAllFilesPriority {
                cookie: cookie.clone(),
                hash: hash.clone(),
            }),
            "SetAllFilesPriority must be emitted for a newly-seen dead torrent"
        );
        assert!(
            out.actions.contains(&QbitAction::DeleteTorrent {
                cookie,
                hash: hash.clone(),
            }),
            "DeleteTorrent must also be emitted for a newly-seen dead torrent"
        );
        assert!(
            out.publish
                .contains(&QbitPublish::DeadTorrentRemoved { hash, mam_id: None }),
            "DeadTorrentRemoved must be published"
        );
    }

    // ── EvictOneForDiskPressure unit tests (QBIT-11 / story 22) ──────────────

    #[test]
    fn evict_one_selects_longest_seed_time_satisfied_torrent() {
        // The candidate with the largest seed_time among HnR-satisfied torrents
        // is selected.  Both torrents below are satisfied (seed_time >= 72h).
        let (mut m, cookie) = authenticated_machine();
        let hash_short = TorrentHash("s".repeat(40));
        let hash_long = TorrentHash("l".repeat(40));
        load_torrent(&mut m, record(&hash_short, 1, 72 * 3600)); // exactly 72h
        load_torrent(&mut m, record(&hash_long, 1, 80 * 3600)); // 80h — winner
        let out = m.handle_command(Instant::now(), QbitCommand::EvictOneForDiskPressure);
        assert_eq!(
            out.actions,
            vec![QbitAction::DeleteTorrent {
                cookie,
                hash: hash_long,
            }],
            "eviction must target the torrent with the longest seed_time"
        );
    }

    #[test]
    fn evict_one_no_satisfied_torrents_emits_no_delete() {
        let (mut m, _cookie) = authenticated_machine();
        let hash = TorrentHash("u".repeat(40));
        // 1 byte downloaded, only 1 hour seeded — HnR-unsatisfied.
        load_torrent(&mut m, record(&hash, 1, 3600));
        let out = m.handle_command(Instant::now(), QbitCommand::EvictOneForDiskPressure);
        assert!(
            out.actions.is_empty(),
            "no delete must be emitted when no HnR-satisfied candidate exists"
        );
    }

    #[test]
    fn evict_one_no_cookie_emits_no_delete() {
        let mut m = machine();
        let hash = TorrentHash("v".repeat(40));
        load_torrent(&mut m, record(&hash, 0, 0));
        let out = m.handle_command(Instant::now(), QbitCommand::EvictOneForDiskPressure);
        assert!(
            out.actions.is_empty(),
            "no delete must be emitted when no cookie is present"
        );
    }

    #[test]
    fn evict_one_skips_hnr_unsatisfied_even_with_longer_seed_time() {
        // Unsatisfied torrent has the longest total seed_time but must be skipped.
        let (mut m, cookie) = authenticated_machine();
        let hash_unsat = TorrentHash("w".repeat(40));
        let hash_sat = TorrentHash("x".repeat(40));
        // Unsatisfied: 100h seed_time but still has bytes to go (seed_time < 72h
        // would normally block, but here we test the filter directly — set 1h).
        load_torrent(&mut m, record(&hash_unsat, 1, 3600)); // 1 byte, 1h — unsatisfied
        load_torrent(&mut m, record(&hash_sat, 0, 0)); // 0 bytes — always satisfied
        let out = m.handle_command(Instant::now(), QbitCommand::EvictOneForDiskPressure);
        assert_eq!(
            out.actions,
            vec![QbitAction::DeleteTorrent {
                cookie,
                hash: hash_sat,
            }],
            "unsatisfied torrent must not be selected even when present"
        );
    }

    // ── Privacy auto-revert unit tests (QBIT-12/QBIT-13 / story 23) ─────────

    #[test]
    fn preferences_read_with_dht_emits_disable_and_publishes_observed() {
        // Authenticated + DHT=true → DisableBannedPrivacySettings + BannedPrivacySettingsObserved.
        let (mut m, cookie) = authenticated_machine();
        let out = handle(
            &mut m,
            QbitEvent::PreferencesRead {
                listen_port: None,
                dht: true,
                pex: false,
                lsd: false,
                max_active_torrents: u32::MAX,
            },
        );
        assert!(
            out.actions
                .contains(&QbitAction::DisableBannedPrivacySettings { cookie }),
            "DisableBannedPrivacySettings must be emitted for dht=true when authenticated"
        );
        assert!(
            out.publish
                .contains(&QbitPublish::BannedPrivacySettingsObserved {
                    dht: true,
                    pex: false,
                    lsd: false,
                }),
            "BannedPrivacySettingsObserved must be published when dht=true"
        );
    }

    #[test]
    fn preferences_read_clean_no_disable_no_publish() {
        // No banned setting → no DisableBannedPrivacySettings, no privacy publish.
        let (mut m, _cookie) = authenticated_machine();
        let out = handle(
            &mut m,
            QbitEvent::PreferencesRead {
                listen_port: None,
                dht: false,
                pex: false,
                lsd: false,
                max_active_torrents: u32::MAX,
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::DisableBannedPrivacySettings { .. })),
            "no DisableBannedPrivacySettings when all settings are clean"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::BannedPrivacySettingsObserved { .. })),
            "no BannedPrivacySettingsObserved when all settings are clean"
        );
    }

    #[test]
    fn preferences_read_dht_unauthenticated_no_disable_action_but_still_publishes() {
        // Judgment call: unauthenticated + banned setting observed → we still publish
        // BannedPrivacySettingsObserved so the domain can fire a Critical alert even
        // though we cannot yet issue the disable action (no cookie).
        let mut m = machine();
        let out = handle(
            &mut m,
            QbitEvent::PreferencesRead {
                listen_port: None,
                dht: true,
                pex: false,
                lsd: false,
                max_active_torrents: u32::MAX,
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::DisableBannedPrivacySettings { .. })),
            "no DisableBannedPrivacySettings without a cookie (QBIT-1)"
        );
        assert!(
            out.publish
                .contains(&QbitPublish::BannedPrivacySettingsObserved {
                    dht: true,
                    pex: false,
                    lsd: false,
                }),
            "BannedPrivacySettingsObserved must still be published even without a cookie"
        );
    }

    #[test]
    fn privacy_settings_disable_failed_schedules_sync_retry_publishes_unavailable() {
        // QBIT-13: failure → one SyncRetry scheduled + Unavailable published.
        let (mut m, _cookie) = authenticated_machine();
        let out = handle(
            &mut m,
            QbitEvent::PrivacySettingsDisableFailed {
                reason: "forbidden".to_string(),
            },
        );
        assert_eq!(
            out.actions,
            vec![QbitAction::ScheduleTimer {
                timer: QbitTimer::SyncRetry,
                after: Duration::from_secs(2),
            }],
            "PrivacySettingsDisableFailed must schedule exactly one SyncRetry"
        );
        assert_eq!(
            out.publish,
            vec![QbitPublish::Unavailable {
                reason: "forbidden".to_string(),
            }],
            "PrivacySettingsDisableFailed must publish Unavailable"
        );
    }

    #[test]
    fn privacy_settings_disabled_is_noop() {
        // QBIT-12: success is a no-op.
        let (mut m, _cookie) = authenticated_machine();
        let out = handle(&mut m, QbitEvent::PrivacySettingsDisabled);
        assert!(
            out.actions.is_empty(),
            "PrivacySettingsDisabled must emit no actions"
        );
        assert!(
            out.publish.is_empty(),
            "PrivacySettingsDisabled must emit no publishes"
        );
    }

    #[test]
    fn torrent_refresh_timer_also_issues_read_preferences() {
        // TorrentRefresh now piggbacks ReadPreferences for continuous prefs observation.
        let (mut m, cookie) = authenticated_machine();
        let out = handle(&mut m, QbitEvent::TimerFired(QbitTimer::TorrentRefresh));
        assert!(
            out.actions.contains(&QbitAction::ListTorrents {
                cookie: cookie.clone()
            }),
            "TorrentRefresh must still issue ListTorrents"
        );
        assert!(
            out.actions
                .contains(&QbitAction::ReadPreferences { cookie }),
            "TorrentRefresh must also issue ReadPreferences"
        );
        assert!(
            out.actions.contains(&QbitAction::ScheduleTimer {
                timer: QbitTimer::TorrentRefresh,
                after: Duration::from_secs(30),
            }),
            "TorrentRefresh must re-schedule itself"
        );
    }

    // ── Queue-orchestration unit tests (QBIT-14/15/16 / story 24) ───────────

    fn satisfied_uploading(hash: &TorrentHash, seed_secs: u64) -> TorrentRecord {
        TorrentRecord {
            hash: hash.clone(),
            downloaded_bytes: 1024,
            seed_time: Duration::from_secs(seed_secs),
            state: TorrentState::Uploading,
            mam_id: None,
        }
    }

    fn unsatisfied_paused_uploading(hash: &TorrentHash) -> TorrentRecord {
        TorrentRecord {
            hash: hash.clone(),
            downloaded_bytes: 1024,
            seed_time: Duration::from_secs(3600), // 1h < 72h
            state: TorrentState::PausedUploading,
            mam_id: None,
        }
    }

    fn authenticated_machine_with_limit(limit: u32) -> (QbitMachine, AuthCookie) {
        let (mut m, cookie) = authenticated_machine();
        m.max_active_torrents = limit;
        (m, cookie)
    }

    #[test]
    fn queue_below_limit_no_orchestration() {
        // Active count < limit → no orchestration even with eligible torrents.
        let (mut m, cookie) = authenticated_machine_with_limit(10);
        let sat = TorrentHash("a".repeat(40));
        let unsat = TorrentHash("b".repeat(40));
        load_torrent(&mut m, satisfied_uploading(&sat, 72 * 3600));
        load_torrent(&mut m, unsatisfied_paused_uploading(&unsat));

        // Only 2 active but limit is 10 → no orchestration.
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![
                    satisfied_uploading(&sat, 72 * 3600),
                    unsatisfied_paused_uploading(&unsat),
                ],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::PauseTorrent { .. })),
            "no PauseTorrent when below limit"
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::ForceResumeTorrent { .. })),
            "no ForceResumeTorrent when below limit"
        );
        let _ = cookie; // suppress unused warning
    }

    #[test]
    fn queue_at_limit_no_parked_unsatisfied_no_orchestration() {
        // Active count >= limit but no parked unsatisfied torrent → no orchestration.
        let (mut m, _) = authenticated_machine_with_limit(1);
        let sat = TorrentHash("c".repeat(40));
        load_torrent(&mut m, satisfied_uploading(&sat, 72 * 3600));

        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![satisfied_uploading(&sat, 72 * 3600)],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::PauseTorrent { .. })),
            "no PauseTorrent when no parked unsatisfied torrent"
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::ForceResumeTorrent { .. })),
            "no ForceResumeTorrent when no parked unsatisfied torrent"
        );
    }

    #[test]
    fn queue_at_limit_parked_unsatisfied_but_no_satisfied_seeder_no_orchestration() {
        // Active count >= limit + parked unsatisfied exists, but no satisfied Uploading → skip.
        let (mut m, _) = authenticated_machine_with_limit(0);
        let unsat = TorrentHash("d".repeat(40));
        load_torrent(&mut m, unsatisfied_paused_uploading(&unsat));

        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![unsatisfied_paused_uploading(&unsat)],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::PauseTorrent { .. })),
            "no PauseTorrent when no satisfied seeder exists"
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::ForceResumeTorrent { .. })),
            "no ForceResumeTorrent when no satisfied seeder exists"
        );
    }

    #[test]
    fn queue_orchestration_full_case() {
        // Active >= limit + parked unsatisfied + satisfied seeder → orchestrate.
        let (mut m, cookie) = authenticated_machine_with_limit(1);
        let sat = TorrentHash("e".repeat(40));
        let unsat = TorrentHash("f".repeat(40));
        load_torrent(&mut m, satisfied_uploading(&sat, 72 * 3600));
        load_torrent(&mut m, unsatisfied_paused_uploading(&unsat));

        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![
                    satisfied_uploading(&sat, 72 * 3600),
                    unsatisfied_paused_uploading(&unsat),
                ],
            },
        );
        assert!(
            out.actions.contains(&QbitAction::PauseTorrent {
                cookie: cookie.clone(),
                hash: sat.clone(),
            }),
            "PauseTorrent must target the satisfied seeder"
        );
        assert!(
            out.actions.contains(&QbitAction::ForceResumeTorrent {
                cookie,
                hash: unsat.clone(),
            }),
            "ForceResumeTorrent must target the parked unsatisfied torrent"
        );
        assert!(
            out.publish.contains(&QbitPublish::QueueOrchestrated {
                paused: sat,
                force_resumed: unsat,
            }),
            "QueueOrchestrated must be published"
        );
    }

    #[test]
    fn queue_no_cookie_no_orchestration() {
        // No cookie → no orchestration even when limits would trigger.
        let mut m = machine();
        m.max_active_torrents = 0; // limit = 0, definitely at/above
        let sat = TorrentHash("g".repeat(40));
        let unsat = TorrentHash("h".repeat(40));
        load_torrent(&mut m, satisfied_uploading(&sat, 72 * 3600));
        load_torrent(&mut m, unsatisfied_paused_uploading(&unsat));

        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: vec![
                    satisfied_uploading(&sat, 72 * 3600),
                    unsatisfied_paused_uploading(&unsat),
                ],
            },
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::PauseTorrent { .. })),
            "no PauseTorrent without a cookie"
        );
        assert!(
            !out.actions
                .iter()
                .any(|a| matches!(a, QbitAction::ForceResumeTorrent { .. })),
            "no ForceResumeTorrent without a cookie"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::QueueOrchestrated { .. })),
            "no QueueOrchestrated published without a cookie"
        );
    }

    // ── Unsatisfied-quota unit tests (QBIT-17/18 / story 25) ────────────────

    fn machine_with_quota(limit: u32) -> QbitMachine {
        QbitMachine::new(
            QbitConfig {
                auth_retry: Duration::from_secs(1),
                sync_retry: Duration::from_secs(2),
                torrent_refresh: Duration::from_secs(30),
                hnr_seed_time: HNR_SEED_TIME,
                unsatisfied_quota_limit: limit,
            },
            Instant::now(),
        )
    }

    fn unsatisfied_record(hash: &TorrentHash) -> TorrentRecord {
        // downloaded > 0, seed_time < 72h → HnR-unsatisfied
        TorrentRecord {
            hash: hash.clone(),
            downloaded_bytes: 1024,
            seed_time: Duration::from_secs(3600), // 1h
            state: TorrentState::Uploading,
            mam_id: None,
        }
    }

    fn load_n_unsatisfied(m: &mut QbitMachine, n: usize) {
        let torrents: Vec<TorrentRecord> = (0..n)
            .map(|i| unsatisfied_record(&TorrentHash(format!("{:0>40x}", i))))
            .collect();
        let _ = m.handle(
            Instant::now(),
            Timed::now(QbitEvent::TorrentsListed { torrents }),
        );
    }

    #[test]
    fn quota_disabled_no_publish() {
        // unsatisfied_quota_limit = 0 → gate disabled, never publishes quota events.
        let mut m = machine_with_quota(0);
        let out = handle(
            &mut m,
            QbitEvent::TorrentsListed {
                torrents: (0..200_u32)
                    .map(|i| unsatisfied_record(&TorrentHash(format!("{:0>40x}", i))))
                    .collect(),
            },
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::UnsatisfiedQuotaCritical { .. })),
            "quota disabled must never publish UnsatisfiedQuotaCritical"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::UnsatisfiedQuotaApproaching { .. })),
            "quota disabled must never publish UnsatisfiedQuotaApproaching"
        );
    }

    #[test]
    fn quota_critical_at_limit() {
        // unsatisfied == limit → UnsatisfiedQuotaCritical.
        let limit: u32 = 10;
        let mut m = machine_with_quota(limit);
        let torrents: Vec<TorrentRecord> = (0..limit)
            .map(|i| unsatisfied_record(&TorrentHash(format!("{:0>40x}", i))))
            .collect();
        let out = handle(&mut m, QbitEvent::TorrentsListed { torrents });
        assert!(
            out.publish.iter().any(|p| matches!(
                p,
                QbitPublish::UnsatisfiedQuotaCritical {
                    unsatisfied,
                    limit: l
                } if *unsatisfied == limit && *l == limit
            )),
            "must publish UnsatisfiedQuotaCritical when unsatisfied == limit"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::UnsatisfiedQuotaApproaching { .. })),
            "must not also publish UnsatisfiedQuotaApproaching"
        );
    }

    #[test]
    fn quota_approaching_at_limit_minus_1() {
        // unsatisfied == limit - 1 → UnsatisfiedQuotaApproaching (boundary).
        let limit: u32 = 10;
        let mut m = machine_with_quota(limit);
        let torrents: Vec<TorrentRecord> = (0..(limit - 1))
            .map(|i| unsatisfied_record(&TorrentHash(format!("{:0>40x}", i))))
            .collect();
        let out = handle(&mut m, QbitEvent::TorrentsListed { torrents });
        assert!(
            out.publish.iter().any(|p| matches!(
                p,
                QbitPublish::UnsatisfiedQuotaApproaching {
                    unsatisfied,
                    limit: l
                } if *unsatisfied == limit - 1 && *l == limit
            )),
            "must publish UnsatisfiedQuotaApproaching when unsatisfied == limit - 1"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::UnsatisfiedQuotaCritical { .. })),
            "must not publish UnsatisfiedQuotaCritical when below limit"
        );
    }

    #[test]
    fn quota_approaching_at_limit_minus_5() {
        // unsatisfied == limit - 5 → UnsatisfiedQuotaApproaching (lower boundary).
        let limit: u32 = 10;
        let mut m = machine_with_quota(limit);
        let torrents: Vec<TorrentRecord> = (0..(limit - 5))
            .map(|i| unsatisfied_record(&TorrentHash(format!("{:0>40x}", i))))
            .collect();
        let out = handle(&mut m, QbitEvent::TorrentsListed { torrents });
        assert!(
            out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::UnsatisfiedQuotaApproaching { .. })),
            "must publish UnsatisfiedQuotaApproaching when unsatisfied == limit - 5"
        );
    }

    #[test]
    fn quota_no_publish_below_warning_threshold() {
        // unsatisfied == limit - 6 → no quota publish.
        let limit: u32 = 10;
        let mut m = machine_with_quota(limit);
        let torrents: Vec<TorrentRecord> = (0..(limit - 6))
            .map(|i| unsatisfied_record(&TorrentHash(format!("{:0>40x}", i))))
            .collect();
        let out = handle(&mut m, QbitEvent::TorrentsListed { torrents });
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::UnsatisfiedQuotaCritical { .. })),
            "must not publish critical below warning threshold"
        );
        assert!(
            !out.publish
                .iter()
                .any(|p| matches!(p, QbitPublish::UnsatisfiedQuotaApproaching { .. })),
            "must not publish approaching when 6 below limit"
        );
    }

    #[test]
    fn unsatisfied_count_accessor() {
        let mut m = machine_with_quota(100);
        // 2 unsatisfied (downloaded > 0, seed < 72h), 1 satisfied (seed >= 72h), 1 zero-byte
        let _ = m.handle(
            Instant::now(),
            Timed::now(QbitEvent::TorrentsListed {
                torrents: vec![
                    unsatisfied_record(&TorrentHash("a".repeat(40))),
                    unsatisfied_record(&TorrentHash("b".repeat(40))),
                    record(&TorrentHash("c".repeat(40)), 1, 72 * 3600), // satisfied
                    record(&TorrentHash("d".repeat(40)), 0, 0),         // zero-byte
                ],
            }),
        );
        assert_eq!(m.unsatisfied_count(), 2);
    }

    #[test]
    fn unsatisfied_quota_full_accessor_false_when_below_limit() {
        let mut m = machine_with_quota(5);
        load_n_unsatisfied(&mut m, 4);
        assert!(!m.unsatisfied_quota_full());
    }

    #[test]
    fn unsatisfied_quota_full_accessor_true_at_limit() {
        let mut m = machine_with_quota(5);
        load_n_unsatisfied(&mut m, 5);
        assert!(m.unsatisfied_quota_full());
    }

    #[test]
    fn unsatisfied_quota_full_false_when_limit_is_zero() {
        let mut m = machine_with_quota(0);
        load_n_unsatisfied(&mut m, 1000);
        assert!(!m.unsatisfied_quota_full(), "limit 0 must disable the gate");
    }
}

#[cfg(test)]
mod prop_tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};
    use windlass_types::{
        AuthCookie, MamTorrentId, TorrentHash, TorrentRecord, TorrentState, VpnPort,
    };

    use crate::{
        PrivacySettings, QbitAction, QbitCommand, QbitConfig, QbitEvent, QbitMachine, QbitPublish,
        QbitTimer,
    };

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    fn any_auth_cookie() -> impl Strategy<Value = AuthCookie> {
        "[a-zA-Z0-9]{8,32}".prop_map(AuthCookie::new)
    }

    fn any_torrent_hash() -> impl Strategy<Value = TorrentHash> {
        "[a-f0-9]{40}".prop_map(TorrentHash)
    }

    fn any_torrent_state() -> impl Strategy<Value = TorrentState> {
        prop_oneof![
            Just(TorrentState::Downloading),
            Just(TorrentState::StalledDownloading),
            Just(TorrentState::Uploading),
            Just(TorrentState::StalledUploading),
            Just(TorrentState::ForcedUpload),
            Just(TorrentState::PausedDownloading),
            Just(TorrentState::PausedUploading),
            Just(TorrentState::Error),
            any::<String>().prop_map(TorrentState::Other),
        ]
    }

    fn any_mam_id() -> impl Strategy<Value = MamTorrentId> {
        (1u64..=u64::MAX).prop_map(|id| MamTorrentId::try_new(id).unwrap())
    }

    fn any_torrent_record() -> impl Strategy<Value = TorrentRecord> {
        (
            any_torrent_hash(),
            any::<u64>(),
            // seed_time as secs: 0..=(200*3600) to keep durations reasonable
            (0u64..=(200 * 3600)),
            any_torrent_state(),
            proptest::option::of(any_mam_id()),
        )
            .prop_map(
                |(hash, downloaded_bytes, seed_secs, state, mam_id)| TorrentRecord {
                    hash,
                    downloaded_bytes,
                    seed_time: Duration::from_secs(seed_secs),
                    state,
                    mam_id,
                },
            )
    }

    fn any_torrent_map() -> impl Strategy<Value = HashMap<TorrentHash, TorrentRecord>> {
        prop::collection::vec(any_torrent_record(), 0..4)
            .prop_map(|records| records.into_iter().map(|r| (r.hash.clone(), r)).collect())
    }

    // Fully-arbitrary state, including unreachable combinations: the tested
    // invariants are total.
    fn any_qbit_machine() -> impl Strategy<Value = QbitMachine> {
        (
            proptest::option::of(any_auth_cookie()),
            proptest::option::of(any_vpn_port()),
            proptest::option::of(any_vpn_port()),
            any::<bool>(),
            any_torrent_map(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<u32>(),
            any::<u32>(),
        )
            .prop_map(
                |(
                    cookie,
                    listen_port,
                    desired_listen_port,
                    refresh_scheduled,
                    torrents,
                    dht,
                    pex,
                    lsd,
                    max_active_torrents,
                    unsatisfied_quota_limit,
                )| {
                    let mut machine = QbitMachine::new(
                        QbitConfig {
                            auth_retry: Duration::from_secs(1),
                            sync_retry: Duration::from_secs(2),
                            torrent_refresh: Duration::from_secs(30),
                            hnr_seed_time: Duration::from_secs(72 * 3600),
                            unsatisfied_quota_limit,
                        },
                        Instant::now(),
                    );
                    machine.cookie = cookie;
                    machine.listen_port = listen_port;
                    machine.desired_listen_port = desired_listen_port;
                    machine.refresh_scheduled = refresh_scheduled;
                    machine.torrents = torrents;
                    machine.privacy = PrivacySettings { dht, pex, lsd };
                    machine.max_active_torrents = max_active_torrents;
                    machine
                },
            )
    }

    fn any_qbit_event() -> impl Strategy<Value = QbitEvent> {
        prop_oneof![
            Just(QbitEvent::Init),
            any_auth_cookie().prop_map(|cookie| QbitEvent::AuthSucceeded { cookie }),
            any::<String>().prop_map(|reason| QbitEvent::AuthFailed { reason }),
            (
                proptest::option::of(any_vpn_port()),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<u32>(),
            )
                .prop_map(|(listen_port, dht, pex, lsd, max_active_torrents)| {
                    QbitEvent::PreferencesRead {
                        listen_port,
                        dht,
                        pex,
                        lsd,
                        max_active_torrents,
                    }
                }),
            any::<String>().prop_map(|reason| QbitEvent::PreferencesFailed { reason }),
            any_vpn_port().prop_map(|port| QbitEvent::ListenPortSet { port }),
            (any_vpn_port(), any::<String>())
                .prop_map(|(port, reason)| QbitEvent::ListenPortSetFailed { port, reason }),
            prop::collection::vec(any_torrent_record(), 0..4)
                .prop_map(|torrents| QbitEvent::TorrentsListed { torrents }),
            Just(QbitEvent::PrivacySettingsDisabled),
            any::<String>().prop_map(|reason| QbitEvent::PrivacySettingsDisableFailed { reason }),
            Just(QbitEvent::TimerFired(QbitTimer::AuthRetry)),
            Just(QbitEvent::TimerFired(QbitTimer::SyncRetry)),
            Just(QbitEvent::TimerFired(QbitTimer::TorrentRefresh)),
        ]
    }

    fn any_qbit_command() -> impl Strategy<Value = QbitCommand> {
        prop_oneof![
            Just(QbitCommand::EnsureAuthenticated),
            any_vpn_port().prop_map(|port| QbitCommand::EnsureListenPort { port }),
            Just(QbitCommand::RefreshTorrents),
            any_torrent_hash().prop_map(|hash| QbitCommand::PauseTorrent { hash }),
            any_torrent_hash().prop_map(|hash| QbitCommand::ResumeTorrent { hash }),
            any_torrent_hash().prop_map(|hash| QbitCommand::DeleteTorrent { hash }),
            Just(QbitCommand::EvictOneForDiskPressure),
        ]
    }

    fn carries_cookie(action: &QbitAction) -> bool {
        matches!(
            action,
            QbitAction::ReadPreferences { .. }
                | QbitAction::SetListenPort { .. }
                | QbitAction::ListTorrents { .. }
                | QbitAction::PauseTorrent { .. }
                | QbitAction::ResumeTorrent { .. }
                | QbitAction::DeleteTorrent { .. }
                | QbitAction::SetAllFilesPriority { .. }
                | QbitAction::DisableBannedPrivacySettings { .. }
                | QbitAction::ForceResumeTorrent { .. }
        )
    }

    proptest! {
        // GLOBAL-1 (no panic).
        #[test]
        fn handle_never_panics(mut machine in any_qbit_machine(), event in any_qbit_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // GLOBAL-1 (no panic) for commands.
        #[test]
        fn handle_command_never_panics(mut machine in any_qbit_machine(), command in any_qbit_command()) {
            let _ = machine.handle_command(Instant::now(), command);
        }

        // QBIT-1 (Guarantees C/D): no cookie-bearing action is emitted unless the
        // machine is authenticated — for events and for commands.
        #[test]
        fn events_emit_no_cookie_action_while_unauthenticated(
            mut machine in any_qbit_machine(),
            event in any_qbit_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for action in &out.actions {
                if carries_cookie(action) {
                    prop_assert!(machine.is_authenticated());
                }
            }
        }

        #[test]
        fn commands_emit_no_cookie_action_while_unauthenticated(
            mut machine in any_qbit_machine(),
            command in any_qbit_command(),
        ) {
            let out = machine.handle_command(Instant::now(), command);
            for action in &out.actions {
                if carries_cookie(action) {
                    prop_assert!(machine.is_authenticated());
                }
            }
        }

        // QBIT-4 (Guarantee C): every published ListenPortReady carries a port
        // that agrees with the desired target (or there is no desired target).
        // The machine now defends against dishonest ListenPortSet events (story
        // 18), so the generator is fully unconstrained — no shell-contract
        // rewrite needed.
        #[test]
        fn listen_port_ready_matches_desired(
            mut machine in any_qbit_machine(),
            event in any_qbit_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for publish in &out.publish {
                if let QbitPublish::ListenPortReady { port } = publish {
                    prop_assert!(
                        machine.desired_listen_port.is_none()
                            || machine.desired_listen_port == Some(*port)
                    );
                }
            }
        }

        // QBIT-8 [safety] (Guarantee A): no DeleteTorrent action is ever emitted
        // for a hash that is known to the machine with downloaded_bytes > 0 and
        // seed_time < hnr_seed_time. This is the HnR seed-time lock invariant.
        // Tested against fully-arbitrary machine state (total invariant).
        #[test]
        fn no_delete_action_for_hnr_unsatisfied_torrent_on_event(
            mut machine in any_qbit_machine(),
            event in any_qbit_event(),
        ) {
            // Snapshot the unsatisfied hashes BEFORE handle mutates state.
            let unsatisfied: std::collections::HashSet<TorrentHash> = machine.torrents.iter()
                .filter(|(_, t)| {
                    t.downloaded_bytes > 0 && t.seed_time < machine.config.hnr_seed_time
                })
                .map(|(h, _)| h.clone())
                .collect();
            let out = machine.handle(Instant::now(), Timed::now(event));
            for action in &out.actions {
                if let QbitAction::DeleteTorrent { hash, .. } = action {
                    prop_assert!(
                        !unsatisfied.contains(hash),
                        "DeleteTorrent emitted for HnR-unsatisfied hash {hash:?}"
                    );
                }
            }
        }

        #[test]
        fn no_delete_action_for_hnr_unsatisfied_torrent_on_command(
            mut machine in any_qbit_machine(),
            command in any_qbit_command(),
        ) {
            // Snapshot the unsatisfied hashes BEFORE handle_command mutates state.
            let unsatisfied: std::collections::HashSet<TorrentHash> = machine.torrents.iter()
                .filter(|(_, t)| {
                    t.downloaded_bytes > 0 && t.seed_time < machine.config.hnr_seed_time
                })
                .map(|(h, _)| h.clone())
                .collect();
            let out = machine.handle_command(Instant::now(), command);
            for action in &out.actions {
                if let QbitAction::DeleteTorrent { hash, .. } = action {
                    prop_assert!(
                        !unsatisfied.contains(hash),
                        "DeleteTorrent emitted for HnR-unsatisfied hash {hash:?}"
                    );
                }
            }
        }

        // QBIT-9 [safety] (Guarantee A): every DeleteTorrent emitted by the
        // dead-torrent listing path targets a torrent whose downloaded_bytes == 0.
        // Because a dead torrent is defined as zero-byte, the HnR gate (QBIT-8)
        // also allows it, so the two invariants compose.
        // Tested against fully-arbitrary machine state (total invariant).
        #[test]
        fn dead_torrent_delete_targets_only_zero_byte_torrents(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            // Build a lookup of the listed torrents BEFORE they overwrite the map.
            let listed: HashMap<TorrentHash, TorrentRecord> = torrents
                .iter()
                .map(|t| (t.hash.clone(), t.clone()))
                .collect();

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            for action in &out.actions {
                if let QbitAction::DeleteTorrent { hash, .. } = action {
                    // The delete must target a torrent that was in the new listing
                    // and that torrent must have downloaded_bytes == 0.
                    if let Some(record) = listed.get(hash) {
                        prop_assert_eq!(
                            record.downloaded_bytes,
                            0,
                            "DeleteTorrent from TorrentsListed targets a non-zero-byte torrent"
                        );
                    }
                    // If the hash is NOT in the listed set (shouldn't happen via
                    // this path, but compose with QBIT-8 to be sure), the test is
                    // vacuously satisfied.
                }
            }
        }

        // QBIT-10 [safety] (Guarantee A): no SetAllFilesPriority is emitted for
        // a hash that was already in self.torrents before the TorrentsListed event.
        // Fire-once semantics: the action is emitted only for newly-seen hashes.
        // Tested against fully-arbitrary machine state (total invariant).
        #[test]
        fn no_set_all_files_priority_for_previously_known_hash(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            // Snapshot the known hashes BEFORE handle mutates state.
            let previously_known: std::collections::HashSet<TorrentHash> =
                machine.torrents.keys().cloned().collect();

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            for action in &out.actions {
                if let QbitAction::SetAllFilesPriority { hash, .. } = action {
                    prop_assert!(
                        !previously_known.contains(hash),
                        "SetAllFilesPriority emitted for already-known hash {hash:?}"
                    );
                }
            }
        }

        // QBIT-1 extended: SetAllFilesPriority (now in carries_cookie) is never
        // emitted while unauthenticated.  This is covered generically by the
        // existing `events_emit_no_cookie_action_while_unauthenticated` test
        // because carries_cookie now includes SetAllFilesPriority.
        // The test below is a dedicated targeted check for clarity.
        #[test]
        fn no_set_all_files_priority_without_cookie(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            // Force unauthenticated.
            machine.cookie = None;
            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));
            for action in &out.actions {
                prop_assert!(
                    !matches!(action, QbitAction::SetAllFilesPriority { .. }),
                    "SetAllFilesPriority must never be emitted without a cookie"
                );
            }
        }

        // QBIT-11 [safety] (disk-pressure eviction gate — §22):
        // `EvictOneForDiskPressure` emits at most one `DeleteTorrent`, and any
        // emitted `DeleteTorrent` targets an HnR-satisfied torrent.
        // Composes with QBIT-8 (total invariant).
        #[test]
        fn evict_one_emits_at_most_one_delete(mut machine in any_qbit_machine()) {
            let out = machine.handle_command(
                Instant::now(),
                QbitCommand::EvictOneForDiskPressure,
            );
            let delete_actions: Vec<_> = out
                .actions
                .iter()
                .filter(|a| matches!(a, QbitAction::DeleteTorrent { .. }))
                .collect();
            prop_assert!(
                delete_actions.len() <= 1,
                "EvictOneForDiskPressure emitted {} DeleteTorrent actions, expected at most 1",
                delete_actions.len()
            );
        }

        #[test]
        fn evict_one_targets_only_hnr_satisfied_torrent(mut machine in any_qbit_machine()) {
            // Snapshot unsatisfied hashes BEFORE the command.
            let unsatisfied: std::collections::HashSet<TorrentHash> = machine
                .torrents
                .iter()
                .filter(|(_, t)| {
                    t.downloaded_bytes > 0 && t.seed_time < machine.config.hnr_seed_time
                })
                .map(|(h, _)| h.clone())
                .collect();

            let out = machine.handle_command(
                Instant::now(),
                QbitCommand::EvictOneForDiskPressure,
            );
            for action in &out.actions {
                if let QbitAction::DeleteTorrent { hash, .. } = action {
                    prop_assert!(
                        !unsatisfied.contains(hash),
                        "EvictOneForDiskPressure DeleteTorrent targets HnR-unsatisfied hash {hash:?}"
                    );
                }
            }
        }

        // QBIT-12 [safety] (banned privacy auto-revert — §23):
        // For any state with `cookie == Some`, a `PreferencesRead` event in which
        // any of `dht/pex/lsd` is true emits exactly one `DisableBannedPrivacySettings`
        // action and publishes `BannedPrivacySettingsObserved`.
        // `cookie == None` never emits the disable action (QBIT-1).
        // Total invariant.
        #[test]
        fn preferences_read_with_banned_setting_and_cookie_emits_disable(
            mut machine in any_qbit_machine(),
            listen_port in proptest::option::of(any_vpn_port()),
            dht in any::<bool>(),
            pex in any::<bool>(),
            lsd in any::<bool>(),
        ) {
            // Only test when at least one banned setting is true.
            prop_assume!(dht || pex || lsd);

            let has_cookie = machine.cookie.is_some();
            let event = QbitEvent::PreferencesRead {
                listen_port,
                dht,
                pex,
                lsd,
                max_active_torrents: u32::MAX,
            };
            let out = machine.handle(Instant::now(), Timed::now(event));

            let disable_count = out.actions.iter().filter(|a| {
                matches!(a, QbitAction::DisableBannedPrivacySettings { .. })
            }).count();

            if has_cookie {
                prop_assert_eq!(
                    disable_count,
                    1,
                    "PreferencesRead with banned setting and cookie must emit exactly one \
                     DisableBannedPrivacySettings"
                );
            } else {
                prop_assert_eq!(
                    disable_count,
                    0,
                    "PreferencesRead without cookie must not emit DisableBannedPrivacySettings"
                );
            }

            // BannedPrivacySettingsObserved must always be published when any setting is banned,
            // regardless of cookie (the qBit core publishes the observation independently of
            // whether it can act on it — the domain needs to alert regardless).
            let observed_count = out.publish.iter().filter(|p| {
                matches!(p, QbitPublish::BannedPrivacySettingsObserved { .. })
            }).count();
            prop_assert_eq!(
                observed_count,
                1,
                "PreferencesRead with any banned setting must publish exactly one \
                 BannedPrivacySettingsObserved"
            );
        }

        // QBIT-12 complement: no banned setting → no disable action, no privacy publish.
        #[test]
        fn preferences_read_with_no_banned_settings_emits_no_disable_and_no_publish(
            mut machine in any_qbit_machine(),
            listen_port in proptest::option::of(any_vpn_port()),
        ) {
            let event = QbitEvent::PreferencesRead {
                listen_port,
                dht: false,
                pex: false,
                lsd: false,
                max_active_torrents: u32::MAX,
            };
            let out = machine.handle(Instant::now(), Timed::now(event));

            prop_assert!(
                !out.actions.iter().any(|a| matches!(a, QbitAction::DisableBannedPrivacySettings { .. })),
                "no DisableBannedPrivacySettings when all settings are clean"
            );
            prop_assert!(
                !out.publish.iter().any(|p| matches!(p, QbitPublish::BannedPrivacySettingsObserved { .. })),
                "no BannedPrivacySettingsObserved publish when all settings are clean"
            );
        }

        // QBIT-13 [safety] (privacy retry — §23):
        // `PrivacySettingsDisableFailed` schedules exactly one `SyncRetry` and
        // publishes `Unavailable`; no immediate retry action.
        // Total invariant.
        #[test]
        fn privacy_disable_failed_schedules_sync_retry_and_publishes_unavailable(
            mut machine in any_qbit_machine(),
            reason in any::<String>(),
        ) {
            let event = QbitEvent::PrivacySettingsDisableFailed { reason };
            let out = machine.handle(Instant::now(), Timed::now(event));

            let sync_retry_count = out.actions.iter().filter(|a| {
                matches!(a, QbitAction::ScheduleTimer { timer: QbitTimer::SyncRetry, .. })
            }).count();
            prop_assert_eq!(
                sync_retry_count,
                1,
                "PrivacySettingsDisableFailed must schedule exactly one SyncRetry"
            );

            let unavailable_count = out.publish.iter().filter(|p| {
                matches!(p, QbitPublish::Unavailable { .. })
            }).count();
            prop_assert_eq!(
                unavailable_count,
                1,
                "PrivacySettingsDisableFailed must publish exactly one Unavailable"
            );

            // No disable action should be emitted immediately (no tight loop).
            prop_assert!(
                !out.actions.iter().any(|a| matches!(a, QbitAction::DisableBannedPrivacySettings { .. })),
                "PrivacySettingsDisableFailed must not immediately retry the disable action"
            );
        }

        // QBIT-12: PrivacySettingsDisabled is a no-op.
        #[test]
        fn privacy_settings_disabled_is_noop(mut machine in any_qbit_machine()) {
            let out = machine.handle(
                Instant::now(),
                Timed::now(QbitEvent::PrivacySettingsDisabled),
            );
            prop_assert!(out.actions.is_empty(), "PrivacySettingsDisabled must emit no actions");
            prop_assert!(out.publish.is_empty(), "PrivacySettingsDisabled must emit no publishes");
        }

        // QBIT-14 [safety] (queue orchestration: never pause unsatisfied — §24):
        // Every `PauseTorrent` emitted from the `TorrentsListed` orchestration
        // path targets a known HnR-satisfied torrent
        // (`seed_time >= hnr_seed_time` or `downloaded_bytes == 0`).
        // Tested against fully-arbitrary machine state (total invariant).
        #[test]
        fn orchestration_pause_targets_only_hnr_satisfied(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            // Build a lookup of the listed torrents BEFORE they overwrite the map.
            let listed: HashMap<TorrentHash, TorrentRecord> = torrents
                .iter()
                .map(|t| (t.hash.clone(), t.clone()))
                .collect();
            let hnr_seed_time = machine.config.hnr_seed_time;

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            for action in &out.actions {
                if let QbitAction::PauseTorrent { hash, .. } = action {
                    // Must be known in the listing AND HnR-satisfied.
                    if let Some(record) = listed.get(hash) {
                        prop_assert!(
                            record.downloaded_bytes == 0 || record.seed_time >= hnr_seed_time,
                            "PauseTorrent targets HnR-unsatisfied hash {hash:?}"
                        );
                    }
                }
            }
        }

        // QBIT-15 [safety] (queue orchestration: force-resume protects unsatisfied — §24):
        // Every `ForceResumeTorrent` emitted targets a known HnR-unsatisfied torrent
        // with `downloaded_bytes > 0 && seed_time < hnr_seed_time` and a
        // paused/stalled-upload state.
        // Total invariant.
        #[test]
        fn orchestration_force_resume_targets_only_hnr_unsatisfied_parked(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            let listed: HashMap<TorrentHash, TorrentRecord> = torrents
                .iter()
                .map(|t| (t.hash.clone(), t.clone()))
                .collect();
            let hnr_seed_time = machine.config.hnr_seed_time;

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            for action in &out.actions {
                if let QbitAction::ForceResumeTorrent { hash, .. } = action {
                    if let Some(record) = listed.get(hash) {
                        prop_assert!(
                            record.downloaded_bytes > 0 && record.seed_time < hnr_seed_time,
                            "ForceResumeTorrent targets a non-unsatisfied hash {hash:?}"
                        );
                        prop_assert!(
                            matches!(
                                record.state,
                                TorrentState::PausedUploading | TorrentState::StalledUploading
                            ),
                            "ForceResumeTorrent targets a torrent not in paused/stalled-upload \
                             state: {:?}",
                            record.state
                        );
                    }
                }
            }
        }

        // QBIT-16 [safety] (queue orchestration: limit-triggered — §24):
        // A `QueueOrchestrated` publish is emitted only when
        // `active_count >= max_active_torrents` at observation time, and only
        // when cookie is present.
        // Total invariant.
        #[test]
        fn queue_orchestrated_only_when_limit_reached(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            // Snapshot active count and limit BEFORE handle mutates state.
            let active_count_before = u32::try_from(
                machine.torrents.values().filter(|t| t.state.is_active()).count(),
            )
            .unwrap_or(u32::MAX);
            let limit_before = machine.max_active_torrents;
            let had_cookie = machine.cookie.is_some();

            // But orchestration uses the NEW torrent list — compute active count from it.
            let new_active_count = u32::try_from(
                torrents.iter().filter(|t| t.state.is_active()).count(),
            )
            .unwrap_or(u32::MAX);

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            for publish in &out.publish {
                if let QbitPublish::QueueOrchestrated { .. } = publish {
                    prop_assert!(
                        had_cookie,
                        "QueueOrchestrated published without a cookie"
                    );
                    prop_assert!(
                        new_active_count >= limit_before,
                        "QueueOrchestrated published but new_active_count={new_active_count} \
                         < limit={limit_before} (old active={active_count_before})"
                    );
                }
            }
        }

        // QBIT-17 [safety] (quota critical — §25):
        // `TorrentsListed` publishes `UnsatisfiedQuotaCritical { unsatisfied, limit }`
        // iff `limit > 0 && unsatisfied >= limit` after the map is replaced.
        // Total invariant against fully-arbitrary state.
        #[test]
        fn quota_critical_iff_limit_positive_and_count_at_or_above(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            let limit = machine.config.unsatisfied_quota_limit;
            let hnr_seed_time = machine.config.hnr_seed_time;

            // Compute the expected unsatisfied count from the NEW listing.
            let new_unsatisfied = u32::try_from(
                torrents.iter().filter(|t| {
                    t.downloaded_bytes > 0 && t.seed_time < hnr_seed_time
                }).count()
            ).unwrap_or(u32::MAX);

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            let critical_count = out.publish.iter().filter(|p| {
                matches!(p, QbitPublish::UnsatisfiedQuotaCritical { .. })
            }).count();

            let expected_critical = if limit > 0 && new_unsatisfied >= limit { 1 } else { 0 };
            prop_assert_eq!(
                critical_count,
                expected_critical,
                "QBIT-17: UnsatisfiedQuotaCritical count mismatch (limit={}, unsatisfied={})",
                limit,
                new_unsatisfied,
            );
        }

        // QBIT-18 [safety] (quota approaching — §25):
        // `TorrentsListed` publishes `UnsatisfiedQuotaApproaching { unsatisfied, limit }`
        // iff `limit > 0 && limit.saturating_sub(5) <= unsatisfied < limit`.
        // Total invariant against fully-arbitrary state.
        #[test]
        fn quota_approaching_iff_limit_positive_and_count_in_warning_range(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            let limit = machine.config.unsatisfied_quota_limit;
            let hnr_seed_time = machine.config.hnr_seed_time;

            let new_unsatisfied = u32::try_from(
                torrents.iter().filter(|t| {
                    t.downloaded_bytes > 0 && t.seed_time < hnr_seed_time
                }).count()
            ).unwrap_or(u32::MAX);

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            let approaching_count = out.publish.iter().filter(|p| {
                matches!(p, QbitPublish::UnsatisfiedQuotaApproaching { .. })
            }).count();

            let in_warning_range = limit > 0
                && new_unsatisfied >= limit.saturating_sub(5)
                && new_unsatisfied < limit;
            let expected_approaching = if in_warning_range { 1 } else { 0 };
            prop_assert_eq!(
                approaching_count,
                expected_approaching,
                "QBIT-18: UnsatisfiedQuotaApproaching count mismatch (limit={}, unsatisfied={})",
                limit,
                new_unsatisfied,
            );
        }

        // QBIT-19 [safety] (§29): positive counterpart to QBIT-17/18.
        // `TorrentsListed` publishes `UnsatisfiedQuotaOk { unsatisfied, limit }`
        // iff `limit > 0 && unsatisfied < limit.saturating_sub(5)` — i.e. the
        // count is strictly below the approaching band.  Total invariant.
        #[test]
        fn quota_ok_iff_limit_positive_and_count_below_warning_range(
            mut machine in any_qbit_machine(),
            torrents in prop::collection::vec(any_torrent_record(), 0..5),
        ) {
            let limit = machine.config.unsatisfied_quota_limit;
            let hnr_seed_time = machine.config.hnr_seed_time;

            let new_unsatisfied = u32::try_from(
                torrents.iter().filter(|t| {
                    t.downloaded_bytes > 0 && t.seed_time < hnr_seed_time
                }).count()
            ).unwrap_or(u32::MAX);

            let event = QbitEvent::TorrentsListed { torrents };
            let out = machine.handle(Instant::now(), Timed::now(event));

            let ok_count = out.publish.iter().filter(|p| {
                matches!(p, QbitPublish::UnsatisfiedQuotaOk { .. })
            }).count();

            let below_warning = limit > 0 && new_unsatisfied < limit.saturating_sub(5);
            let expected_ok = if below_warning { 1 } else { 0 };
            prop_assert_eq!(
                ok_count,
                expected_ok,
                "QBIT-19: UnsatisfiedQuotaOk count mismatch (limit={}, unsatisfied={})",
                limit,
                new_unsatisfied,
            );
        }

        // QBIT-20 [safety] (§29): positive counterpart to QBIT-12.
        // `PreferencesRead` publishes exactly one `PrivacyClean` iff none of
        // DHT/PeX/LSD is enabled, and zero otherwise.  Total invariant.
        #[test]
        fn privacy_clean_iff_all_three_off(
            mut machine in any_qbit_machine(),
            listen_port in proptest::option::of(any_vpn_port()),
            dht in any::<bool>(),
            pex in any::<bool>(),
            lsd in any::<bool>(),
            max_active_torrents in any::<u32>(),
        ) {
            let event = QbitEvent::PreferencesRead {
                listen_port,
                dht,
                pex,
                lsd,
                max_active_torrents,
            };
            let out = machine.handle(Instant::now(), Timed::now(event));

            let clean_count = out.publish.iter()
                .filter(|p| matches!(p, QbitPublish::PrivacyClean))
                .count();
            let banned_count = out.publish.iter()
                .filter(|p| matches!(p, QbitPublish::BannedPrivacySettingsObserved { .. }))
                .count();

            if dht || pex || lsd {
                prop_assert_eq!(clean_count, 0,
                    "QBIT-20: PrivacyClean must not fire when any setting is on");
                prop_assert_eq!(banned_count, 1,
                    "QBIT-12: BannedPrivacySettingsObserved must fire when any setting is on");
            } else {
                prop_assert_eq!(clean_count, 1,
                    "QBIT-20: PrivacyClean must fire when all three are off");
                prop_assert_eq!(banned_count, 0,
                    "QBIT-12: BannedPrivacySettingsObserved must not fire when all are off");
            }
        }
    }
}
